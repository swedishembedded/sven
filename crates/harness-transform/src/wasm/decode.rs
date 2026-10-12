// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The binary decoder: sections in, a validated [`Module`] out.
//!
//! Refusals are load-bearing: a module that declares imports, tables, a
//! start section or a second memory is not "unsupported" — it is refused,
//! because each of those is a way a helper could reach outside its own
//! bytes.

use super::{
    err, BinOp, BlockType, CmpOp, Const, ConvOp, Export, FuncBody, FuncType, Global, GlobalKind,
    Instr, MemOp, Module, Op, Result, UnOp, ValType, WasmError,
};

const MAGIC: [u8; 4] = [0x00, 0x61, 0x73, 0x6D]; // \0wasm
const VERSION: [u8; 4] = [0x01, 0x00, 0x00, 0x00];

/// How many functions and bodies the subset accepts — a helper is small by
/// construction; the caps bound pathological modules before validation.
const MAX_FUNCS: u32 = 10_000;
const MAX_TYPES: u32 = 10_000;
const MAX_GLOBALS: u32 = 10_000;
const MAX_DATA_SEGMENTS: u32 = 10_000;
const MAX_DATA_BYTES: u64 = 16 * 1024 * 1024;

// ── byte reader ────────────────────────────────────────────────────────

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn byte(&mut self) -> Result<u8> {
        let b = *self
            .bytes
            .get(self.pos)
            .ok_or_else(|| WasmError("unexpected end of module".into()))?;
        self.pos += 1;
        Ok(b)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.bytes.len() {
            return err("unexpected end of module");
        }
        let out = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    /// A `u32` LEB128, rejecting the non-canonical forms (>5 bytes).
    fn u32(&mut self) -> Result<u32> {
        let mut out: u32 = 0;
        for i in 0..5 {
            let b = self.byte()?;
            if i == 4 && b & 0x80 != 0 {
                return err("integer representation too long");
            }
            out |= u32::from(b & 0x7F) << (i * 7);
            if b & 0x80 == 0 {
                return Ok(out);
            }
        }
        err("integer representation too long")
    }

    /// An `s32` LEB128 sign-extended into `i32`.
    fn s32(&mut self) -> Result<i32> {
        let mut out: i32 = 0;
        let mut shift = 0u32;
        for i in 0..5 {
            let b = self.byte()?;
            if i == 4 && b & 0x80 != 0 {
                return err("integer representation too long");
            }
            out |= i32::from(b & 0x7F) << shift;
            shift += 7;
            if b & 0x80 == 0 {
                // Sign-extend from the bits actually written.
                if b & 0x40 != 0 && shift < 32 {
                    out |= -1i32 << shift;
                }
                return Ok(out);
            }
        }
        err("integer representation too long")
    }

    /// An `s64` LEB128, rejecting the non-canonical forms (>10 bytes).
    fn s64(&mut self) -> Result<i64> {
        let mut out: i64 = 0;
        let mut shift = 0u32;
        for i in 0..10 {
            let b = self.byte()?;
            if i == 9 && b & 0x80 != 0 {
                return err("integer representation too long");
            }
            let masked = i64::from(b & 0x7F);
            if shift < 64 {
                out |= masked << shift;
            } else if (b & 0x40 != 0 && masked != 0x7F) || (shift >= 64 && masked != 0) {
                return err("integer too large");
            }
            shift += 7;
            if b & 0x80 == 0 {
                if b & 0x40 != 0 && shift < 64 {
                    out |= -1i64 << shift;
                }
                return Ok(out);
            }
        }
        err("integer representation too long")
    }

    /// An `f32`/`f64` little-endian constant.
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn name(&mut self) -> Result<String> {
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| WasmError("malformed UTF-8 name".into()))
    }

    /// The section's declared body was fully consumed.
    fn done(&self) -> Result<()> {
        if self.pos != self.bytes.len() {
            return err("section size mismatch");
        }
        Ok(())
    }
}

// ── section decode ─────────────────────────────────────────────────────

