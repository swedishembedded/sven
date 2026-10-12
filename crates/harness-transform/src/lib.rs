// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The harness helper runtime: an isolated WebAssembly interpreter for
//! `transform` steps, with fuel and memory caps, JSON Schema boundaries at
//! both ends and no host interface a helper could reach the world through.
//!
//! A helper is a WebAssembly module in the sandbox subset (see
//! [`wasm`]): it imports nothing, starts nothing, and can only touch its
//! own linear memory. The host hands it one JSON document and reads one
//! back, through a fixed ABI:
//!
//! ```text
//! run(input_ptr: i32, input_len: i32) -> i64
//!   ; result packs (out_ptr << 32) | out_len
//! ```
//!
//! The host appends the serialized input at the end of the instance's
//! linear memory (growing it in whole pages, within the run's page cap),
//! calls `run`, and reads the output range back out of memory. Every
//! boundary is checked: the input against the input schema before the
//! call, the bytes against memory before they are parsed, and the parsed
//! value against the output schema after. A fresh instance serves every
//! invocation — a helper cannot carry state from one call to the next.

use std::sync::Arc;

use serde_json::Value;
use sha2::{Digest, Sha256};

pub mod cache;
pub mod error;
pub mod wasm;

pub use error::{SchemaStage, TransformError};

use wasm::{decode::decode, exec, validate::validate_module, Export, Module, Val, ValType};

/// The execution budget of one helper invocation. Both bounds are explicit
/// in every call; there are no ambient knobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Interpreter ticks — one per executed instruction.
    pub fuel: u64,
    /// Upper bound on linear memory, in 64 KiB pages (the input the host
    /// appends counts against this, as does any `memory.grow`).
    pub max_pages: u32,
}

/// A compiled helper: a validated sandbox module plus the JSON Schema
/// boundaries its transform step declared. Cheap to clone (the module is
/// shared); every [`Transform::run`] starts from a fresh instance.
#[derive(Debug)]
pub struct Transform {
    module: Arc<Module>,
    input: Option<jsonschema::Validator>,
    output: Option<jsonschema::Validator>,
    digest: [u8; 32],
}

impl Transform {
    /// Compiles helper bytes into a transform: decode, validate, check the
    /// ABI, compile the schemas. This is the expensive step — see
    /// [`cache::ModuleCache`] for memoizing it across invocations.
    pub fn compile(
        wasm: &[u8],
        input_schema: Option<&Value>,
        output_schema: Option<&Value>,
    ) -> Result<Self, TransformError> {
        let module = Arc::new(compile_module(wasm)?);
        Self::from_module(module, digest(wasm), input_schema, output_schema)
    }

    /// Builds a transform around an already-compiled module (a cache hit),
    /// checking the ABI and compiling the schemas.
    pub fn from_module(
        module: Arc<Module>,
        digest: [u8; 32],
        input_schema: Option<&Value>,
        output_schema: Option<&Value>,
    ) -> Result<Self, TransformError> {
        check_abi(&module)?;
        Ok(Self {
            input: compile_schema(input_schema, SchemaStage::Input)?,
            output: compile_schema(output_schema, SchemaStage::Output)?,
            module,
            digest,
        })
    }

    /// The SHA-256 of the helper's bytes — the key plans pin and caches use.
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }

    /// Runs one invocation. The input must pass the input schema, the
    /// helper must finish inside its fuel and memory budgets, and the
    /// output must parse as JSON and pass the output schema. Failure is a
    /// typed [`TransformError`] naming the boundary that broke.
    pub fn run(&self, input: &Value, limits: &Limits) -> Result<Value, TransformError> {
        if let Some(schema) = &self.input {
            if let Some(message) = first_schema_error(schema, input) {
                return Err(TransformError::Schema {
                    stage: SchemaStage::Input,
                    message,
                });
            }
        }
        let mut inst = exec::instantiate(&self.module, limits.max_pages)
            .map_err(|_| TransformError::Memory)?;
        let input_bytes = serde_json::to_vec(input)
            .map_err(|e| TransformError::Contract(format!("input is not serializable: {e}")))?;
        let ptr = inst
            .append(&input_bytes)
            .map_err(|_| TransformError::Memory)?;
        let packed = inst
            .call_export(
                "run",
                &[Val::I32(ptr as i32), Val::I32(input_bytes.len() as i32)],
                limits.fuel,
            )
            .map_err(|e| match e {
                exec::ExecError::Fuel => TransformError::Fuel,
                exec::ExecError::Trap(message) => TransformError::Trap(message),
            })?;
        let (out_ptr, out_len) = match packed.as_slice() {
            [Val::I64(p)] => {
                let packed = *p as u64;
                ((packed >> 32) as u32, packed as u32)
            }
            _ => {
                return Err(TransformError::Contract(
                    "`run` must return exactly one i64".into(),
                ))
            }
        };
        let mem = inst.memory();
        if u64::from(out_ptr) + u64::from(out_len) > mem.len() as u64 {
            return Err(TransformError::Contract(
                "the returned output range escapes helper memory".into(),
            ));
        }
        let out: Value =
            serde_json::from_slice(&mem[out_ptr as usize..out_ptr as usize + out_len as usize])
                .map_err(|e| TransformError::Contract(format!("output is not valid JSON: {e}")))?;
        if let Some(schema) = &self.output {
            if let Some(message) = first_schema_error(schema, &out) {
                return Err(TransformError::Schema {
                    stage: SchemaStage::Output,
                    message,
                });
            }
        }
        Ok(out)
    }
}

