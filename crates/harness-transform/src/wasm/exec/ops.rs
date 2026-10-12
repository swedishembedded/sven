// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The per-opcode semantics of the sandbox's arithmetic, comparison,
//! memory and conversion operations: pure functions over the value stack,
//! with the WebAssembly spec's trap and rounding rules. Kept apart from
//! the control-flow machinery so the recursion path stays small.

use super::{mismatched_tag, pop_f32, pop_f64, pop_i32, pop_i64, trap, CmpOp, ExecError, St};
use crate::wasm::{BinOp, ConvOp, Instr, MemOp, UnOp, Val};

/// The typed arithmetic, comparison, unary and conversion operations: one
/// value (or two) off the stack, one back on.
pub(super) fn exec_numeric(st: &mut St, instr: &Instr) -> Result<(), ExecError> {
    use Instr::*;
    match instr {
        I32Bin(op) => {
            let b = pop_i32(st)?;
            let a = pop_i32(st)?;
            st.stack.push(Val::I32(i32_bin(*op, a, b)?));
        }
        I64Bin(op) => {
            let b = pop_i64(st)?;
            let a = pop_i64(st)?;
            let r = i64_bin(*op, a, b)?;
            st.stack.push(Val::I64(r));
        }
        F32Bin(op) => {
            let b = pop_f32(st)?;
            let a = pop_f32(st)?;
            let r = match *op {
                BinOp::FAdd => a + b,
                BinOp::FSub => a - b,
                BinOp::FMul => a * b,
                BinOp::FDiv => a / b,
                BinOp::FMin => f32_min(a, b),
                BinOp::FMax => f32_max(a, b),
                BinOp::FCopysign => a.copysign(b),
                _ => return Err(mismatched_tag()),
            };
            st.stack.push(Val::F32(r));
        }
        F64Bin(op) => {
            let b = pop_f64(st)?;
            let a = pop_f64(st)?;
            let r = match *op {
                BinOp::FAdd => a + b,
                BinOp::FSub => a - b,
                BinOp::FMul => a * b,
                BinOp::FDiv => a / b,
                BinOp::FMin => f64_min(a, b),
                BinOp::FMax => f64_max(a, b),
                BinOp::FCopysign => a.copysign(b),
                _ => return Err(mismatched_tag()),
            };
            st.stack.push(Val::F64(r));
        }
        I32Cmp(op) => {
            let b = pop_i32(st)?;
            let a = pop_i32(st)?;
            st.stack.push(Val::I32(i32::from(i32_cmp(*op, a, b))));
        }
        I64Cmp(op) => {
            let b = pop_i64(st)?;
            let a = pop_i64(st)?;
            st.stack.push(Val::I32(i32::from(i64_cmp(*op, a, b))));
        }
        F32Cmp(op) => {
            let b = pop_f32(st)?;
            let a = pop_f32(st)?;
            st.stack.push(Val::I32(i32::from(f32_cmp(*op, a, b))));
        }
        F64Cmp(op) => {
            let b = pop_f64(st)?;
            let a = pop_f64(st)?;
            st.stack.push(Val::I32(i32::from(f64_cmp(*op, a, b))));
        }
        I32Un(op) => {
            let a = pop_i32(st)?;
            let r = match *op {
                UnOp::Clz => a.leading_zeros() as i32,
                UnOp::Ctz => a.trailing_zeros() as i32,
                UnOp::Popcnt => a.count_ones() as i32,
                _ => return Err(mismatched_tag()),
            };
            st.stack.push(Val::I32(r));
        }
        I64Un(op) => {
            let a = pop_i64(st)?;
            let r = match *op {
                UnOp::Clz => a.leading_zeros() as i64,
                UnOp::Ctz => a.trailing_zeros() as i64,
                UnOp::Popcnt => a.count_ones() as i64,
                _ => return Err(mismatched_tag()),
            };
            st.stack.push(Val::I64(r));
        }
        F32Un(op) => {
            let a = pop_f32(st)?;
            let r = match *op {
                UnOp::FAbs => a.abs(),
                UnOp::FNeg => -a,
                UnOp::FCeil => a.ceil(),
                UnOp::FFloor => a.floor(),
                UnOp::FTrunc => a.trunc(),
                UnOp::FNearest => a.round_ties_even(),
                UnOp::FSqrt => a.sqrt(),
                _ => return Err(mismatched_tag()),
            };
            st.stack.push(Val::F32(r));
        }
        F64Un(op) => {
            let a = pop_f64(st)?;
            let r = match *op {
                UnOp::FAbs => a.abs(),
                UnOp::FNeg => -a,
                UnOp::FCeil => a.ceil(),
                UnOp::FFloor => a.floor(),
                UnOp::FTrunc => a.trunc(),
                UnOp::FNearest => a.round_ties_even(),
                UnOp::FSqrt => a.sqrt(),
                _ => return Err(mismatched_tag()),
            };
            st.stack.push(Val::F64(r));
        }
        I32Eqz => {
            let a = pop_i32(st)?;
            st.stack.push(Val::I32(i32::from(a == 0)));
        }
        I64Eqz => {
            let a = pop_i64(st)?;
            st.stack.push(Val::I32(i32::from(a == 0)));
        }
        Conv(op) => exec_conv(st, *op)?,
        _ => return Err(mismatched_tag()),
    }
    Ok(())
}

