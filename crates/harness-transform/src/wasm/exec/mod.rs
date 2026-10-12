// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The executor: runs a validated module's functions on the interpreter's
//! own stack, globals and linear memory.
//!
//! Everything that can stop an execution early is a typed
//! [`ExecError`], never a panic: a helper's fuel can run out, a dynamic
//! trap can fire (division by zero, an out-of-bounds access, a bad
//! conversion), and a malformed module can only get this far by defeating
//! the decoder and the validator together — the defensive traps below
//! report that as what it is.

mod ops;

use super::{CmpOp, Export, Instr, Module, Op, Val, ValType, WasmError};
use ops::{exec_memop, exec_numeric};

/// The linear memory's page size, as the spec defines it.
const PAGE_SIZE: usize = 65_536;
/// The address space of a 32-bit memory, in pages.
const MAX_PAGES: u32 = 65_536;
/// How deep direct calls may nest. Each level is Rust recursion, and the
/// worst case (call depth times control nesting) must stay well inside an
/// ordinary thread stack — no dedicated execution thread is required to
/// make a runaway helper fail as a typed error instead of crashing.
const MAX_CALL_DEPTH: u32 = 256;
/// How deep structured control may nest at runtime — the same bound the
/// validator enforces, restated here because the executor cannot lean on
/// its caller.
const MAX_NESTING: u32 = 512;

/// Why an execution stopped early.
#[derive(Debug, Clone, PartialEq)]
pub enum ExecError {
    /// The execution ran past its fuel budget. One unit per executed
    /// instruction, so this bounds the work, not the wall clock.
    Fuel,
    /// A dynamic trap: the shapes the validator cannot rule out.
    Trap(String),
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecError::Fuel => f.write_str("fuel exhausted"),
            ExecError::Trap(m) => f.write_str(m),
        }
    }
}

pub(crate) fn trap(msg: impl Into<String>) -> ExecError {
    ExecError::Trap(msg.into())
}

/// The mutable state of one execution: globals, linear memory, the
/// operand stack and the remaining fuel. Owned outright, so an execution
/// on a dedicated thread needs nothing shared.
pub(crate) struct St {
    pub(crate) globals: Vec<Val>,
    pub(crate) memory: Vec<u8>,
    pub(crate) stack: Vec<Val>,
    pub(crate) fuel: u64,
    pub(crate) page_cap: u32,
    /// How many direct calls are open above the current one.
    pub(crate) call_depth: u32,
}

/// Where a decoded sequence left off: fall through, branch to the `n`th
/// enclosing label, or return from the function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flow {
    Normal,
    Branch(u32),
    Return,
}

/// An instantiated module: its linear memory, sized to the declaration and
/// initialized with its data segments, and its globals, initialized to
/// their constant initializers.
pub struct Instance<'m> {
    module: &'m Module,
    globals: Vec<Val>,
    memory: Vec<u8>,
    page_cap: u32,
}

/// Instantiates a validated module under a page cap. Refuses a memory
/// declaration larger than the cap and a data segment that would not fit —
/// both before any helper code runs.
pub fn instantiate(module: &Module, page_cap: u32) -> Result<Instance<'_>, WasmError> {
    if page_cap > MAX_PAGES {
        return Err(WasmError(format!(
            "page cap {page_cap} exceeds the address space"
        )));
    }
    if module.memory_min_pages > page_cap {
        return Err(WasmError(format!(
            "memory declaration ({} pages) exceeds the page cap ({page_cap})",
            module.memory_min_pages
        )));
    }
    let mut memory = vec![0u8; module.memory_min_pages as usize * PAGE_SIZE];
    for (offset, bytes) in &module.data {
        let end = u64::from(*offset) + bytes.len() as u64;
        if end > memory.len() as u64 {
            return Err(WasmError(format!(
                "data segment at offset {offset} does not fit the declared memory"
            )));
        }
        memory[*offset as usize..end as usize].copy_from_slice(bytes);
    }
    let globals = module.globals.iter().map(|g| g.init.val()).collect();
    Ok(Instance {
        module,
        globals,
        memory,
        page_cap,
    })
}