/// Decodes and validates helper bytes: the sandbox's whole static gate.
pub fn compile_module(wasm: &[u8]) -> Result<Module, TransformError> {
    let module = decode(wasm).map_err(|e| TransformError::Decode(e.0))?;
    validate_module(&module).map_err(|e| TransformError::Decode(e.0))?;
    Ok(module)
}

/// The SHA-256 of helper bytes — the key caches and plan digests use.
pub fn digest(wasm: &[u8]) -> [u8; 32] {
    Sha256::digest(wasm).into()
}

/// The helper ABI: `run` exported as a function of type
/// `(i32, i32) -> i64`, and a memory to host the input in.
fn check_abi(module: &Module) -> Result<(), TransformError> {
    let fidx = match module.exports.get("run") {
        Some(Export::Func(i)) => *i,
        Some(_) => {
            return Err(TransformError::Abi(
                "the export `run` is not a function".into(),
            ))
        }
        None => {
            return Err(TransformError::Abi(
                "the module does not export `run`".into(),
            ))
        }
    };
    let ty = module
        .func_type(fidx)
        .map_err(|e| TransformError::Abi(e.to_string()))?;
    if ty.params != [ValType::I32, ValType::I32] || ty.results != [ValType::I64] {
        let types = |ts: &[ValType]| {
            ts.iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        return Err(TransformError::Abi(format!(
            "`run` must have type (i32, i32) -> i64, found ({}) -> ({})",
            types(&ty.params),
            types(&ty.results)
        )));
    }
    if module.memory_min_pages == 0 {
        return Err(TransformError::Abi(
            "the module declares no memory to host its input".into(),
        ));
    }
    Ok(())
}

/// Compiles one schema boundary. An undeclared schema is no schema — the
/// value passes unexamined; a declared one that does not itself compile
/// fails the transform (schemas are part of the step's contract).
fn compile_schema(
    schema: Option<&Value>,
    stage: SchemaStage,
) -> Result<Option<jsonschema::Validator>, TransformError> {
    match schema {
        None => Ok(None),
        Some(schema) => {
            jsonschema::validator_for(schema)
                .map(Some)
                .map_err(|e| TransformError::Schema {
                    stage,
                    message: e.to_string(),
                })
        }
    }
}

/// The first schema violation as a message naming the instance path.
fn first_schema_error(validator: &jsonschema::Validator, instance: &Value) -> Option<String> {
    validator.iter_errors(instance).next().map(|e| {
        let path = e.instance_path().to_string();
        if path.is_empty() {
            e.to_string()
        } else {
            format!("at `{path}`: {e}")
        }
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::wasm::testutil::*;

    /// Glues instruction fragments into one op sequence.
    fn cat(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    /// Packs (ptr << 32) | len into an i64 result.
    fn pack(ptr: i32, len: i32) -> Vec<u8> {
        cat(&[
            &i32_const(len),
            &[I64_EXTEND_I32_U],
            &i32_const(ptr),
            &[I64_EXTEND_I32_U],
            &i64_const(32),
            &[I64_SHL],
            &[I64_OR],
        ])
    }

    /// A helper that copies its input bytes to offset 1024 and returns
    /// them: a JSON echo through the whole ABI. Locals: 0 = ptr and
    /// 1 = len (params), 2 = i.
    fn echo_module() -> Vec<u8> {
        let ops = cat(&[
            &[BLOCK],
            &[EMPTY],
            &[LOOP],
            &[EMPTY],
            &[LOCAL_GET, 2],
            &[LOCAL_GET, 1],
            &[I32_GE_U],
            &br_if(1),
            &i32_const(1024),
            &[LOCAL_GET, 2],
            &[I32_ADD], // dst = 1024 + i
            &[LOCAL_GET, 0],
            &[LOCAL_GET, 2],
            &[I32_ADD], // src = ptr + i
            &[I32_LOAD8_U],
            &memarg(0, 0),
            &[I32_STORE8],
            &memarg(0, 0),
            &[LOCAL_GET, 2],
            &i32_const(1),
            &[I32_ADD],
            &[LOCAL_SET, 2],
            &br(0),
            &[END],
            &[END],
            &[LOCAL_GET, 1],
            &[I64_EXTEND_I32_U],
            &i32_const(1024),
            &[I64_EXTEND_I32_U],
            &i64_const(32),
            &[I64_SHL],
            &[I64_OR],
        ]);
        Builder::default()
            .memory(1)
            .func(&[ValType::I32, ValType::I32], &[ValType::I64])
            .0
            .body(&[(1, ValType::I32)], &ops)
            .export_func("run", 0)
            .build()
    }

    /// A helper that ignores its input and always returns a fixed JSON
    /// document parked in a data segment.
    fn const_json_module(document: &[u8], at: i32) -> Vec<u8> {
        Builder::default()
            .memory(1)
            .func(&[ValType::I32, ValType::I32], &[ValType::I64])
            .0
            .body(&[], &pack(at, document.len() as i32))
            .export_func("run", 0)
            .data(at, document)
            .build()
    }

    fn limits() -> Limits {
        Limits {
            fuel: 100_000,
            max_pages: 2,
        }
    }

    #[test]
    fn echoes_its_input_through_the_abi() {
        let transform = Transform::compile(&echo_module(), None, None).unwrap();
        let input = json!({"query": "first", "n": 3});
        let out = transform.run(&input, &limits()).unwrap();
        assert_eq!(out, input);
    }

    #[test]
    fn every_invocation_gets_a_fresh_instance() {
        let transform = Transform::compile(&echo_module(), None, None).unwrap();
        let input = json!("again");
        assert_eq!(transform.run(&input, &limits()).unwrap(), input);
        assert_eq!(transform.run(&input, &limits()).unwrap(), input);
    }

    #[test]
    fn validates_the_input_against_the_schema() {
        let schema = json!({"type": "object"});
        let transform = Transform::compile(&echo_module(), Some(&schema), None).unwrap();
        let err = transform.run(&json!("a string"), &limits()).unwrap_err();
        assert!(
            matches!(
                err,
                TransformError::Schema {
                    stage: SchemaStage::Input,
                    ..
                }
            ),
            "{err}"
        );
        transform.run(&json!({"ok": true}), &limits()).unwrap();
    }

    #[test]
    fn validates_the_output_against_the_schema() {
        let module = const_json_module(br#"{"n":42}"#, 100);
        let schema = json!({"type": "object", "properties": {"n": {"minimum": 100}}});
        let transform = Transform::compile(&module, None, Some(&schema)).unwrap();
        let err = transform.run(&json!(null), &limits()).unwrap_err();
        assert!(
            matches!(
                err,
                TransformError::Schema {
                    stage: SchemaStage::Output,
                    ..
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn a_schema_that_does_not_compile_fails_the_transform_at_compile_time() {
        let schema = json!(42);
        let err = Transform::compile(&echo_module(), Some(&schema), None).unwrap_err();
        assert!(
            matches!(
                err,
                TransformError::Schema {
                    stage: SchemaStage::Input,
                    ..
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn the_abi_is_checked_at_compile_time() {
        // No `run` export at all.
        let bytes = Builder::default()
            .memory(1)
            .func(&[], &[])
            .0
            .body(&[], &[])
            .build();
        assert!(matches!(
            Transform::compile(&bytes, None, None),
            Err(TransformError::Abi(_))
        ));
        // `run` exported as a global, not a function.
        let bytes = Builder::default()
            .memory(1)
            .global(false, ValType::I32, &i32_const(0))
            .func(&[ValType::I32, ValType::I32], &[ValType::I64])
            .0
            .body(&[], &cat(&[&i32_const(1), &[I64_EXTEND_I32_U]]))
            .export_global("run", 0)
            .build();
        assert!(matches!(
            Transform::compile(&bytes, None, None),
            Err(TransformError::Abi(_))
        ));
        // Wrong signature.
        let bytes = module_with_run(&[], &[ValType::I32], &[], &i32_const(1));
        assert!(matches!(
            Transform::compile(&bytes, None, None),
            Err(TransformError::Abi(_))
        ));
        // No memory to host the input in.
        let bytes = Builder::default()
            .func(&[ValType::I32, ValType::I32], &[ValType::I64])
            .0
            .body(&[], &cat(&[&i32_const(1), &[I64_EXTEND_I32_U]]))
            .export_func("run", 0)
            .build();
        assert!(matches!(
            Transform::compile(&bytes, None, None),
            Err(TransformError::Abi(_))
        ));
    }

    #[test]
    fn helper_bytes_are_rejected_at_decode() {
        // Not WebAssembly at all.
        assert!(matches!(
            Transform::compile(b"garbage", None, None),
            Err(TransformError::Decode(_))
        ));
    }

    #[test]
    fn an_infinite_helper_exhausts_its_fuel() {
        // A loop (result i64) that always restarts: valid, never finishes.
        let bytes = Builder::default()
            .memory(1)
            .func(&[ValType::I32, ValType::I32], &[ValType::I64])
            .0
            .body(&[], &[LOOP, vt(ValType::I64), BR, 0, END])
            .export_func("run", 0)
            .build();
        let transform = Transform::compile(&bytes, None, None).unwrap();
        assert!(matches!(
            transform.run(&json!(null), &limits()),
            Err(TransformError::Fuel)
        ));
    }

    #[test]
    fn a_trapping_helper_reports_the_trap() {
        let ops = cat(&[
            &i32_const(1),
            &i32_const(0),
            &[I32_DIV_S],
            &[DROP],
            &pack(0, 0),
        ]);
        let bytes = Builder::default()
            .memory(1)
            .func(&[ValType::I32, ValType::I32], &[ValType::I64])
            .0
            .body(&[], &ops)
            .export_func("run", 0)
            .build();
        let transform = Transform::compile(&bytes, None, None).unwrap();
        let err = transform.run(&json!(null), &limits()).unwrap_err();
        assert!(
            matches!(err, TransformError::Trap(ref m) if m.contains("divide by zero")),
            "{err}"
        );
    }

    #[test]
    fn an_output_range_outside_memory_is_a_contract_violation() {
        let bytes = Builder::default()
            .memory(1)
            .func(&[ValType::I32, ValType::I32], &[ValType::I64])
            .0
            .body(&[], &pack(65_536, 10))
            .export_func("run", 0)
            .build();
        let transform = Transform::compile(&bytes, None, None).unwrap();
        let err = transform.run(&json!(null), &limits()).unwrap_err();
        assert!(matches!(err, TransformError::Contract(_)), "{err}");
    }

    #[test]
    fn an_output_that_is_not_json_is_a_contract_violation() {
        let bytes = const_json_module(b"not json", 100);
        let transform = Transform::compile(&bytes, None, None).unwrap();
        let err = transform.run(&json!(null), &limits()).unwrap_err();
        assert!(
            matches!(err, TransformError::Contract(ref m) if m.contains("not valid JSON")),
            "{err}"
        );
    }

    #[test]
    fn an_input_larger_than_the_page_budget_is_refused() {
        let transform = Transform::compile(&echo_module(), None, None).unwrap();
        let big = json!("x".repeat(70_000));
        let tight = Limits {
            fuel: 1_000_000,
            max_pages: 1,
        };
        assert!(
            matches!(transform.run(&big, &tight), Err(TransformError::Memory)),
            "expected a memory refusal"
        );
    }

    #[test]
    fn digests_pin_helper_content() {
        let a = echo_module();
        let b = const_json_module(br#"{"n":42}"#, 100);
        assert_ne!(digest(&a), digest(&b));
        let transform = Transform::compile(&a, None, None).unwrap();
        assert_eq!(transform.digest(), digest(&a));
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use crate::wasm::testutil::*;

    fn cat(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    fn some_module(n: u8) -> Vec<u8> {
        Builder::default()
            .memory(1)
            .func(&[ValType::I32, ValType::I32], &[ValType::I64])
            .0
            .body(&[], &cat(&[&i32_const(n as i32), &[I64_EXTEND_I32_U]]))
            .export_func("run", 0)
            .build()
    }

    #[test]
    fn caches_a_module_by_its_content() {
        let wasm = some_module(1);
        let mut cache = cache::ModuleCache::bounded(4);
        let first = cache.load(&wasm).unwrap();
        let again = cache.load(&wasm).unwrap();
        assert_eq!(cache.len(), 1);
        assert!(Arc::ptr_eq(&first, &again));
        // Different bytes compile to a different module.
        let other = cache.load(&some_module(2)).unwrap();
        assert_eq!(cache.len(), 2);
        assert!(!Arc::ptr_eq(&first, &other));
    }

    #[test]
    fn evicts_the_oldest_entry_when_full() {
        let a = some_module(1);
        let b = some_module(2);
        let mut cache = cache::ModuleCache::bounded(1);
        let first = cache.load(&a).unwrap();
        let _second = cache.load(&b).unwrap();
        assert_eq!(cache.len(), 1);
        // The first entry was evicted: loading it again recompiles.
        let third = cache.load(&a).unwrap();
        assert!(!Arc::ptr_eq(&first, &third));
        assert_eq!(cache.len(), 1);
    }
}