/// Decodes a module, refusing everything the helper sandbox does not
/// offer. The returned module still needs validation
/// ([`super::validate::validate_module`]) before execution.
pub fn decode(bytes: &[u8]) -> Result<Module> {
    let mut r = Reader::new(bytes);
    if r.take(4)? != MAGIC {
        return err("not a WebAssembly module (bad magic)");
    }
    if r.take(4)? != VERSION {
        return err("unsupported WebAssembly version");
    }

    let mut module = Module::default();
    let mut seen_memories = 0usize;
    let mut seen_code = false;
    let mut seen_function = false;
    let mut last_order = 0u8;

    while r.pos < r.bytes.len() {
        let id = r.byte()?;
        let size = r.u32()? as usize;
        let body = if id == 0 {
            // Custom sections: skip without decoding their content.
            r.take(size)?
        } else {
            if id > 12 {
                return err(format!("malformed section id {id}"));
            }
            if r.pos + size > r.bytes.len() {
                return err("section extends past the module");
            }
            r.take(size)?
        };
        match id {
            0 => continue,
            2 => return err(
                "imports are not part of the helper subset: a helper cannot declare host functions",
            ),
            4 => return err("tables are not part of the helper subset: no indirect calls"),
            8 => return err("a start section is not part of the helper subset"),
            9 => return err("element sections are not part of the helper subset"),
            12 => return err("a data count section is not part of the helper subset"),
            _ => {}
        }
        // Non-custom sections must appear in order, once.
        let order = match id {
            1 => 1,
            3 => 3,
            5 => 5,
            6 => 6,
            7 => 7,
            10 => 10,
            11 => 11,
            _ => unreachable!("refused ids handled above"),
        };
        if order <= last_order {
            return err(format!(
                "unexpected section id {id} (out of order or duplicate)"
            ));
        }
        last_order = order;
        let mut s = Reader::new(body);
        match id {
            1 => {
                let count = s.u32()?;
                if count > MAX_TYPES {
                    return err("too many types");
                }
                for _ in 0..count {
                    if s.byte()? != 0x60 {
                        return err("malformed function type");
                    }
                    let params = read_val_types(&mut s)?;
                    let results = read_val_types(&mut s)?;
                    if results.len() > 1 {
                        return err("multi-value function types are not part of the helper subset");
                    }
                    module.types.push(FuncType { params, results });
                }
                s.done()?;
            }
            3 => {
                seen_function = true;
                let count = s.u32()?;
                if count > MAX_FUNCS {
                    return err("too many functions");
                }
                for _ in 0..count {
                    let ty = s.u32()?;
                    if ty as usize >= module.types.len() {
                        return err("unknown type index");
                    }
                    module.funcs.push(ty);
                }
                s.done()?;
            }
            5 => {
                seen_memories += 1;
                if seen_memories > 1 {
                    return err("multiple memories are not part of the helper subset");
                }
                let flags = s.u32()?;
                if flags > 1 {
                    return err("malformed memory limits");
                }
                let min = s.u32()?;
                if flags == 1 {
                    let _max = s.u32()?; // declared max; the adapter caps anyway
                }
                if min > 65_536 {
                    return err("memory minimum exceeds the address space");
                }
                module.memory_min_pages = min;
                s.done()?;
            }
            6 => {
                let count = s.u32()?;
                if count > MAX_GLOBALS {
                    return err("too many globals");
                }
                for _ in 0..count {
                    let kind = match s.byte()? {
                        0x00 => GlobalKind::Immutable,
                        0x01 => GlobalKind::Mutable,
                        _ => return err("malformed global kind"),
                    };
                    let ty = ValType::from_byte(s.byte()?)
                        .ok_or_else(|| WasmError("malformed global type".into()))?;
                    let init = read_const(&mut s)?;
                    if s.byte()? != 0x0B {
                        return err("expected end of global initializer");
                    }
                    module.globals.push(Global { kind, ty, init });
                }
                s.done()?;
            }
            7 => {
                let count = s.u32()?;
                for _ in 0..count {
                    let name = s.name()?;
                    let export = match s.byte()? {
                        0x00 => Export::Func(s.u32()?),
                        0x02 => {
                            if s.byte()? != 0x00 {
                                return err("malformed memory export");
                            }
                            Export::Memory
                        }
                        0x03 => Export::Global(s.u32()?),
                        _ => return err("unsupported export kind (no tables in the subset)"),
                    };
                    if module.exports.insert(name, export).is_some() {
                        return err("duplicate export name");
                    }
                }
                s.done()?;
            }
            10 => {
                seen_code = true;
                let count = s.u32()?;
                if count != module.funcs.len() as u32 {
                    return err("function and code section lengths disagree");
                }
                for _ in 0..count {
                    let body_size = s.u32()? as usize;
                    let body = s.take(body_size)?;
                    module.bodies.push(decode_body(body)?);
                }
                s.done()?;
            }
            11 => {
                let count = s.u32()?;
                if count > MAX_DATA_SEGMENTS {
                    return err("too many data segments");
                }
                for _ in 0..count {
                    let flags = s.u32()?;
                    if flags != 0 {
                        return err("only active memory-0 data segments are part of the subset");
                    }
                    let offset = match read_const(&mut s)? {
                        Const::I32(x) => x,
                        _ => return err("data offset must be i32"),
                    };
                    if s.byte()? != 0x0B {
                        return err("expected end of data offset");
                    }
                    let len = s.u32()? as usize;
                    if len as u64 > MAX_DATA_BYTES {
                        return err("data segment too large");
                    }
                    let bytes = s.take(len)?.to_vec();
                    module.data.push((offset as u32, bytes));
                }
                s.done()?;
            }
            _ => unreachable!(),
        }
    }

    if seen_code != seen_function {
        return err("function and code sections disagree");
    }
    Ok(module)
}

