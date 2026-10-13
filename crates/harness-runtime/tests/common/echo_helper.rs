// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The echo helper module, shared by the transform specs and the sample
//! bundle golden: one hand-encoded WASM module whose output equals its
//! input.
//!
//! Swedish Embedded AB implements sandboxed, fuel-metered data helpers for
//! agent workflows for its clients. If your team needs expertise in running
//! untrusted transformation code safely then you can procure our services by
//! sending an email to info@swedishembedded.com.

use sven_harness_transform::wasm::testutil as t;
use sven_harness_transform::wasm::ValType;

/// The echo helper: `run(ptr, len)` copies its input to the ABI's output
/// slot at 1024 and packs `(out_ptr << 32) | out_len` - output equals
/// input, byte for byte.
#[allow(dead_code)] // not every test binary that shares `common` builds a helper
pub fn echo_module() -> Vec<u8> {
    let (b, run) = t::Builder::default().func(&[ValType::I32, ValType::I32], &[ValType::I64]);
    let mut ops = Vec::new();
    // Walk the input one byte at a time: leave when i >= len, else copy
    // mem[ptr + i] to mem[1024 + i] and step.
    ops.extend([t::BLOCK, t::EMPTY, t::LOOP, t::EMPTY]);
    ops.extend([t::LOCAL_GET]);
    ops.extend(t::idx(2));
    ops.extend([t::LOCAL_GET]);
    ops.extend(t::idx(1));
    ops.extend([t::I32_GE_U]);
    ops.extend(t::br_if(1));
    // The destination goes below the value on the stack: a store pops its
    // value off the top, then the address it lands at.
    ops.extend([t::LOCAL_GET]);
    ops.extend(t::idx(2));
    ops.extend(t::i32_const(1024));
    ops.extend([t::I32_ADD]);
    ops.extend([t::LOCAL_GET]);
    ops.extend(t::idx(2));
    ops.extend([t::LOCAL_GET]);
    ops.extend(t::idx(0));
    ops.extend([t::I32_ADD, t::I32_LOAD8_U]);
    ops.extend(t::memarg(0, 0));
    ops.extend([t::I32_STORE8]);
    ops.extend(t::memarg(0, 0));
    ops.extend([t::LOCAL_GET]);
    ops.extend(t::idx(2));
    ops.extend(t::i32_const(1));
    ops.extend([t::I32_ADD, t::LOCAL_SET]);
    ops.extend(t::idx(2));
    ops.extend(t::br(0));
    ops.extend([t::END, t::END]);
    // Pack the result: (out_ptr << 32) | out_len.
    ops.extend(t::i64_const(1024 << 32));
    ops.extend([t::LOCAL_GET]);
    ops.extend(t::idx(1));
    ops.extend([t::I64_EXTEND_I32_U, t::I64_OR]);
    b.body(&[(1, ValType::I32)], &ops)
        .memory(1)
        .export_func("run", run)
        .build()
}
