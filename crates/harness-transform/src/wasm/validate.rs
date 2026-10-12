// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The module validator: the same typing discipline the WebAssembly core
//! spec prescribes, over the subset's instruction set. A module that does
//! not validate is refused before a single instruction executes — a
//! `transform` step's failures are the helper's declared contract, never a
//! half-decoded guess.

use super::{err, ConvOp, GlobalKind, Instr, Module, Op, Result, ValType, WasmError};

/// A function validated in its module context: types, globals and the
/// function index space.
struct Ctx<'a> {
    module: &'a Module,
}

/// One open control frame: what a branch to it carries, what it ends
/// with, the value-stack height it was entered at, and whether its arm
/// has already made the rest of the sequence unreachable.
struct Ctrl {
    /// Types a branch to this label carries: a block's or if's results, a
    /// loop's (empty — the subset has no block parameters).
    label_types: Vec<ValType>,
    /// Types the frame leaves when its arm completes.
    end_types: Vec<ValType>,
    /// Value-stack height at entry.
    height: usize,
    /// The arm hit `unreachable`, `br` or `return`: everything after is
    /// dead code, and stack reads against this frame accept anything.
    unreachable: bool,
}

/// The abstract value stack: a known type, or a polymorphic placeholder
/// (after `unreachable`).
type AbstractVal = Option<ValType>;

struct FnValidator<'a> {
    ctx: &'a Ctx<'a>,
    /// Locals: the function's parameters followed by its declared locals.
    locals: Vec<ValType>,
    results: Vec<ValType>,
    vals: Vec<AbstractVal>,
    ctrls: Vec<Ctrl>,
}

impl<'a> FnValidator<'a> {
    fn new(
        ctx: &'a Ctx<'a>,
        params: Vec<ValType>,
        locals: Vec<ValType>,
        results: Vec<ValType>,
    ) -> Self {
        let mut all = params;
        all.extend(locals);
        Self {
            ctx,
            locals: all,
            results,
            vals: Vec::new(),
            ctrls: Vec::new(),
        }
    }

    fn push(&mut self, ty: ValType) -> Result<()> {
        self.vals.push(Some(ty));
        Ok(())
    }

    /// Pops a value of `expect`. Against an unreachable frame the stack
    /// bottom is polymorphic: the pop accepts anything and consumes
    /// nothing beyond the frame's entry height.
    fn pop(&mut self, expect: ValType) -> Result<()> {
        let frame = self
            .ctrls
            .last()
            .ok_or_else(|| WasmError("internal: control frame missing".into()))?;
        if self.vals.len() == frame.height {
            if frame.unreachable {
                return Ok(());
            }
            return err("type mismatch: stack underflow");
        }
        let got = self.vals.pop().flatten();
        match got {
            None => Ok(()), // polymorphic value accepts any demand
            Some(ty) => {
                if ty != expect {
                    return err(format!("type mismatch: expected {expect}, got {ty}"));
                }
                Ok(())
            }
        }
    }

    fn unreachable_mark(&mut self) -> Result<()> {
        let frame = self
            .ctrls
            .last_mut()
            .ok_or_else(|| WasmError("internal: control frame missing".into()))?;
        self.vals.truncate(frame.height);
        frame.unreachable = true;
        Ok(())
    }

    /// Enters a structured frame. The frame starts empty — the subset has
    /// no block parameters — and its arm produces the end types itself.
    fn push_ctrl(&mut self, label_types: Vec<ValType>, end_types: Vec<ValType>) -> Result<()> {
        self.ctrls.push(Ctrl {
            label_types,
            height: self.vals.len(),
            unreachable: false,
            end_types,
        });
        Ok(())
    }