fn read_val_types(s: &mut Reader<'_>) -> Result<Vec<ValType>> {
    let count = s.u32()? as usize;
    let mut out = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let b = s.byte()?;
        out.push(
            ValType::from_byte(b)
                .ok_or_else(|| WasmError(format!("unknown value type 0x{b:02X}")))?,
        );
    }
    Ok(out)
}

fn read_const(s: &mut Reader<'_>) -> Result<Const> {
    Ok(match s.byte()? {
        0x41 => Const::I32(s.s32()?),
        0x42 => Const::I64(s.s64()?),
        0x43 => Const::F32(s.f32()?),
        0x44 => Const::F64(s.f64()?),
        other => return err(format!("unsupported constant opcode 0x{other:02X}")),
    })
}

// ── instruction decode ─────────────────────────────────────────────────

/// What terminated a decoded instruction sequence: the `End` of its
/// enclosing frame, or an `Else` inside an `If` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Terminator {
    End,
    Else,
}

/// Decodes one function body into a structured instruction tree.
pub fn decode_body(bytes: &[u8]) -> Result<FuncBody> {
    let mut r = Reader::new(bytes);
    let decls = r.u32()?;
    let mut locals = Vec::new();
    for _ in 0..decls {
        let count = r.u32()?;
        let ty =
            ValType::from_byte(r.byte()?).ok_or_else(|| WasmError("unknown local type".into()))?;
        if locals.len() + count as usize > 100_000 {
            return err("too many locals");
        }
        for _ in 0..count {
            locals.push(ty);
        }
    }
    let (ops, terminator) = decode_seq(&mut r)?;
    if terminator != Terminator::End {
        return err("unexpected else at function level");
    }
    if r.pos != r.bytes.len() {
        return err("unexpected content after function end");
    }
    Ok(FuncBody { locals, body: ops })
}

/// Decodes instructions until the sequence's `End` (or an `Else`, which
/// only the `If` decoder accepts), structuring blocks, loops and ifs as it
/// goes.
fn decode_seq(r: &mut Reader<'_>) -> Result<(Vec<Op>, Terminator)> {
    let mut ops = Vec::new();
    loop {
        let b = r.byte()?;
        match b {
            0x0B => return Ok((ops, Terminator::End)),
            0x05 => return Ok((ops, Terminator::Else)),
            0x02..=0x04 => {
                let ty = block_type(r)?;
                let (inner, terminator) = decode_seq(r)?;
                let op = match (b, terminator) {
                    (0x02, Terminator::End) => Op::Block { ty, then: inner },
                    (0x03, Terminator::End) => Op::Loop { ty, body: inner },
                    (0x04, Terminator::End) => Op::If {
                        ty,
                        then: inner,
                        els: None,
                    },
                    (0x04, Terminator::Else) => {
                        let (els, terminator2) = decode_seq(r)?;
                        if terminator2 != Terminator::End {
                            return err("unexpected else");
                        }
                        Op::If {
                            ty,
                            then: inner,
                            els: Some(els),
                        }
                    }
                    _ => return err("else outside an if"),
                };
                ops.push(op);
            }
            _ => ops.push(Op::Simple(decode_simple(r, b)?)),
        }
    }
}

fn block_type(r: &mut Reader<'_>) -> Result<BlockType> {
    // Only the one-byte forms are in the subset: empty or one value type.
    match r.byte()? {
        0x40 => Ok(BlockType::Empty),
        b => ValType::from_byte(b)
            .map(BlockType::Value)
            .ok_or_else(|| WasmError(format!("unsupported block type 0x{b:02X}"))),
    }
}

