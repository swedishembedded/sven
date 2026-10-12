//! Executor tests: hand-assembled modules exercising control flow,
//! arithmetic, memory and the depth caps.

use super::super::testutil::*;
use super::super::{decode::decode, validate::validate_module};
use super::*;

/// Glues instruction fragments (byte slices of mixed known lengths)
/// into one op sequence.
fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

fn run(inst: &mut Instance<'_>, args: &[Val], fuel: u64) -> Result<Vec<Val>, ExecError> {
    inst.call_export("run", args, fuel)
}

/// Decode, validate and instantiate in one step; declares `module`
/// (the instance borrows it) and `inst` in the caller's scope.
macro_rules! load {
    ($bytes:expr, $cap:expr, $name:ident) => {
        let module = decode($bytes).expect("module decodes");
        validate_module(&module).expect("module validates");
        let mut $name = instantiate(&module, $cap).expect("module instantiates");
    };
}

/// i32 load at (local 0), for memory tests.
fn load_module() -> Vec<u8> {
    Builder::default()
        .memory(1)
        .func(&[ValType::I32], &[ValType::I32])
        .0
        .body(&[], &cat(&[&[LOCAL_GET, 0], &[I32_LOAD], &memarg(2, 0)]))
        .export_func("run", 0)
        .build()
}

#[test]
fn returns_a_constant() {
    let bytes = module_with_run(&[], &[ValType::I32], &[], &i32_const(42));
    load!(&bytes, 1, inst);
    assert_eq!(run(&mut inst, &[], 100).unwrap(), vec![Val::I32(42)]);
}

#[test]
fn computes_with_parameters() {
    let bytes = module_with_run(
        &[ValType::I32, ValType::I32],
        &[ValType::I32],
        &[],
        &[LOCAL_GET, 0, LOCAL_GET, 1, I32_ADD],
    );
    load!(&bytes, 1, inst);
    let out = run(&mut inst, &[Val::I32(40), Val::I32(2)], 100).unwrap();
    assert_eq!(out, vec![Val::I32(42)]);
}

#[test]
fn initializes_declared_locals_to_zero() {
    let bytes = module_with_run(&[], &[ValType::I32], &[(1, ValType::I32)], &[LOCAL_GET, 0]);
    load!(&bytes, 1, inst);
    assert_eq!(run(&mut inst, &[], 100).unwrap(), vec![Val::I32(0)]);
}

#[test]
fn loops_until_the_counter_runs_out() {
    // Sum 4+3+2+1 into a local, leaving the loop through an empty block
    // once the counter reaches zero — the canonical shape for a loop
    // whose label carries nothing.
    let ops = cat(&[
        &i32_const(4), // n = 4
        &[LOCAL_SET, 1],
        &[LOOP],
        &[vt(ValType::I32)], // loop (result i32)
        &[BLOCK],
        &[EMPTY], // block () { ... }
        &[LOCAL_GET, 1],
        &[I32_EQZ],
        &br_if(0),       // n == 0: leave the block
        &[LOCAL_GET, 0], // acc += n
        &[LOCAL_GET, 1],
        &[I32_ADD],
        &[LOCAL_SET, 0],
        &[LOCAL_GET, 1], // n -= 1
        &i32_const(1),
        &[I32_SUB],
        &[LOCAL_SET, 1],
        &br(1),          // restart the loop
        &[END],          // block end: only the exit path lands here
        &[LOCAL_GET, 0], // loop result: acc
        &[END],
    ]);
    let bytes = module_with_run(
        &[],
        &[ValType::I32],
        &[(1, ValType::I32), (1, ValType::I32)],
        &ops,
    );
    load!(&bytes, 1, inst);
    let out = run(&mut inst, &[], 10_000).unwrap();
    assert_eq!(out, vec![Val::I32(10)]);
}

#[test]
fn recursion_across_direct_calls() {
    // f(n) = n == 0 ? 0 : n + f(n - 1); run(4) == 10. A direct
    // self-reference needs the function index before its body exists,
    // which the two-stage declare-then-body builder supports.
    let (b, f) = Builder::default().func(&[ValType::I32], &[ValType::I32]);
    let mut call_f = vec![CALL];
    call_f.extend(idx(f));
    let ops = cat(&[
        &[LOCAL_GET, 0],
        &[I32_EQZ],
        &[IF],
        &[vt(ValType::I32)],
        &i32_const(0),
        &[ELSE],
        &[LOCAL_GET, 0],
        &[LOCAL_GET, 0],
        &i32_const(1),
        &[I32_SUB],
        &call_f,
        &[I32_ADD],
        &[END],
    ]);
    let bytes = b.body(&[], &ops).export_func("run", f).build();
    load!(&bytes, 1, inst);
    assert_eq!(
        run(&mut inst, &[Val::I32(4)], 10_000).unwrap(),
        vec![Val::I32(10)]
    );
}