    /// The `else` boundary of an `if`: demands the then-arm's results,
    /// resets the stack to the frame's height and clears the frame's
    /// unreachable flag, so the else-arm validates fresh against the same
    /// frame.
    fn reset_ctrl(&mut self) -> Result<()> {
        let (end_types, height) = {
            let frame = self
                .ctrls
                .last()
                .ok_or_else(|| WasmError("internal: control frame missing".into()))?;
            (frame.end_types.clone(), frame.height)
        };
        for ty in end_types.iter().rev() {
            self.pop(*ty)?;
        }
        if self.vals.len() != height {
            return err("type mismatch: values remain at the end of a block");
        }
        self.vals.truncate(height);
        if let Some(frame) = self.ctrls.last_mut() {
            frame.unreachable = false;
        }
        Ok(())
    }

    /// Leaves the innermost frame: demands its results (against the frame
    /// itself — its height and reachability state govern the demand),
    /// checks nothing was left over, then restores the entry height and
    /// leaves the results for the enclosing sequence.
    fn pop_ctrl(&mut self) -> Result<()> {
        let (end_types, height) = {
            let frame = self
                .ctrls
                .last()
                .ok_or_else(|| WasmError("internal: control frame missing".into()))?;
            (frame.end_types.clone(), frame.height)
        };
        for ty in end_types.iter().rev() {
            self.pop(*ty)?;
        }
        if self.vals.len() != height {
            return err("type mismatch: values remain at the end of a block");
        }
        self.ctrls.pop();
        for ty in end_types {
            self.push(ty)?;
        }
        Ok(())
    }

    /// Pops the types a branch to the `n`th enclosing label carries.
    fn pop_label_types(&mut self, label: u32) -> Result<Vec<ValType>> {
        let types = label_types_for(self, label)?;
        for ty in types.iter().rev() {
            self.pop(*ty)?;
        }
        Ok(types)
    }
}

/// Validates every function body against the module's declarations. Decode
/// already ran; this is the type-checking pass.
pub fn validate_module(module: &Module) -> Result<()> {
    let ctx = Ctx { module };
    for (index, body) in module.bodies.iter().enumerate() {
        let ty = module.func_type(index as u32)?;
        let mut v = FnValidator::new(
            &ctx,
            ty.params.clone(),
            body.locals.clone(),
            ty.results.clone(),
        );
        // The function's outermost label carries its results — a branch to
        // it is a return.
        v.push_ctrl(ty.results.clone(), ty.results.clone())?;
        check_ops(&mut v, &body.body, 0)?;
        v.pop_ctrl()?;
        // pop_ctrl left exactly the re-pushed results on the stack.
        if !v.ctrls.is_empty()
            || v.vals.len() != ty.results.len()
            || v.vals
                .iter()
                .zip(&ty.results)
                .any(|(got, want)| *got != Some(*want))
        {
            return err("type mismatch: function end");
        }
    }
    Ok(())
}

fn check_ops(v: &mut FnValidator<'_>, ops: &[Op], depth: u32) -> Result<()> {
    if depth > 1_000 {
        return err("control nesting too deep");
    }
    for op in ops {
        check_op(v, op, depth)?;
    }
    Ok(())
}

fn check_op(v: &mut FnValidator<'_>, op: &Op, depth: u32) -> Result<()> {
    match op {
        Op::Block { ty, then } => {
            v.push_ctrl(ty.results(), ty.results())?;
            check_ops(v, then, depth + 1)?;
            v.pop_ctrl()
        }
        Op::Loop { ty, body } => {
            // A loop's label carries nothing: a branch to it restarts the
            // body with an empty stack contribution.
            v.push_ctrl(Vec::new(), ty.results())?;
            check_ops(v, body, depth + 1)?;
            v.pop_ctrl()
        }
        Op::If { ty, then, els } => {
            v.pop(ValType::I32)?;
            v.push_ctrl(ty.results(), ty.results())?;
            check_ops(v, then, depth + 1)?;
            match els {
                Some(els) => {
                    v.reset_ctrl()?;
                    check_ops(v, els, depth + 1)?;
                    v.pop_ctrl()?;
                }
                None => {
                    if !ty.results().is_empty() {
                        return err("if without else must not produce a value");
                    }
                    v.pop_ctrl()?;
                }
            }
            Ok(())
        }
        Op::Simple(instr) => check_simple(v, instr),
    }
}

