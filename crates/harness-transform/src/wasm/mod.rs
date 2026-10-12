// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A minimal WebAssembly (MVP core subset) interpreter for harness
//! helpers.
//!
//! Scope, on purpose: this decodes, validates and executes the module
//! shapes a self-contained transform helper compiles to — one linear
//! memory, locals and globals, i32/i64/f32/f64 arithmetic and comparison,
//! control flow and direct calls. Anything the sandbox must not offer is
//! refused at decode time, not merely absent: imports of any kind, tables
//! and indirect calls, the `start` section, and multiple memories. A
//! module with no imports can never reach the outside world through the
//! helper host interface — the isolation the adapter promises is
//! structural, not a matter of permission checks.
//!
//! Fuel bounds the work one execution may do (one unit per executed
//! instruction); a page cap bounds the linear memory. Both are the
//! adapter's call, not the module's.

pub mod decode;
pub mod exec;
pub mod validate;

#[cfg(any(test, feature = "test-util"))]
pub mod testutil;

/// Why decoding, validation or execution refused or failed.
#[derive(Debug, Clone, PartialEq)]
pub struct WasmError(pub String);

impl std::fmt::Display for WasmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for WasmError {}

pub type Result<T> = std::result::Result<T, WasmError>;

pub(crate) fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(WasmError(msg.into()))
}

/// The value types the subset speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValType {
    I32,
    I64,
    F32,
    F64,
}

impl ValType {
    pub(crate) fn from_byte(b: u8) -> Option<ValType> {
        match b {
            0x7F => Some(ValType::I32),
            0x7E => Some(ValType::I64),
            0x7D => Some(ValType::F32),
            0x7C => Some(ValType::F64),
            _ => None,
        }
    }
}

impl std::fmt::Display for ValType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ValType::I32 => "i32",
            ValType::I64 => "i64",
            ValType::F32 => "f32",
            ValType::F64 => "f64",
        })
    }
}

/// A runtime value on the interpreter's stack.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Val {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
}

impl Val {
    pub(crate) fn ty(&self) -> ValType {
        match self {
            Val::I32(_) => ValType::I32,
            Val::I64(_) => ValType::I64,
            Val::F32(_) => ValType::F32,
            Val::F64(_) => ValType::F64,
        }
    }
}

/// A function type: parameters and results. The subset allows at most one
/// result (MVP style).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncType {
    pub params: Vec<ValType>,
    pub results: Vec<ValType>,
}

/// A constant initializer value (globals and data offsets).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Const {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
}

impl Const {
    pub(crate) fn val(&self) -> Val {
        match self {
            Const::I32(x) => Val::I32(*x),
            Const::I64(x) => Val::I64(*x),
            Const::F32(x) => Val::F32(*x),
            Const::F64(x) => Val::F64(*x),
        }
    }
}

/// Whether a global may be written after instantiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalKind {
    Immutable,
    Mutable,
}

#[derive(Debug, Clone)]
pub struct Global {
    pub kind: GlobalKind,
    pub ty: ValType,
    pub init: Const,
}

/// A module's export: the helper ABI addresses functions by name.
#[derive(Debug, Clone)]
pub enum Export {
    Func(u32),
    Global(u32),
    Memory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockType {
    Empty,
    Value(ValType),
}

impl BlockType {
    /// The types a branch to this block's label carries (its results), and
    /// the types its arm must produce.
    pub(crate) fn results(self) -> Vec<ValType> {
        match self {
            BlockType::Empty => Vec::new(),
            BlockType::Value(t) => vec![t],
        }
    }
}

/// The memory access operations, with their natural widths. Sub-word
/// integer forms sign- or zero-extend; sub-word stores only exist for the
/// low bits of their integer type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemOp {
    I32Load,
    I64Load,
    F32Load,
    F64Load,
    I32Load8S,
    I32Load8U,
    I32Load16S,
    I32Load16U,
    I32Store,
    I64Store,
    F32Store,
    F64Store,
    I32Store8,
    I32Store16,
}

impl MemOp {
    /// (value type, byte width, sign-extends)
    pub(crate) fn shape(self) -> (ValType, u32, bool) {
        use MemOp::*;
        match self {
            I32Load => (ValType::I32, 4, false),
            I64Load => (ValType::I64, 8, false),
            F32Load => (ValType::F32, 4, false),
            F64Load => (ValType::F64, 8, false),
            I32Load8S => (ValType::I32, 1, true),
            I32Load8U => (ValType::I32, 1, false),
            I32Load16S => (ValType::I32, 2, true),
            I32Load16U => (ValType::I32, 2, false),
            I32Store => (ValType::I32, 4, false),
            I64Store => (ValType::I64, 8, false),
            F32Store => (ValType::F32, 4, false),
            F64Store => (ValType::F64, 8, false),
            I32Store8 => (ValType::I32, 1, false),
            I32Store16 => (ValType::I32, 2, false),
        }
    }
}