#[test]
fn a_branch_carries_its_value_out_of_a_nested_block() {
    let ops = cat(&[&[BLOCK], &[vt(ValType::I32)], &i32_const(7), &br(0), &[END]]);
    let bytes = module_with_run(&[], &[ValType::I32], &[], &ops);
    load!(&bytes, 1, inst);
    assert_eq!(run(&mut inst, &[], 100).unwrap(), vec![Val::I32(7)]);
}

#[test]
fn if_and_else_pick_their_arm() {
    let ops = cat(&[
        &[LOCAL_GET, 0],
        &[IF],
        &[vt(ValType::I32)],
        &i32_const(10),
        &[ELSE],
        &i32_const(20),
        &[END],
    ]);
    let bytes = module_with_run(&[ValType::I32], &[ValType::I32], &[], &ops);
    load!(&bytes, 1, inst);
    assert_eq!(
        run(&mut inst, &[Val::I32(1)], 100).unwrap(),
        vec![Val::I32(10)]
    );
    assert_eq!(
        run(&mut inst, &[Val::I32(0)], 100).unwrap(),
        vec![Val::I32(20)]
    );
}

#[test]
fn select_picks_by_condition() {
    let ops = cat(&[&[LOCAL_GET, 0], &[LOCAL_GET, 1], &[LOCAL_GET, 2], &[SELECT]]);
    let bytes = module_with_run(
        &[ValType::I32, ValType::I32, ValType::I32],
        &[ValType::I32],
        &[],
        &ops,
    );
    load!(&bytes, 1, inst);
    assert_eq!(
        run(&mut inst, &[Val::I32(5), Val::I32(9), Val::I32(1)], 100).unwrap(),
        vec![Val::I32(5)]
    );
    assert_eq!(
        run(&mut inst, &[Val::I32(5), Val::I32(9), Val::I32(0)], 100).unwrap(),
        vec![Val::I32(9)]
    );
}

#[test]
fn br_table_picks_where_to_land() {
    // Three nested (result i32) blocks; 42 sits on the stack while the
    // table chooses which block's label it lands on.
    let ops = cat(&[
        &[BLOCK],
        &[vt(ValType::I32)], // outer: ... * 1000
        &[BLOCK],
        &[vt(ValType::I32)], // middle: ... + 7
        &[BLOCK],
        &[vt(ValType::I32)], // inner: bare 42
        &i32_const(42),
        &[LOCAL_GET, 0],
        &[BR_TABLE],
        &leb32(3), // target count
        &leb32(0), // -> inner
        &leb32(1), // -> middle
        &leb32(2), // -> outer
        &leb32(0), // default -> inner
        &[END],    // inner end
        &i32_const(7),
        &[I32_ADD],
        &[END], // middle end
        &i32_const(1000),
        &[I32_MUL],
        &[END], // outer end
    ]);
    let bytes = module_with_run(&[ValType::I32], &[ValType::I32], &[], &ops);
    load!(&bytes, 1, inst);
    // Inner landing: 42 flows out through both sequences: (42+7)*1000.
    assert_eq!(
        run(&mut inst, &[Val::I32(0)], 100).unwrap(),
        vec![Val::I32(49_000)]
    );
    // Middle landing: 42 skips the +7: 42*1000.
    assert_eq!(
        run(&mut inst, &[Val::I32(1)], 100).unwrap(),
        vec![Val::I32(42_000)]
    );
    // Outer landing: the branch skips the *1000 — the label value is
    // the function's result.
    assert_eq!(
        run(&mut inst, &[Val::I32(2)], 100).unwrap(),
        vec![Val::I32(42)]
    );
    // An out-of-table index takes the default (inner, like 0).
    assert_eq!(
        run(&mut inst, &[Val::I32(99)], 100).unwrap(),
        vec![Val::I32(49_000)]
    );
}