fn check_simple(v: &mut FnValidator<'_>, instr: &Instr) -> Result<()> {
    use Instr::*;
    match instr {
        Const(c) => {
            let ty = match c {
                super::Const::I32(_) => ValType::I32,
                super::Const::I64(_) => ValType::I64,
                super::Const::F32(_) => ValType::F32,
                super::Const::F64(_) => ValType::F64,
            };
            v.push(ty)
        }
        LocalGet(i) => {
            let ty = local(v, *i)?;
            v.push(ty)
        }
        LocalSet(i) => {
            let ty = local(v, *i)?;
            v.pop(ty)
        }
        LocalTee(i) => {
            let ty = local(v, *i)?;
            v.pop(ty)?;
            v.push(ty)
        }
        GlobalGet(i) => {
            let g = v
                .ctx
                .module
                .globals
                .get(*i as usize)
                .ok_or_else(|| WasmError(format!("unknown global {i}")))?;
            v.push(g.ty)
        }
        GlobalSet(i) => {
            let g = v
                .ctx
                .module
                .globals
                .get(*i as usize)
                .ok_or_else(|| WasmError(format!("unknown global {i}")))?;
            if g.kind != GlobalKind::Mutable {
                return err("global is immutable");
            }
            v.pop(g.ty)
        }
        MemOp(op, ..) => {
            let (ty, ..) = op.shape();
            match op {
                super::MemOp::I32Load
                | super::MemOp::I64Load
                | super::MemOp::F32Load
                | super::MemOp::F64Load
                | super::MemOp::I32Load8S
                | super::MemOp::I32Load8U
                | super::MemOp::I32Load16S
                | super::MemOp::I32Load16U => {
                    v.pop(ValType::I32)?;
                    v.push(ty)
                }
                _ => {
                    v.pop(ty)?;
                    v.pop(ValType::I32)
                }
            }
        }
        MemorySize => v.push(ValType::I32),
        MemoryGrow => {
            v.pop(ValType::I32)?;
            v.push(ValType::I32)
        }
        Br(label) => {
            v.pop_label_types(*label)?;
            v.unreachable_mark()
        }
        BrIf(label) => {
            // The condition sits on top; the branch's values sit below it.
            // Whether or not the branch fires, the fall-through continues
            // with them — pop and put them back.
            v.pop(ValType::I32)?;
            let types = v.pop_label_types(*label)?;
            for ty in types {
                v.push(ty)?;
            }
            Ok(())
        }
        BrTable(targets, default) => {
            let default_types = label_types_for(v, *default)?;
            for target in targets.iter() {
                if label_types_for(v, *target)? != default_types {
                    return err("br_table targets disagree on their label types");
                }
            }
            v.pop(ValType::I32)?;
            for ty in default_types.iter().rev() {
                v.pop(*ty)?;
            }
            v.unreachable_mark()
        }
        Return => {
            for ty in v.results.clone() {
                v.pop(ty)?;
            }
            v.unreachable_mark()
        }
        Call(i) => {
            let ty = v.ctx.module.func_type(*i)?;
            for p in ty.params.iter().rev() {
                v.pop(*p)?;
            }
            for r in &ty.results {
                v.push(*r)?;
            }
            Ok(())
        }
        Unreachable => v.unreachable_mark(),
        Nop => Ok(()),
        Drop => {
            if v.vals.len() == v.ctrls.last().map(|c| c.height).unwrap_or(0)
                && !v.ctrls.last().map(|c| c.unreachable).unwrap_or(false)
            {
                return err("type mismatch: stack underflow");
            }
            v.vals.pop();
            Ok(())
        }
        Select => {
            v.pop(ValType::I32)?;
            let a = pop_val(v)?;
            let b = pop_val(v)?;
            match (a, b) {
                (Some(t1), Some(t2)) => {
                    if t1 != t2 {
                        return err("select operand types differ");
                    }
                    v.push(t1)
                }
                (Some(t), None) | (None, Some(t)) => v.push(t),
                (None, None) => v.push(ValType::I32), // unconstrained; i32 is harmless
            }
        }
        I32Bin(_) => binop_int(v, ValType::I32),
        I64Bin(_) => binop_int(v, ValType::I64),
        F32Bin(_) => binop_float(v, ValType::F32),
        F64Bin(_) => binop_float(v, ValType::F64),
        I32Cmp(_) => cmpop_int(v, ValType::I32),
        I64Cmp(_) => cmpop_int(v, ValType::I64),
        F32Cmp(_) => cmpop_float(v, ValType::F32),
        F64Cmp(_) => cmpop_float(v, ValType::F64),
        I32Un(_) => unop_int(v, ValType::I32),
        I64Un(_) => unop_int(v, ValType::I64),
        F32Un(_) => unop_float(v, ValType::F32),
        F64Un(_) => unop_float(v, ValType::F64),
        I32Eqz => {
            v.pop(ValType::I32)?;
            v.push(ValType::I32)
        }
        I64Eqz => {
            v.pop(ValType::I64)?;
            v.push(ValType::I32)
        }
        Conv(op) => {
            let (from, to) = conv_shape(*op);
            v.pop(from)?;
            v.push(to)
        }
    }
}