// ── memory ─────────────────────────────────────────────────────────────

pub(super) fn exec_memop(st: &mut St, op: MemOp, offset: u32) -> Result<(), ExecError> {
    let (_, width, _) = op.shape();
    // A store's value sits above its address on the stack: pop the value
    // first, then the address the bounds check needs.
    let popped = match op {
        MemOp::I32Store | MemOp::I32Store8 | MemOp::I32Store16 => Val::I32(pop_i32(st)?),
        MemOp::I64Store => Val::I64(pop_i64(st)?),
        MemOp::F32Store => Val::F32(pop_f32(st)?),
        MemOp::F64Store => Val::F64(pop_f64(st)?),
        _ => Val::I32(0), // a load carries no value
    };
    let addr = u64::from(pop_i32(st)? as u32);
    let eff = addr + u64::from(offset);
    if eff + u64::from(width) > st.memory.len() as u64 {
        return Err(trap("out of bounds memory access"));
    }
    let at = eff as usize;
    match op {
        MemOp::I32Store => {
            let Val::I32(v) = popped else {
                return Err(mismatched_tag());
            };
            st.memory[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
        MemOp::I64Store => {
            let Val::I64(v) = popped else {
                return Err(mismatched_tag());
            };
            st.memory[at..at + 8].copy_from_slice(&v.to_le_bytes());
        }
        MemOp::F32Store => {
            let Val::F32(v) = popped else {
                return Err(mismatched_tag());
            };
            st.memory[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
        MemOp::F64Store => {
            let Val::F64(v) = popped else {
                return Err(mismatched_tag());
            };
            st.memory[at..at + 8].copy_from_slice(&v.to_le_bytes());
        }
        MemOp::I32Store8 => {
            let Val::I32(v) = popped else {
                return Err(mismatched_tag());
            };
            st.memory[at] = v as u8;
        }
        MemOp::I32Store16 => {
            let Val::I32(v) = popped else {
                return Err(mismatched_tag());
            };
            st.memory[at..at + 2].copy_from_slice(&(v as u16).to_le_bytes());
        }
        _ => {
            // A load: assemble the value from the memory bytes, then push.
            let bytes = &st.memory[at..at + width as usize];
            let v = match op {
                MemOp::I32Load => Val::I32(i32::from_le_bytes(bytes.try_into().unwrap())),
                MemOp::I64Load => Val::I64(i64::from_le_bytes(bytes.try_into().unwrap())),
                MemOp::F32Load => Val::F32(f32::from_le_bytes(bytes.try_into().unwrap())),
                MemOp::F64Load => Val::F64(f64::from_le_bytes(bytes.try_into().unwrap())),
                MemOp::I32Load8S => Val::I32(i8::from_le_bytes([bytes[0]]) as i32),
                MemOp::I32Load8U => Val::I32(i32::from(bytes[0])),
                MemOp::I32Load16S => Val::I32(i16::from_le_bytes(bytes.try_into().unwrap()) as i32),
                MemOp::I32Load16U => {
                    Val::I32(i32::from(u16::from_le_bytes(bytes.try_into().unwrap())))
                }
                _ => return Err(mismatched_tag()),
            };
            st.stack.push(v);
        }
    }
    Ok(())
}

// ── integer arithmetic ─────────────────────────────────────────────────

fn i32_bin(op: BinOp, a: i32, b: i32) -> Result<i32, ExecError> {
    use BinOp::*;
    Ok(match op {
        Add => a.wrapping_add(b),
        Sub => a.wrapping_sub(b),
        Mul => a.wrapping_mul(b),
        DivS => {
            if b == 0 {
                return Err(trap("integer divide by zero"));
            }
            if a == i32::MIN && b == -1 {
                return Err(trap("integer overflow"));
            }
            a.wrapping_div(b)
        }
        DivU => {
            if b == 0 {
                return Err(trap("integer divide by zero"));
            }
            (a as u32 / b as u32) as i32
        }
        RemS => {
            if b == 0 {
                return Err(trap("integer divide by zero"));
            }
            a.wrapping_rem(b)
        }
        RemU => {
            if b == 0 {
                return Err(trap("integer divide by zero"));
            }
            (a as u32 % b as u32) as i32
        }
        And => a & b,
        Or => a | b,
        Xor => a ^ b,
        Shl => a.wrapping_shl(b as u32),
        ShrS => a.wrapping_shr(b as u32),
        ShrU => (a as u32).wrapping_shr(b as u32) as i32,
        Rotl => a.rotate_left(b as u32),
        Rotr => a.rotate_right(b as u32),
        _ => return Err(mismatched_tag()),
    })
}

fn i64_bin(op: BinOp, a: i64, b: i64) -> Result<i64, ExecError> {
    use BinOp::*;
    Ok(match op {
        Add => a.wrapping_add(b),
        Sub => a.wrapping_sub(b),
        Mul => a.wrapping_mul(b),
        DivS => {
            if b == 0 {
                return Err(trap("integer divide by zero"));
            }
            if a == i64::MIN && b == -1 {
                return Err(trap("integer overflow"));
            }
            a.wrapping_div(b)
        }
        DivU => {
            if b == 0 {
                return Err(trap("integer divide by zero"));
            }
            (a as u64 / b as u64) as i64
        }
        RemS => {
            if b == 0 {
                return Err(trap("integer divide by zero"));
            }
            a.wrapping_rem(b)
        }
        RemU => {
            if b == 0 {
                return Err(trap("integer divide by zero"));
            }
            (a as u64 % b as u64) as i64
        }
        And => a & b,
        Or => a | b,
        Xor => a ^ b,
        Shl => a.wrapping_shl((b as u64 & 0x3F) as u32),
        ShrS => a.wrapping_shr((b as u64 & 0x3F) as u32),
        ShrU => (a as u64).wrapping_shr((b as u64 & 0x3F) as u32) as i64,
        Rotl => a.rotate_left((b as u64 & 0x3F) as u32),
        Rotr => a.rotate_right((b as u64 & 0x3F) as u32),
        _ => return Err(mismatched_tag()),
    })
}

fn i32_cmp(op: CmpOp, a: i32, b: i32) -> bool {
    use CmpOp::*;
    match op {
        Eq => a == b,
        Ne => a != b,
        LtS => a < b,
        LtU => (a as u32) < (b as u32),
        GtS => a > b,
        GtU => (a as u32) > (b as u32),
        LeS => a <= b,
        LeU => (a as u32) <= (b as u32),
        GeS => a >= b,
        GeU => (a as u32) >= (b as u32),
        _ => false, // float comparisons are type-tagged elsewhere
    }
}

fn i64_cmp(op: CmpOp, a: i64, b: i64) -> bool {
    use CmpOp::*;
    match op {
        Eq => a == b,
        Ne => a != b,
        LtS => a < b,
        LtU => (a as u64) < (b as u64),
        GtS => a > b,
        GtU => (a as u64) > (b as u64),
        LeS => a <= b,
        LeU => (a as u64) <= (b as u64),
        GeS => a >= b,
        GeU => (a as u64) >= (b as u64),
        _ => false,
    }
}

fn f32_cmp(op: CmpOp, a: f32, b: f32) -> bool {
    use CmpOp::*;
    match op {
        Eq => a == b,
        Ne => a != b,
        FLt => a < b,
        FGt => a > b,
        FLe => a <= b,
        FGe => a >= b,
        _ => false,
    }
}

fn f64_cmp(op: CmpOp, a: f64, b: f64) -> bool {
    use CmpOp::*;
    match op {
        Eq => a == b,
        Ne => a != b,
        FLt => a < b,
        FGt => a > b,
        FLe => a <= b,
        FGe => a >= b,
        _ => false,
    }
}

// The spec's fmin/fmax: NaN wins, and among zeros the sign matters —
// min(+0,-0) is -0, max(+0,-0) is +0.

fn f32_min(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        return f32::NAN;
    }
    if a == 0.0 && b == 0.0 {
        return if a.is_sign_negative() || b.is_sign_negative() {
            -0.0
        } else {
            0.0
        };
    }
    if a < b {
        a
    } else {
        b
    }
}

fn f32_max(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        return f32::NAN;
    }
    if a == 0.0 && b == 0.0 {
        return if a.is_sign_positive() || b.is_sign_positive() {
            0.0
        } else {
            -0.0
        };
    }
    if a > b {
        a
    } else {
        b
    }
}

fn f64_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    if a == 0.0 && b == 0.0 {
        return if a.is_sign_negative() || b.is_sign_negative() {
            -0.0
        } else {
            0.0
        };
    }
    if a < b {
        a
    } else {
        b
    }
}

fn f64_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    if a == 0.0 && b == 0.0 {
        return if a.is_sign_positive() || b.is_sign_positive() {
            0.0
        } else {
            -0.0
        };
    }
    if a > b {
        a
    } else {
        b
    }
}