/// The integer and float binary operations, type-tagged at decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    DivS,
    DivU,
    RemS,
    RemU,
    And,
    Or,
    Xor,
    Shl,
    ShrS,
    ShrU,
    Rotl,
    Rotr,
    FAdd,
    FSub,
    FMul,
    FDiv,
    FMin,
    FMax,
    FCopysign,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    LtS,
    LtU,
    GtS,
    GtU,
    LeS,
    LeU,
    GeS,
    GeU,
    FLt,
    FGt,
    FLe,
    FGe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Clz,
    Ctz,
    Popcnt,
    FAbs,
    FNeg,
    FCeil,
    FFloor,
    FTrunc,
    FNearest,
    FSqrt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvOp {
    I32WrapI64,
    I32TruncF32S,
    I32TruncF32U,
    I32TruncF64S,
    I32TruncF64U,
    I64ExtendI32S,
    I64ExtendI32U,
    I64TruncF32S,
    I64TruncF32U,
    I64TruncF64S,
    I64TruncF64U,
    F32ConvertI32S,
    F32ConvertI32U,
    F32ConvertI64S,
    F32ConvertI64U,
    F32DemoteF64,
    F64ConvertI32S,
    F64ConvertI32U,
    F64ConvertI64S,
    F64ConvertI64U,
    F64PromoteF32,
    I32ReinterpretF32,
    I64ReinterpretF64,
    F32ReinterpretI32,
    F64ReinterpretI64,
}

/// A decoded plain instruction.
#[derive(Debug, Clone)]
pub enum Instr {
    Const(Const),
    LocalGet(u32),
    LocalSet(u32),
    LocalTee(u32),
    GlobalGet(u32),
    GlobalSet(u32),
    MemOp(MemOp, u32, u32),
    MemorySize,
    MemoryGrow,
    Br(u32),
    BrIf(u32),
    BrTable(Box<[u32]>, u32),
    Return,
    Call(u32),
    Unreachable,
    Nop,
    Drop,
    Select,
    I32Bin(BinOp),
    I64Bin(BinOp),
    F32Bin(BinOp),
    F64Bin(BinOp),
    I32Cmp(CmpOp),
    I64Cmp(CmpOp),
    F32Cmp(CmpOp),
    F64Cmp(CmpOp),
    I32Un(UnOp),
    I64Un(UnOp),
    F32Un(UnOp),
    F64Un(UnOp),
    I32Eqz,
    I64Eqz,
    Conv(ConvOp),
}

/// A decoded structured instruction: its arms are already nested, so the
/// executor never re-matches `End` or `Else`.
#[derive(Debug, Clone)]
pub enum Op {
    Simple(Instr),
    Block {
        ty: BlockType,
        then: Vec<Op>,
    },
    Loop {
        ty: BlockType,
        body: Vec<Op>,
    },
    If {
        ty: BlockType,
        then: Vec<Op>,
        els: Option<Vec<Op>>,
    },
}

/// One function body: locals (declared ones; parameters are prepended by
/// the caller) and the structured instruction tree.
#[derive(Debug, Clone)]
pub struct FuncBody {
    pub locals: Vec<ValType>,
    pub body: Vec<Op>,
}

/// A decoded module: the subset's sections only.
#[derive(Debug, Default, Clone)]
pub struct Module {
    pub types: Vec<FuncType>,
    /// Type index per defined function, in function index order.
    pub funcs: Vec<u32>,
    pub globals: Vec<Global>,
    pub exports: std::collections::BTreeMap<String, Export>,
    pub memory_min_pages: u32,
    /// (offset in bytes, bytes) per data segment, in order.
    pub data: Vec<(u32, Vec<u8>)>,
    pub bodies: Vec<FuncBody>,
}

impl Module {
    /// The function type of function index `i` (defined functions come
    /// after imports — and there are no imports in the subset).
    pub(crate) fn func_type(&self, i: u32) -> Result<&FuncType> {
        let ty = *self
            .funcs
            .get(i as usize)
            .ok_or_else(|| WasmError(format!("unknown function {i}")))?;
        self.types
            .get(ty as usize)
            .ok_or_else(|| WasmError(format!("unknown type {ty}")))
    }
}