fn local(v: &FnValidator<'_>, i: u32) -> Result<ValType> {
    v.locals
        .get(i as usize)
        .copied()
        .ok_or_else(|| WasmError(format!("unknown local {i}")))
}

fn label_types_for(v: &FnValidator<'_>, label: u32) -> Result<Vec<ValType>> {
    let len = v.ctrls.len();
    v.ctrls
        .get(
            len.checked_sub(1 + label as usize)
                .ok_or_else(|| WasmError(format!("branch depth {label} out of range")))?,
        )
        .map(|c| c.label_types.clone())
        .ok_or_else(|| WasmError(format!("branch depth {label} out of range")))
}

/// Pops any one value, honoring polymorphism.
fn pop_val(v: &mut FnValidator<'_>) -> Result<AbstractVal> {
    let frame = v
        .ctrls
        .last()
        .ok_or_else(|| WasmError("internal: control frame missing".into()))?;
    if v.vals.len() == frame.height {
        if frame.unreachable {
            return Ok(None);
        }
        return err("type mismatch: stack underflow");
    }
    Ok(v.vals.pop().flatten())
}

fn binop_int(v: &mut FnValidator<'_>, ty: ValType) -> Result<()> {
    v.pop(ty)?;
    v.pop(ty)?;
    v.push(ty)
}

fn binop_float(v: &mut FnValidator<'_>, ty: ValType) -> Result<()> {
    v.pop(ty)?;
    v.pop(ty)?;
    v.push(ty)
}

fn cmpop_int(v: &mut FnValidator<'_>, ty: ValType) -> Result<()> {
    v.pop(ty)?;
    v.pop(ty)?;
    v.push(ValType::I32)
}

fn cmpop_float(v: &mut FnValidator<'_>, ty: ValType) -> Result<()> {
    v.pop(ty)?;
    v.pop(ty)?;
    v.push(ValType::I32)
}

fn unop_int(v: &mut FnValidator<'_>, ty: ValType) -> Result<()> {
    v.pop(ty)?;
    v.push(ty)
}

fn unop_float(v: &mut FnValidator<'_>, ty: ValType) -> Result<()> {
    v.pop(ty)?;
    v.push(ty)
}