impl<'m> Instance<'m> {
    /// The module this instance was built from.
    pub fn module(&self) -> &'m Module {
        self.module
    }

    /// The linear memory, as the helper ABI reads back a helper's output.
    pub fn memory(&self) -> &[u8] {
        &self.memory
    }

    pub fn pages(&self) -> u32 {
        (self.memory.len() / PAGE_SIZE) as u32
    }

    /// Writes host bytes into the memory (the helper ABI appends the
    /// serialized input this way), bounds-checked.
    pub fn store(&mut self, offset: u32, bytes: &[u8]) -> Result<(), WasmError> {
        let end = u64::from(offset) + bytes.len() as u64;
        if end > self.memory.len() as u64 {
            return Err(WasmError("store past the end of memory".into()));
        }
        self.memory[offset as usize..end as usize].copy_from_slice(bytes);
        Ok(())
    }

    /// Appends host bytes to the end of the linear memory, growing it in
    /// whole pages up to the page cap, and returns the offset the bytes
    /// start at. This is how the helper ABI delivers the serialized input.
    pub fn append(&mut self, bytes: &[u8]) -> Result<u32, WasmError> {
        let start = self.memory.len();
        let end = start + bytes.len();
        let need_pages = end.div_ceil(PAGE_SIZE) as u32;
        if need_pages > self.page_cap {
            return Err(WasmError("input does not fit the memory budget".into()));
        }
        self.memory.resize(need_pages as usize * PAGE_SIZE, 0);
        self.memory[start..end].copy_from_slice(bytes);
        Ok(start as u32)
    }

    /// Calls an exported function. Arguments must match the export's
    /// signature exactly — the helper ABI is a contract, not a convention.
    pub fn call_export(
        &mut self,
        name: &str,
        args: &[Val],
        fuel: u64,
    ) -> Result<Vec<Val>, ExecError> {
        let fidx = match self.module.exports.get(name) {
            Some(Export::Func(i)) => *i,
            Some(_) => return Err(trap(format!("export `{name}` is not a function"))),
            None => return Err(trap(format!("no export named `{name}`"))),
        };
        let ty = self
            .module
            .func_type(fidx)
            .map_err(|e| trap(e.to_string()))?;
        if ty.params.len() != args.len() || args.iter().zip(&ty.params).any(|(v, t)| v.ty() != *t) {
            return Err(trap(format!(
                "export `{name}` expects ({}), got {} value(s) of other types",
                ty.params
                    .iter()
                    .map(|t| t.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                args.len()
            )));
        }
        let mut st = St {
            globals: std::mem::take(&mut self.globals),
            memory: std::mem::take(&mut self.memory),
            stack: Vec::new(),
            fuel,
            page_cap: self.page_cap,
            call_depth: 0,
        };
        let out = exec_call(self.module, &mut st, fidx, args.to_vec());
        self.globals = st.globals;
        self.memory = st.memory;
        out
    }
}

/// Zero value of a type, for initializing declared locals.
fn zero_val(t: ValType) -> Val {
    match t {
        ValType::I32 => Val::I32(0),
        ValType::I64 => Val::I64(0),
        ValType::F32 => Val::F32(0.0),
        ValType::F64 => Val::F64(0.0),
    }
}

fn tick(st: &mut St) -> Result<(), ExecError> {
    if st.fuel == 0 {
        return Err(ExecError::Fuel);
    }
    st.fuel -= 1;
    Ok(())
}

/// One call frame: locals fresh per call, the operand stack and fuel
/// shared with the enclosing execution.
fn exec_call(
    module: &Module,
    st: &mut St,
    func_index: u32,
    args: Vec<Val>,
) -> Result<Vec<Val>, ExecError> {
    if st.call_depth >= MAX_CALL_DEPTH {
        return Err(trap("call stack exhausted"));
    }
    let ty = module
        .func_type(func_index)
        .map_err(|e| trap(e.to_string()))?;
    let body = &module
        .bodies
        .get(func_index as usize)
        .ok_or_else(|| trap(format!("unknown function {func_index}")))?;
    let mut locals: Vec<Val> = args;
    locals.extend(body.locals.iter().map(|t| zero_val(*t)));
    let base = st.stack.len();
    st.call_depth += 1;
    // A branch reaching the function's own label is a return; validation
    // guarantees the label's values — the function's results — sit on top.
    let ran = exec_ops(module, st, &mut locals, &body.body, 0);
    st.call_depth -= 1;
    ran?;
    let n = ty.results.len();
    if st.stack.len() < base + n {
        return Err(trap("value stack underflow at return"));
    }
    let results = st.stack.split_off(st.stack.len() - n);
    st.stack.truncate(base);
    Ok(results)
}