fn decode_simple(r: &mut Reader<'_>, b: u8) -> Result<Instr> {
    let op = match b {
        0x41 => Instr::Const(Const::I32(r.s32()?)),
        0x42 => Instr::Const(Const::I64(r.s64()?)),
        0x43 => Instr::Const(Const::F32(r.f32()?)),
        0x44 => Instr::Const(Const::F64(r.f64()?)),
        0x20 => Instr::LocalGet(r.u32()?),
        0x21 => Instr::LocalSet(r.u32()?),
        0x22 => Instr::LocalTee(r.u32()?),
        0x23 => Instr::GlobalGet(r.u32()?),
        0x24 => Instr::GlobalSet(r.u32()?),
        0x28 => mem_instr(MemOp::I32Load, r)?,
        0x29 => mem_instr(MemOp::I64Load, r)?,
        0x2A => mem_instr(MemOp::F32Load, r)?,
        0x2B => mem_instr(MemOp::F64Load, r)?,
        0x2C => mem_instr(MemOp::I32Load8S, r)?,
        0x2D => mem_instr(MemOp::I32Load8U, r)?,
        0x2E => mem_instr(MemOp::I32Load16S, r)?,
        0x2F => mem_instr(MemOp::I32Load16U, r)?,
        0x36 => mem_instr(MemOp::I32Store, r)?,
        0x37 => mem_instr(MemOp::I64Store, r)?,
        0x38 => mem_instr(MemOp::F32Store, r)?,
        0x39 => mem_instr(MemOp::F64Store, r)?,
        0x3A => mem_instr(MemOp::I32Store8, r)?,
        0x3B => mem_instr(MemOp::I32Store16, r)?,
        0x3F => {
            expect_zero(r)?;
            Instr::MemorySize
        }
        0x40 => {
            expect_zero(r)?;
            Instr::MemoryGrow
        }
        0x0C => Instr::Br(r.u32()?),
        0x0D => Instr::BrIf(r.u32()?),
        0x0E => {
            let count = r.u32()?;
            if count > 10_000 {
                return err("br_table too large");
            }
            let mut targets = Vec::with_capacity(count as usize);
            for _ in 0..count {
                targets.push(r.u32()?);
            }
            let default = r.u32()?;
            Instr::BrTable(targets.into_boxed_slice(), default)
        }
        0x0F => Instr::Return,
        0x10 => Instr::Call(r.u32()?),
        0x00 => Instr::Unreachable,
        0x01 => Instr::Nop,
        0x1A => Instr::Drop,
        0x1B => Instr::Select,
        _ => decode_numeric(b)?,
    };
    Ok(op)
}

fn expect_zero(r: &mut Reader<'_>) -> Result<()> {
    if r.byte()? != 0x00 {
        return err("zero byte expected");
    }
    Ok(())
}

fn memop(r: &mut Reader<'_>) -> Result<(u32, u32)> {
    let align = r.u32()?;
    let offset = r.u32()?;
    Ok((align, offset))
}

fn mem_instr(op: MemOp, r: &mut Reader<'_>) -> Result<Instr> {
    let (align, offset) = memop(r)?;
    Ok(Instr::MemOp(op, align, offset))
}