#[test]
fn globals_persist_across_calls() {
    let (b, run_f) = Builder::default()
        .global(true, ValType::I32, &i32_const(10))
        .func(&[], &[ValType::I32]);
    let (b, set_f) = b.func(&[ValType::I32], &[]);
    let bytes = b
        .body(&[], &[GLOBAL_GET, 0])
        .body(&[], &[LOCAL_GET, 0, GLOBAL_SET, 0])
        .export_func("run", run_f)
        .export_func("set", set_f)
        .build();
    load!(&bytes, 1, inst);
    assert_eq!(run(&mut inst, &[], 100).unwrap(), vec![Val::I32(10)]);
    inst.call_export("set", &[Val::I32(77)], 100).unwrap();
    assert_eq!(run(&mut inst, &[], 100).unwrap(), vec![Val::I32(77)]);
}

#[test]
fn memory_stores_and_loads() {
    let bytes = load_module();
    load!(&bytes, 1, inst);
    inst.store(16, &7i32.to_le_bytes()).unwrap();
    let out = run(&mut inst, &[Val::I32(16)], 100).unwrap();
    assert_eq!(out, vec![Val::I32(7)]);
}

#[test]
fn memory_access_out_of_bounds_traps() {
    let bytes = load_module();
    load!(&bytes, 1, inst);
    let err = run(&mut inst, &[Val::I32(65_533)], 100).unwrap_err();
    assert_eq!(err, ExecError::Trap("out of bounds memory access".into()));
}

#[test]
fn memory_grow_reports_failure_softly() {
    let bytes = Builder::default()
        .memory_capped(1, 2)
        .func(&[ValType::I32], &[ValType::I32])
        .0
        .body(&[], &[LOCAL_GET, 0, MEMORY_GROW, 0])
        .export_func("run", 0)
        .build();
    load!(&bytes, 2, inst);
    assert_eq!(
        run(&mut inst, &[Val::I32(1)], 100).unwrap(),
        vec![Val::I32(1)]
    );
    assert_eq!(inst.pages(), 2);
    assert_eq!(
        run(&mut inst, &[Val::I32(5)], 100).unwrap(),
        vec![Val::I32(-1)]
    );
    assert_eq!(inst.pages(), 2);
}

#[test]
fn memory_size_reports_current_pages() {
    let bytes = Builder::default()
        .memory(2)
        .func(&[], &[ValType::I32])
        .0
        .body(&[], &[MEMORY_SIZE, 0])
        .export_func("run", 0)
        .build();
    load!(&bytes, 8, inst);
    assert_eq!(run(&mut inst, &[], 100).unwrap(), vec![Val::I32(2)]);
}

#[test]
fn data_segments_land_at_their_offsets() {
    let bytes = Builder::default()
        .memory(1)
        .func(&[ValType::I32], &[ValType::I32])
        .0
        .body(&[], &cat(&[&[LOCAL_GET, 0], &[I32_LOAD8_U], &memarg(0, 0)]))
        .export_func("run", 0)
        .data(9, b"hi")
        .build();
    load!(&bytes, 1, inst);
    assert_eq!(
        run(&mut inst, &[Val::I32(9)], 100).unwrap(),
        vec![Val::I32(b'h' as i32)]
    );
    assert_eq!(
        run(&mut inst, &[Val::I32(10)], 100).unwrap(),
        vec![Val::I32(b'i' as i32)]
    );
    assert_eq!(
        run(&mut inst, &[Val::I32(11)], 100).unwrap(),
        vec![Val::I32(0)]
    );
}

#[test]
fn an_infinite_loop_exhausts_its_fuel() {
    let bytes = module_with_run(&[], &[], &[], &[LOOP, EMPTY, BR, 0, END]);
    load!(&bytes, 1, inst);
    let err = run(&mut inst, &[], 1_000).unwrap_err();
    assert_eq!(err, ExecError::Fuel);
}

#[test]
fn runaway_recursion_hits_the_call_depth_cap() {
    // f() = f() + 1 — pure self-recursion.
    let (b, f) = Builder::default().func(&[], &[ValType::I32]);
    let mut call_f = vec![CALL];
    call_f.extend(idx(f));
    let ops = cat(&[&call_f, &i32_const(1), &[I32_ADD]]);
    let bytes = b.body(&[], &ops).export_func("run", f).build();
    load!(&bytes, 1, inst);
    let err = run(&mut inst, &[], u64::from(MAX_CALL_DEPTH) * 10).unwrap_err();
    assert_eq!(err, ExecError::Trap("call stack exhausted".into()));
}