/// A branch to a block's label carries the block's results: take `n`
/// values off the top, cut the stack back to the frame's entry height and
/// leave the values there.
fn rescue(st: &mut St, height: usize, n: usize) -> Result<(), ExecError> {
    if st.stack.len() < height + n {
        return Err(trap("value stack underflow at branch"));
    }
    let vals = st.stack.split_off(st.stack.len() - n);
    st.stack.truncate(height);
    st.stack.extend(vals);
    Ok(())
}

fn exec_ops(
    module: &Module,
    st: &mut St,
    locals: &mut Vec<Val>,
    ops: &[Op],
    depth: u32,
) -> Result<Flow, ExecError> {
    if depth > MAX_NESTING {
        return Err(trap("control nesting too deep"));
    }
    for op in ops {
        match op {
            Op::Simple(instr) => {
                tick(st)?;
                match exec_simple(module, st, locals, instr)? {
                    Flow::Normal => {}
                    flow => return Ok(flow),
                }
            }
            Op::Block { ty, then } => {
                tick(st)?;
                let height = st.stack.len();
                match exec_ops(module, st, locals, then, depth + 1)? {
                    Flow::Normal => {}
                    Flow::Branch(0) => rescue(st, height, ty.results().len())?,
                    Flow::Branch(n) => return Ok(Flow::Branch(n - 1)),
                    Flow::Return => return Ok(Flow::Return),
                }
            }
            Op::Loop { body, .. } => {
                tick(st)?;
                let height = st.stack.len();
                loop {
                    match exec_ops(module, st, locals, body, depth + 1)? {
                        Flow::Normal => break,
                        // A loop's label carries nothing (the subset has no
                        // block parameters): the stack resets to the loop's
                        // entry height and the body runs again.
                        Flow::Branch(0) => st.stack.truncate(height),
                        Flow::Branch(n) => return Ok(Flow::Branch(n - 1)),
                        Flow::Return => return Ok(Flow::Return),
                    }
                }
            }
            Op::If { ty, then, els } => {
                tick(st)?;
                let cond = pop_i32(st)?;
                let height = st.stack.len();
                let arm: &[Op] = if cond != 0 {
                    then
                } else {
                    els.as_deref().unwrap_or(&[])
                };
                match exec_ops(module, st, locals, arm, depth + 1)? {
                    Flow::Normal => {}
                    Flow::Branch(0) => rescue(st, height, ty.results().len())?,
                    Flow::Branch(n) => return Ok(Flow::Branch(n - 1)),
                    Flow::Return => return Ok(Flow::Return),
                }
            }
        }
    }
    Ok(Flow::Normal)
}

pub(crate) fn pop(st: &mut St) -> Result<Val, ExecError> {
    st.stack.pop().ok_or_else(|| trap("value stack underflow"))
}

pub(crate) fn pop_i32(st: &mut St) -> Result<i32, ExecError> {
    match pop(st)? {
        Val::I32(x) => Ok(x),
        v => Err(type_confusion("i32", v)),
    }
}

pub(crate) fn pop_i64(st: &mut St) -> Result<i64, ExecError> {
    match pop(st)? {
        Val::I64(x) => Ok(x),
        v => Err(type_confusion("i64", v)),
    }
}

pub(crate) fn pop_f32(st: &mut St) -> Result<f32, ExecError> {
    match pop(st)? {
        Val::F32(x) => Ok(x),
        v => Err(type_confusion("f32", v)),
    }
}

pub(crate) fn pop_f64(st: &mut St) -> Result<f64, ExecError> {
    match pop(st)? {
        Val::F64(x) => Ok(x),
        v => Err(type_confusion("f64", v)),
    }
}

/// The validator makes this shape impossible; reaching it means a decoded
/// module defeated validation. Reported as a trap, never a panic.
fn type_confusion(expected: &str, v: Val) -> ExecError {
    trap(format!("internal: expected {expected}, got {}", v.ty()))
}

fn p_str(p: &ValType) -> &'static str {
    match p {
        ValType::I32 => "i32",
        ValType::I64 => "i64",
        ValType::F32 => "f32",
        ValType::F64 => "f64",
    }
}