/// (from, to) per conversion.
fn conv_shape(op: ConvOp) -> (ValType, ValType) {
    use ConvOp::*;
    use ValType::*;
    match op {
        I32WrapI64 => (I64, I32),
        I32TruncF32S | I32TruncF32U => (F32, I32),
        I32TruncF64S | I32TruncF64U => (F64, I32),
        I64ExtendI32S | I64ExtendI32U => (I32, I64),
        I64TruncF32S | I64TruncF32U => (F32, I64),
        I64TruncF64S | I64TruncF64U => (F64, I64),
        F32ConvertI32S | F32ConvertI32U => (I32, F32),
        F32ConvertI64S | F32ConvertI64U => (I64, F32),
        F32DemoteF64 => (F64, F32),
        F64ConvertI32S | F64ConvertI32U => (I32, F64),
        F64ConvertI64S | F64ConvertI64U => (I64, F64),
        F64PromoteF32 => (F32, F64),
        I32ReinterpretF32 => (F32, I32),
        I64ReinterpretF64 => (F64, I64),
        F32ReinterpretI32 => (I32, F32),
        F64ReinterpretI64 => (I64, F64),
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;

    // ── accepted shapes ────────────────────────────────────────────────

    #[test]
    fn accepts_a_minimal_function() {
        let ops = [];
        assert!(check(&module_with_run(&[], &[], &[], &ops)).is_ok());
    }

    #[test]
    fn accepts_arithmetic_shapes() {
        // (param i32 i32) (result i32) → i32.add
        let ops = [LOCAL_GET, 0, LOCAL_GET, 1, I32_ADD];
        assert!(check(&module_with_run(
            &[super::super::ValType::I32; 2],
            &[super::super::ValType::I32],
            &[],
            &ops
        ))
        .is_ok());
    }

    #[test]
    fn accepts_a_branch_to_a_block_label_carrying_the_block_result() {
        // (func (result i32) (block (result i32) (i32.const 7) (br 0)))
        let mut ops = vec![BLOCK, 0x7F];
        ops.extend(i32_const(7));
        ops.extend(br(0));
        ops.push(END);
        assert!(check(&module_with_run(
            &[],
            &[super::super::ValType::I32],
            &[],
            &ops
        ))
        .is_ok());
    }

    #[test]
    fn accepts_a_branch_past_a_block_to_the_function_label() {
        // (func (result i32) (block (result i32) (i32.const 1) (br 1))):
        // the branch targets the function label and carries the i32; the
        // block it exits is unreachable, so its own end is polymorphic.
        let mut ops = vec![BLOCK, 0x7F];
        ops.extend(i32_const(1));
        ops.extend(br(1));
        ops.push(END);
        assert!(check(&module_with_run(
            &[],
            &[super::super::ValType::I32],
            &[],
            &ops
        ))
        .is_ok());
    }

    #[test]
    fn accepts_br_table_when_targets_agree() {
        // (func (result i32) (block (result i32) (i32.const 1) (i32.const 0) (br_table 0 0)))
        let mut ops = vec![BLOCK, 0x7F];
        ops.extend(i32_const(1));
        ops.extend(i32_const(0));
        ops.extend(vec![0x0E]);
        ops.extend(leb32(1)); // one target
        ops.extend(leb32(0)); // target
        ops.extend(leb32(0)); // default
        ops.push(END);
        assert!(check(&module_with_run(
            &[],
            &[super::super::ValType::I32],
            &[],
            &ops
        ))
        .is_ok());
    }

    // ── refusals: the branch-label discipline ──────────────────────────

    #[test]
    fn refuses_a_branch_to_a_block_label_without_the_result_value() {
        // (block (result i32) (br 0)) — the branch must carry the i32.
        let ops = [BLOCK, 0x7F, BR, 0, END];
        assert!(check(&module_with_run(
            &[],
            &[super::super::ValType::I32],
            &[],
            &ops
        ))
        .is_err());
    }

    #[test]
    fn refuses_a_wrongly_typed_branch_value() {
        // (i64.const 5) (block (result i32) (br 1)): the branch targets the
        // function label (i32) but carries an i64 — the exact shape that
        // would be type confusion at runtime if a branch's label types were
        // not checked.
        let mut ops = i64_const(5);
        ops.extend(vec![BLOCK, 0x7F, BR, 1, END]);
        assert!(check(&module_with_run(
            &[],
            &[super::super::ValType::I32],
            &[],
            &ops
        ))
        .is_err());
    }

    #[test]
    fn accepts_a_loop_body_branching_to_its_own_label() {
        // (loop (result i32) (i32.const 1) (br 0)) — a loop's label
        // carries nothing (the subset has no block parameters), so the br
        // consumes no value; the loop end is unreachable and polymorphic.
        // This runs forever, which is fuel's job to bound, not the
        // validator's.
        let mut ops = vec![LOOP, 0x7F];
        ops.extend(i32_const(1));
        ops.extend(br(0));
        ops.push(END);
        assert!(check(&module_with_run(
            &[],
            &[super::super::ValType::I32],
            &[],
            &ops
        ))
        .is_ok());
    }

    #[test]
    fn refuses_a_branch_to_the_function_label_without_the_result_value() {
        // (func (result i32) (br 0)) — the outermost label carries the
        // function's results.
        let ops = [BR, 0];
        assert!(check(&module_with_run(
            &[],
            &[super::super::ValType::I32],
            &[],
            &ops
        ))
        .is_err());
    }

    #[test]
    fn refuses_br_table_when_targets_disagree() {
        // (block (result i32) (block (br_table 1 0) ... )): target 1 (the
        // outer block, no values) disagrees with default 0... — build the
        // disagreement directly: two labels with different label types.
        // (block $a (result i32) (block $b (br_table 0 1)) (drop))
        let mut ops = vec![BLOCK, 0x7F, BLOCK, 0x40, 0x0E];
        ops.extend(leb32(1));
        ops.extend(leb32(0)); // target: inner block (no values)
        ops.extend(leb32(1)); // default: outer block (one i32)
        ops.extend(vec![END, END, DROP, END]);
        assert!(check(&module_with_run(
            &[],
            &[super::super::ValType::I32],
            &[],
            &ops
        ))
        .is_err());
    }

    // ── refusals: plain typing ─────────────────────────────────────────

    #[test]
    fn refuses_a_stack_underflow() {
        // (result i32) with nothing on the stack.
        let ops = [];
        assert!(check(&module_with_run(
            &[],
            &[super::super::ValType::I32],
            &[],
            &ops
        ))
        .is_err());
    }

    #[test]
    fn refuses_a_type_mismatch() {
        // i64.add where i32.add is needed: two i32 on the stack.
        let ops = [I32_ADD];
        assert!(check(&module_with_run(
            &[],
            &[super::super::ValType::I64],
            &[],
            &ops
        ))
        .is_err());
    }

    #[test]
    fn refuses_leftover_values_at_the_end_of_a_function() {
        let mut ops = i32_const(1);
        ops.extend(i32_const(2));
        assert!(check(&module_with_run(&[], &[], &[], &ops)).is_err());
    }

    #[test]
    fn refuses_an_if_without_else_that_produces_a_value() {
        // (if (result i32) (i32.const 1) (then (i32.const 1))) — no else.
        let mut ops = i32_const(1);
        ops.extend(vec![IF, 0x7F]);
        ops.extend(i32_const(1));
        ops.extend(vec![END, END]);
        assert!(check(&module_with_run(
            &[],
            &[super::super::ValType::I32],
            &[],
            &ops
        ))
        .is_err());
    }

    #[test]
    fn refuses_writing_an_immutable_global() {
        let b = super::super::testutil::Builder::default();
        let (b, fidx) = b.func(&[], &[]);
        let mut ops = i32_const(1);
        ops.extend(vec![GLOBAL_SET]);
        ops.extend(idx(0));
        let bytes = b
            .global(false, super::super::ValType::I32, &i32_const(0))
            .body(&[], &ops)
            .export_func("run", fidx)
            .build();
        assert!(check(&bytes).is_err());
    }
}