// ── conversions ────────────────────────────────────────────────────────

/// Truncation bounds compared after `.trunc()` — the spec converts the
/// truncated value, so -0.9 → 0 is in range for the unsigned forms.
const I32_S_MIN: f64 = -2147483648.0;
const I32_S_LIM: f64 = 2147483648.0;
const I32_U_LIM: f64 = 4294967296.0;
const I64_S_MIN: f64 = -9223372036854775808.0;
const I64_S_LIM: f64 = 9223372036854775808.0;
const I64_U_LIM: f64 = 18446744073709551616.0;

fn bad_conversion<T>() -> Result<T, ExecError> {
    Err(trap("invalid conversion to integer"))
}

pub(super) fn exec_conv(st: &mut St, op: ConvOp) -> Result<(), ExecError> {
    use ConvOp as C;
    match op {
        C::I32WrapI64 => {
            let a = pop_i64(st)?;
            st.stack.push(Val::I32(a as i32));
        }
        C::I32TruncF32S => {
            let a = pop_f32(st)?;
            let t = f64::from(a).trunc();
            if a.is_nan() || !(I32_S_MIN..I32_S_LIM).contains(&t) {
                return bad_conversion();
            }
            st.stack.push(Val::I32(t as i32));
        }
        C::I32TruncF32U => {
            let a = pop_f32(st)?;
            let t = f64::from(a).trunc();
            if a.is_nan() || !(t > -1.0 && t < I32_U_LIM) {
                return bad_conversion();
            }
            st.stack.push(Val::I32(t as u32 as i32));
        }
        C::I32TruncF64S => {
            let a = pop_f64(st)?;
            let t = a.trunc();
            if a.is_nan() || !(I32_S_MIN..I32_S_LIM).contains(&t) {
                return bad_conversion();
            }
            st.stack.push(Val::I32(t as i32));
        }
        C::I32TruncF64U => {
            let a = pop_f64(st)?;
            let t = a.trunc();
            if a.is_nan() || !(t > -1.0 && t < I32_U_LIM) {
                return bad_conversion();
            }
            st.stack.push(Val::I32(t as u32 as i32));
        }
        C::I64ExtendI32S => {
            let a = pop_i32(st)?;
            st.stack.push(Val::I64(i64::from(a)));
        }
        C::I64ExtendI32U => {
            let a = pop_i32(st)?;
            st.stack.push(Val::I64(i64::from(a as u32)));
        }
        C::I64TruncF32S => {
            let a = pop_f32(st)?;
            let t = f64::from(a).trunc();
            if a.is_nan() || !(I64_S_MIN..I64_S_LIM).contains(&t) {
                return bad_conversion();
            }
            st.stack.push(Val::I64(t as i64));
        }
        C::I64TruncF32U => {
            let a = pop_f32(st)?;
            let t = f64::from(a).trunc();
            if a.is_nan() || !(t > -1.0 && t < I64_U_LIM) {
                return bad_conversion();
            }
            st.stack.push(Val::I64(t as u64 as i64));
        }
        C::I64TruncF64S => {
            let a = pop_f64(st)?;
            let t = a.trunc();
            if a.is_nan() || !(I64_S_MIN..I64_S_LIM).contains(&t) {
                return bad_conversion();
            }
            st.stack.push(Val::I64(t as i64));
        }
        C::I64TruncF64U => {
            let a = pop_f64(st)?;
            let t = a.trunc();
            if a.is_nan() || !(t > -1.0 && t < I64_U_LIM) {
                return bad_conversion();
            }
            st.stack.push(Val::I64(t as u64 as i64));
        }
        C::F32ConvertI32S => {
            let a = pop_i32(st)?;
            st.stack.push(Val::F32(a as f32));
        }
        C::F32ConvertI32U => {
            let a = pop_i32(st)?;
            st.stack.push(Val::F32(a as u32 as f32));
        }
        C::F32ConvertI64S => {
            let a = pop_i64(st)?;
            st.stack.push(Val::F32(a as f32));
        }
        C::F32ConvertI64U => {
            let a = pop_i64(st)?;
            st.stack.push(Val::F32(a as u64 as f32));
        }
        C::F32DemoteF64 => {
            let a = pop_f64(st)?;
            st.stack.push(Val::F32(a as f32));
        }
        C::F64ConvertI32S => {
            let a = pop_i32(st)?;
            st.stack.push(Val::F64(f64::from(a)));
        }
        C::F64ConvertI32U => {
            let a = pop_i32(st)?;
            st.stack.push(Val::F64(f64::from(a as u32)));
        }
        C::F64ConvertI64S => {
            let a = pop_i64(st)?;
            st.stack.push(Val::F64(a as f64));
        }
        C::F64ConvertI64U => {
            let a = pop_i64(st)?;
            st.stack.push(Val::F64(a as u64 as f64));
        }
        C::F64PromoteF32 => {
            let a = pop_f32(st)?;
            st.stack.push(Val::F64(f64::from(a)));
        }
        C::I32ReinterpretF32 => {
            let a = pop_f32(st)?;
            st.stack.push(Val::I32(a.to_bits() as i32));
        }
        C::I64ReinterpretF64 => {
            let a = pop_f64(st)?;
            st.stack.push(Val::I64(a.to_bits() as i64));
        }
        C::F32ReinterpretI32 => {
            let a = pop_i32(st)?;
            st.stack.push(Val::F32(f32::from_bits(a as u32)));
        }
        C::F64ReinterpretI64 => {
            let a = pop_i64(st)?;
            st.stack.push(Val::F64(f64::from_bits(a as u64)));
        }
    }
    Ok(())
}