/// A typed operation dispatch fell through its arms: the decode-time type
/// tag disagrees with the operation the validator approved. Impossible for
/// a module that decoded and validated; reported as a trap, never a panic.
pub(crate) fn mismatched_tag() -> ExecError {
    trap("internal: mismatched operation tag")
}

fn set_local(locals: &mut [Val], i: u32, v: Val) -> Result<(), ExecError> {
    let slot = locals
        .get_mut(i as usize)
        .ok_or_else(|| trap(format!("unknown local {i}")))?;
    *slot = v;
    Ok(())
}

fn exec_simple(
    module: &Module,
    st: &mut St,
    locals: &mut [Val],
    instr: &Instr,
) -> Result<Flow, ExecError> {
    use Instr::*;
    match instr {
        Const(c) => st.stack.push(c.val()),
        LocalGet(i) => {
            let v = *locals
                .get(*i as usize)
                .ok_or_else(|| trap(format!("unknown local {i}")))?;
            st.stack.push(v);
        }
        LocalSet(i) => {
            let v = pop(st)?;
            set_local(locals, *i, v)?;
        }
        LocalTee(i) => {
            let v = pop(st)?;
            set_local(locals, *i, v)?;
            st.stack.push(v);
        }
        GlobalGet(i) => {
            let v = *st
                .globals
                .get(*i as usize)
                .ok_or_else(|| trap(format!("unknown global {i}")))?;
            st.stack.push(v);
        }
        GlobalSet(i) => {
            let v = pop(st)?;
            let slot = st
                .globals
                .get_mut(*i as usize)
                .ok_or_else(|| trap(format!("unknown global {i}")))?;
            *slot = v;
        }
        MemOp(op, _, offset) => exec_memop(st, *op, *offset)?,
        MemorySize => st
            .stack
            .push(Val::I32((st.memory.len() / PAGE_SIZE) as i32)),
        MemoryGrow => {
            let n = pop_i32(st)?;
            let cur = (st.memory.len() / PAGE_SIZE) as u32;
            let new = u64::from(cur) + (n as u32 as u64);
            if new > u64::from(st.page_cap) || new > u64::from(MAX_PAGES) {
                // Spec behavior: an impossible grow reports failure, it
                // does not trap; the module decides what to do with -1.
                st.stack.push(Val::I32(-1));
            } else {
                st.memory.resize(new as usize * PAGE_SIZE, 0);
                st.stack.push(Val::I32(cur as i32));
            }
        }
        Br(d) => return Ok(Flow::Branch(*d)),
        BrIf(d) => {
            let c = pop_i32(st)?;
            if c != 0 {
                return Ok(Flow::Branch(*d));
            }
        }
        BrTable(targets, default) => {
            // An i32 index selects unsigned; negative wraps past the table
            // and lands on the default, as the spec's u32 compare says.
            let i = pop_i32(st)? as u32;
            let d = if (i as usize) < targets.len() {
                targets[i as usize]
            } else {
                *default
            };
            return Ok(Flow::Branch(d));
        }
        Return => return Ok(Flow::Return),
        Call(i) => {
            let ty = module.func_type(*i).map_err(|e| trap(e.to_string()))?;
            let mut args = Vec::with_capacity(ty.params.len());
            for p in ty.params.iter().rev() {
                let v = match pop(st)? {
                    v if v.ty() == *p => v,
                    v => return Err(type_confusion(p_str(p), v)),
                };
                args.push(v);
            }
            args.reverse();
            let results = exec_call(module, st, *i, args)?;
            st.stack.extend(results);
        }
        Unreachable => return Err(trap("unreachable executed")),
        Nop => {}
        Drop => {
            pop(st)?;
        }
        Select => {
            let c = pop_i32(st)?;
            let b = pop(st)?;
            let a = pop(st)?;
            st.stack.push(if c != 0 { a } else { b });
        }
        I32Bin(_) | I64Bin(_) | F32Bin(_) | F64Bin(_) | I32Cmp(_) | I64Cmp(_) | F32Cmp(_)
        | F64Cmp(_) | I32Un(_) | I64Un(_) | F32Un(_) | F64Un(_) | I32Eqz | I64Eqz | Conv(_) => {
            // The numeric dispatch is off the recursion path (it calls
            // nothing); keeping it out of this frame keeps every call and
            // block level small.
            exec_numeric(st, instr)?;
        }
    }
    Ok(Flow::Normal)
}

#[cfg(test)]
mod tests;
