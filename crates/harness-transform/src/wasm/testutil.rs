// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A test-only WebAssembly module encoder: the inverse of the decoder, just
//! enough to hand-assemble the small modules the tests need. Never compiled
//! outside `cfg(test)`.

#![allow(dead_code)]

use super::{decode::decode, validate::validate_module, ValType, WasmError};

// ── scalar encoders ────────────────────────────────────────────────────

pub fn leb32(mut n: u32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut b = (n & 0x7F) as u8;
        n >>= 7;
        if n != 0 {
            b |= 0x80;
        }
        out.push(b);
        if n == 0 {
            return out;
        }
    }
}

/// Signed LEB128 as the spec encodes `i32.const`/`i64.const` payloads.
pub fn sleb(mut v: i64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let b = (v as u8) & 0x7F;
        v >>= 7;
        let sign_bit = b & 0x40 != 0;
        if (v == 0 && !sign_bit) || (v == -1 && sign_bit) {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

pub fn vt(t: ValType) -> u8 {
    match t {
        ValType::I32 => 0x7F,
        ValType::I64 => 0x7E,
        ValType::F32 => 0x7D,
        ValType::F64 => 0x7C,
    }
}

// ── instruction fragments ──────────────────────────────────────────────

pub fn i32_const(v: i32) -> Vec<u8> {
    let mut out = vec![0x41];
    out.extend(sleb(v as i64));
    out
}

pub fn i64_const(v: i64) -> Vec<u8> {
    let mut out = vec![0x42];
    out.extend(sleb(v));
    out
}

pub fn f32_const(v: f32) -> Vec<u8> {
    let mut out = vec![0x43];
    out.extend(v.to_le_bytes());
    out
}

pub fn f64_const(v: f64) -> Vec<u8> {
    let mut out = vec![0x44];
    out.extend(v.to_le_bytes());
    out
}

pub const BLOCK: u8 = 0x02;
pub const LOOP: u8 = 0x03;
pub const IF: u8 = 0x04;
pub const ELSE: u8 = 0x05;
pub const END: u8 = 0x0B;
pub const BR: u8 = 0x0C;
pub const BR_IF: u8 = 0x0D;
pub const RETURN: u8 = 0x0F;
pub const DROP: u8 = 0x1A;
pub const SELECT: u8 = 0x1B;
pub const LOCAL_GET: u8 = 0x20;
pub const LOCAL_SET: u8 = 0x21;
pub const LOCAL_TEE: u8 = 0x22;
pub const GLOBAL_GET: u8 = 0x23;
pub const GLOBAL_SET: u8 = 0x24;
pub const I32_LOAD: u8 = 0x28;
pub const I32_LOAD8_U: u8 = 0x2D;
pub const I32_STORE: u8 = 0x36;
pub const I32_STORE8: u8 = 0x3A;
pub const MEMORY_SIZE: u8 = 0x3F;
pub const MEMORY_GROW: u8 = 0x40;
pub const CALL: u8 = 0x10;
pub const BR_TABLE: u8 = 0x0E;
pub const EMPTY: u8 = 0x40;
pub const I32_ADD: u8 = 0x6A;
pub const I32_DIV_S: u8 = 0x6D;
pub const I32_EQ: u8 = 0x46;
pub const I32_EQZ: u8 = 0x45;
pub const I32_GE_U: u8 = 0x4F;
pub const I32_SUB: u8 = 0x6B;
pub const I32_MUL: u8 = 0x6C;
pub const I64_OR: u8 = 0x84;
pub const I64_SHL: u8 = 0x86;
pub const I64_ADD: u8 = 0x7C;
pub const I64_EXTEND_I32_U: u8 = 0xAD;

pub fn memarg(align: u32, offset: u32) -> Vec<u8> {
    let mut out = leb32(align);
    out.extend(leb32(offset));
    out
}

pub fn br(depth: u32) -> Vec<u8> {
    let mut out = vec![BR];
    out.extend(leb32(depth));
    out
}

pub fn br_if(depth: u32) -> Vec<u8> {
    let mut out = vec![BR_IF];
    out.extend(leb32(depth));
    out
}

pub fn idx(i: u32) -> Vec<u8> {
    leb32(i)
}

/// Local declarations as the code section encodes them: runs of
/// (count, type).
fn locals_decls(locals: &[(u32, ValType)]) -> Vec<u8> {
    let mut out = leb32(locals.len() as u32);
    for (count, ty) in locals {
        out.extend(leb32(*count));
        out.push(vt(*ty));
    }
    out
}

// ── module assembly ────────────────────────────────────────────────────

#[derive(Default)]
pub struct Builder {
    types: Vec<(Vec<ValType>, Vec<ValType>)>,
    funcs: Vec<u32>,
    memory: Option<Vec<u8>>,
    globals: Vec<Vec<u8>>,
    exports: Vec<(String, u8, u32)>,
    data: Vec<(i32, Vec<u8>)>,
    bodies: Vec<Vec<u8>>,
}

impl Builder {
    /// Declares a function type and a function of it; returns the
    /// function's index. The body comes from [`Builder::body`], in order.
    pub fn func(mut self, params: &[ValType], results: &[ValType]) -> (Self, u32) {
        let ty = self.types.len() as u32;
        self.types.push((params.to_vec(), results.to_vec()));
        let idx = self.funcs.len() as u32;
        self.funcs.push(ty);
        (self, idx)
    }

    pub fn body(mut self, locals: &[(u32, ValType)], ops: &[u8]) -> Self {
        let mut b = locals_decls(locals);
        b.extend_from_slice(ops);
        b.push(END);
        self.bodies.push(b);
        self
    }

    pub fn memory(mut self, min: u32) -> Self {
        let mut m = leb32(0);
        m.extend(leb32(min));
        self.memory = Some(m);
        self
    }

    pub fn memory_capped(mut self, min: u32, max: u32) -> Self {
        let mut m = leb32(1);
        m.extend(leb32(min));
        m.extend(leb32(max));
        self.memory = Some(m);
        self
    }

    pub fn global(mut self, mutable: bool, ty: ValType, init: &[u8]) -> Self {
        let mut g = vec![if mutable { 0x01 } else { 0x00 }, vt(ty)];
        g.extend_from_slice(init);
        g.push(END);
        self.globals.push(g);
        self
    }

    pub fn export_func(mut self, name: &str, fidx: u32) -> Self {
        self.exports.push((name.to_string(), 0x00, fidx));
        self
    }

    pub fn export_global(mut self, name: &str, gidx: u32) -> Self {
        self.exports.push((name.to_string(), 0x03, gidx));
        self
    }

    pub fn export_memory(mut self, name: &str) -> Self {
        self.exports.push((name.to_string(), 0x02, 0));
        self
    }

    pub fn data(mut self, offset: i32, bytes: &[u8]) -> Self {
        self.data.push((offset, bytes.to_vec()));
        self
    }

    /// The encoded module bytes.
    pub fn build(self) -> Vec<u8> {
        let mut out = vec![0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00];
        let section = |id: u8, payload: &[u8], out: &mut Vec<u8>| {
            out.push(id);
            out.extend(leb32(payload.len() as u32));
            out.extend_from_slice(payload);
        };
        if !self.types.is_empty() {
            let mut p = leb32(self.types.len() as u32);
            for (params, results) in &self.types {
                p.push(0x60);
                p.extend(leb32(params.len() as u32));
                for t in params {
                    p.push(vt(*t));
                }
                p.extend(leb32(results.len() as u32));
                for t in results {
                    p.push(vt(*t));
                }
            }
            section(1, &p, &mut out);
        }
        if !self.funcs.is_empty() {
            let mut p = leb32(self.funcs.len() as u32);
            for ty in &self.funcs {
                p.extend(leb32(*ty));
            }
            section(3, &p, &mut out);
        }
        if let Some(m) = &self.memory {
            section(5, m, &mut out);
        }
        if !self.globals.is_empty() {
            let mut p = leb32(self.globals.len() as u32);
            for g in &self.globals {
                p.extend_from_slice(g);
            }
            section(6, &p, &mut out);
        }
        if !self.exports.is_empty() {
            let mut p = leb32(self.exports.len() as u32);
            for (name, kind, index) in &self.exports {
                p.extend(leb32(name.len() as u32));
                p.extend_from_slice(name.as_bytes());
                p.push(*kind);
                p.extend(leb32(*index));
            }
            section(7, &p, &mut out);
        }
        if !self.bodies.is_empty() {
            let mut p = leb32(self.bodies.len() as u32);
            for b in &self.bodies {
                p.extend(leb32(b.len() as u32));
                p.extend_from_slice(b);
            }
            section(10, &p, &mut out);
        }
        if !self.data.is_empty() {
            let mut p = leb32(self.data.len() as u32);
            for (offset, bytes) in &self.data {
                p.extend(leb32(0)); // active memory-0 segment
                p.extend(i32_const(*offset));
                p.push(END);
                p.extend(leb32(bytes.len() as u32));
                p.extend_from_slice(bytes);
            }
            section(11, &p, &mut out);
        }
        out
    }
}

/// The module shape nearly every test wants: one function, exported as
/// `run`, plus whatever declarations the test adds around it.
pub fn module_with_run(
    params: &[ValType],
    results: &[ValType],
    locals: &[(u32, ValType)],
    ops: &[u8],
) -> Vec<u8> {
    let (b, fidx) = Builder::default().func(params, results);
    b.body(locals, ops).export_func("run", fidx).build()
}

/// Decode then validate, the way the adapter loads a module.
pub fn check(bytes: &[u8]) -> Result<(), WasmError> {
    let module = decode(bytes)?;
    validate_module(&module)
}