#[test]
fn division_by_zero_traps() {
    let ops = cat(&[&i32_const(6), &i32_const(0), &[I32_DIV_S]]);
    let bytes = module_with_run(&[], &[ValType::I32], &[], &ops);
    load!(&bytes, 1, inst);
    let err = run(&mut inst, &[], 100).unwrap_err();
    assert_eq!(err, ExecError::Trap("integer divide by zero".into()));
}

#[test]
fn truncating_a_nan_or_an_out_of_range_float_traps() {
    const TRUNC_F32_S: u8 = 0xA8; // i32.trunc_f32_s
    let module_bytes = |v: f32| {
        module_with_run(
            &[],
            &[ValType::I32],
            &[],
            &cat(&[&f32_const(v)[..], &[TRUNC_F32_S]]),
        )
    };
    load!(&module_bytes(f32::NAN), 1, nan);
    assert!(run(&mut nan, &[], 100).is_err());
    load!(&module_bytes(3.0e9), 1, big);
    assert!(run(&mut big, &[], 100).is_err());
    load!(&module_bytes(2.9), 1, ok);
    assert_eq!(run(&mut ok, &[], 100).unwrap(), vec![Val::I32(2)]);
}

#[test]
fn unreachable_traps() {
    let bytes = module_with_run(&[], &[], &[], &[0x00]);
    load!(&bytes, 1, inst);
    let err = run(&mut inst, &[], 100).unwrap_err();
    assert!(matches!(err, ExecError::Trap(_)));
}

#[test]
fn a_memory_declaration_above_the_page_cap_is_refused() {
    let bytes = Builder::default()
        .memory(3)
        .func(&[], &[])
        .0
        .body(&[], &[])
        .build();
    let module = decode(&bytes).expect("module decodes");
    validate_module(&module).expect("module validates");
    assert!(instantiate(&module, 2).is_err());
}

#[test]
fn a_data_segment_past_the_memory_is_refused_at_instantiation() {
    let bytes = Builder::default()
        .memory(1)
        .func(&[], &[])
        .0
        .body(&[], &[])
        .data(100_000, b"x")
        .build();
    let module = decode(&bytes).expect("module decodes");
    validate_module(&module).expect("module validates");
    assert!(instantiate(&module, 4).is_err());
}

#[test]
fn a_call_with_the_wrong_argument_types_is_refused() {
    let bytes = module_with_run(&[ValType::I32], &[ValType::I32], &[], &[LOCAL_GET, 0]);
    load!(&bytes, 1, inst);
    assert!(run(&mut inst, &[Val::I64(1)], 100).is_err());
    assert!(run(&mut inst, &[], 100).is_err());
}

#[test]
fn a_store_pops_its_value_above_its_address() {
    // store8(1024, 65) then load8_u(1024): the value sits above the
    // address on the stack, so a store popping in the wrong order would
    // write to the wrong place entirely.
    let ops = cat(&[
        &i32_const(1024),
        &i32_const(65),
        &[I32_STORE8],
        &memarg(0, 0),
        &i32_const(1024),
        &[I32_LOAD8_U],
        &memarg(0, 0),
    ]);
    let bytes = Builder::default()
        .memory(1)
        .func(&[], &[ValType::I32])
        .0
        .body(&[], &ops)
        .export_func("run", 0)
        .build();
    load!(&bytes, 1, inst);
    assert_eq!(run(&mut inst, &[], 100).unwrap(), vec![Val::I32(65)]);
}

#[test]
fn a_wide_store_round_trips_through_memory() {
    let ops = cat(&[
        &i32_const(2048),
        &i32_const(-2),
        &[I32_STORE],
        &memarg(2, 0),
        &i32_const(2048),
        &[I32_LOAD],
        &memarg(2, 0),
    ]);
    let bytes = Builder::default()
        .memory(1)
        .func(&[], &[ValType::I32])
        .0
        .body(&[], &ops)
        .export_func("run", 0)
        .build();
    load!(&bytes, 1, inst);
    assert_eq!(run(&mut inst, &[], 100).unwrap(), vec![Val::I32(-2)]);
}