/// The numeric opcode table, already resolved to typed operations.
fn decode_numeric(b: u8) -> Result<Instr> {
    use BinOp::*;
    use CmpOp::*;
    use ConvOp as C;
    use Instr::*;
    use UnOp as U;
    let op = match b {
        // i32 comparisons
        0x45 => I32Eqz,
        0x46 => I32Cmp(Eq),
        0x47 => I32Cmp(Ne),
        0x48 => I32Cmp(LtS),
        0x49 => I32Cmp(LtU),
        0x4A => I32Cmp(GtS),
        0x4B => I32Cmp(GtU),
        0x4C => I32Cmp(LeS),
        0x4D => I32Cmp(LeU),
        0x4E => I32Cmp(GeS),
        0x4F => I32Cmp(GeU),
        // i64 comparisons
        0x50 => I64Eqz,
        0x51 => I64Cmp(Eq),
        0x52 => I64Cmp(Ne),
        0x53 => I64Cmp(LtS),
        0x54 => I64Cmp(LtU),
        0x55 => I64Cmp(GtS),
        0x56 => I64Cmp(GtU),
        0x57 => I64Cmp(LeS),
        0x58 => I64Cmp(LeU),
        0x59 => I64Cmp(GeS),
        0x5A => I64Cmp(GeU),
        // f32 comparisons
        0x5B => F32Cmp(Eq),
        0x5C => F32Cmp(Ne),
        0x5D => F32Cmp(FLt),
        0x5E => F32Cmp(FGt),
        0x5F => F32Cmp(FLe),
        0x60 => F32Cmp(FGe),
        // f64 comparisons
        0x61 => F64Cmp(Eq),
        0x62 => F64Cmp(Ne),
        0x63 => F64Cmp(FLt),
        0x64 => F64Cmp(FGt),
        0x65 => F64Cmp(FLe),
        0x66 => F64Cmp(FGe),
        // i32 arithmetic
        0x67 => I32Un(U::Clz),
        0x68 => I32Un(U::Ctz),
        0x69 => I32Un(U::Popcnt),
        0x6A => I32Bin(Add),
        0x6B => I32Bin(Sub),
        0x6C => I32Bin(Mul),
        0x6D => I32Bin(DivS),
        0x6E => I32Bin(DivU),
        0x6F => I32Bin(RemS),
        0x70 => I32Bin(RemU),
        0x71 => I32Bin(And),
        0x72 => I32Bin(Or),
        0x73 => I32Bin(Xor),
        0x74 => I32Bin(Shl),
        0x75 => I32Bin(ShrS),
        0x76 => I32Bin(ShrU),
        0x77 => I32Bin(Rotl),
        0x78 => I32Bin(Rotr),
        // i64 arithmetic
        0x79 => I64Un(U::Clz),
        0x7A => I64Un(U::Ctz),
        0x7B => I64Un(U::Popcnt),
        0x7C => I64Bin(Add),
        0x7D => I64Bin(Sub),
        0x7E => I64Bin(Mul),
        0x7F => I64Bin(DivS),
        0x80 => I64Bin(DivU),
        0x81 => I64Bin(RemS),
        0x82 => I64Bin(RemU),
        0x83 => I64Bin(And),
        0x84 => I64Bin(Or),
        0x85 => I64Bin(Xor),
        0x86 => I64Bin(Shl),
        0x87 => I64Bin(ShrS),
        0x88 => I64Bin(ShrU),
        0x89 => I64Bin(Rotl),
        0x8A => I64Bin(Rotr),
        // f32 arithmetic
        0x8B => F32Un(U::FAbs),
        0x8C => F32Un(U::FNeg),
        0x8D => F32Un(U::FCeil),
        0x8E => F32Un(U::FFloor),
        0x8F => F32Un(U::FTrunc),
        0x90 => F32Un(U::FNearest),
        0x91 => F32Un(U::FSqrt),
        0x92 => F32Bin(FAdd),
        0x93 => F32Bin(FSub),
        0x94 => F32Bin(FMul),
        0x95 => F32Bin(FDiv),
        0x96 => F32Bin(FMin),
        0x97 => F32Bin(FMax),
        0x98 => F32Bin(FCopysign),
        // f64 arithmetic
        0x99 => F64Un(U::FAbs),
        0x9A => F64Un(U::FNeg),
        0x9B => F64Un(U::FCeil),
        0x9C => F64Un(U::FFloor),
        0x9D => F64Un(U::FTrunc),
        0x9E => F64Un(U::FNearest),
        0x9F => F64Un(U::FSqrt),
        0xA0 => F64Bin(FAdd),
        0xA1 => F64Bin(FSub),
        0xA2 => F64Bin(FMul),
        0xA3 => F64Bin(FDiv),
        0xA4 => F64Bin(FMin),
        0xA5 => F64Bin(FMax),
        0xA6 => F64Bin(FCopysign),
        // conversions
        0xA7 => Conv(C::I32WrapI64),
        0xA8 => Conv(C::I32TruncF32S),
        0xA9 => Conv(C::I32TruncF32U),
        0xAA => Conv(C::I32TruncF64S),
        0xAB => Conv(C::I32TruncF64U),
        0xAC => Conv(C::I64ExtendI32S),
        0xAD => Conv(C::I64ExtendI32U),
        0xAE => Conv(C::I64TruncF32S),
        0xAF => Conv(C::I64TruncF32U),
        0xB0 => Conv(C::I64TruncF64S),
        0xB1 => Conv(C::I64TruncF64U),
        0xB2 => Conv(C::F32ConvertI32S),
        0xB3 => Conv(C::F32ConvertI32U),
        0xB4 => Conv(C::F32ConvertI64S),
        0xB5 => Conv(C::F32ConvertI64U),
        0xB6 => Conv(C::F32DemoteF64),
        0xB7 => Conv(C::F64ConvertI32S),
        0xB8 => Conv(C::F64ConvertI32U),
        0xB9 => Conv(C::F64ConvertI64S),
        0xBA => Conv(C::F64ConvertI64U),
        0xBB => Conv(C::F64PromoteF32),
        0xBC => Conv(C::I32ReinterpretF32),
        0xBD => Conv(C::I64ReinterpretF64),
        0xBE => Conv(C::F32ReinterpretI32),
        0xBF => Conv(C::F64ReinterpretI64),
        other => return err(format!("unsupported opcode 0x{other:02X}")),
    };
    Ok(op)
}
