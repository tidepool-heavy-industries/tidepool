//! Feasibility spike — Option-C binder-resolution BACK-HALF.
//!
//! Proves the production claim of `plans/ghci-implementation-plan.md` §2/§3: a
//! GHCi-style session reference reaches codegen as an **external**
//! `NVar(stableVarId)` (0xFE-tagged), and when the JIT's `ExternalEnv` maps that
//! id to a live heap pointer, the Var-miss site resolves it to that *seeded*
//! pointer — NOT a poison / `unresolved_var_trap`.
//!
//! Single compile+run (the mechanism), plus the negative control (an id absent
//! from `ExternalEnv` still traps, no regression). Multi-fragment re-entry
//! (`add_function`/`run_fragment`) is out of scope here — separately known-viable.

use tidepool_codegen::context::VMContext;
use tidepool_codegen::emit::expr::compile_expr;
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::host_fns;
use tidepool_codegen::layout;
use tidepool_codegen::pipeline::CodegenPipeline;
use tidepool_repr::{CoreExpr, CoreFrame, VarId};

/// High-byte tag GHC/Option-C stamps on a session binder's external id
/// (`binding_table.rs`: parallel to the error sentinel's `0x45`). The resolution
/// is keyed on `ExternalEnv` membership, NOT this tag — it is here only to make
/// the fixture faithful to a real `stableVarId`.
const EXTERNAL_TAG: u64 = 0xFE;

/// Mint a 0xFE-tagged external id mimicking `stableVarId("…Session.Val.G<g>:x")`.
fn external_var_id(key: u64) -> VarId {
    VarId((EXTERNAL_TAG << 56) | (key & ((1u64 << 56) - 1)))
}

/// Build a stable, 8-byte-aligned boxed-`Int` heap object (`LIT_TAG_INT`,
/// `value = n`) and return the backing storage (kept alive by the caller) plus a
/// pointer to it. This stands in for a tenured, persistently-rooted session
/// value seeded into `ExternalEnv` (Wave 1.A); for this single compile+run no GC
/// runs, so a stable Rust-owned buffer is sufficient and faithful at the pointer
/// boundary the resolution arm cares about.
fn boxed_int(n: i64) -> (Vec<u64>, *const u8) {
    // 3 × u64 = 24 bytes = LIT_TOTAL_SIZE, naturally 8-aligned.
    let mut storage = vec![0u64; (layout::LIT_TOTAL_SIZE / 8) as usize];
    let ptr = storage.as_mut_ptr() as *mut u8;
    unsafe {
        tidepool_heap::layout::write_header(ptr, layout::TAG_LIT, layout::LIT_TOTAL_SIZE as u16);
        *ptr.add(layout::LIT_TAG_OFFSET as usize) = layout::LIT_TAG_INT as u8;
        *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut i64) = n;
    }
    (storage, ptr as *const u8)
}

/// Compile `tree` with the given seeded `ExternalEnv` and run it once,
/// returning the raw result pointer the JIT produced. Mirrors the
/// `emit_expr.rs::compile_and_run` harness, threading a non-empty env.
fn compile_and_run_with_env(tree: &CoreExpr, env: &ExternalEnv) -> *const u8 {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols()).unwrap();
    let func_id = compile_expr(&mut pipeline, tree, "spike_fn", env).expect("compile_expr failed");
    pipeline.finalize().expect("failed to finalize");

    let mut nursery = vec![0u8; 65536];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(nursery.len()) };
    let mut vmctx = VMContext::new(start, end, host_fns::gc_trigger);

    host_fns::set_gc_state(start, nursery.len());
    host_fns::set_stack_map_registry(&pipeline.stack_maps);

    let fp = pipeline.get_function_ptr(func_id);
    let func: unsafe extern "C" fn(*mut VMContext) -> i64 = unsafe { std::mem::transmute(fp) };
    let result = unsafe { func(&mut vmctx as *mut VMContext) };
    // Keep buffers alive until after the call.
    drop(nursery);
    result as *const u8
}

unsafe fn read_lit_int(ptr: *const u8) -> i64 {
    assert_eq!(
        tidepool_heap::layout::read_tag(ptr),
        layout::TAG_LIT,
        "expected a Lit object"
    );
    *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *const i64)
}

/// POSITIVE: a program that is just an external `NVar(X)`, with `X` seeded in the
/// `ExternalEnv` to a live boxed `Int`, resolves to that seeded pointer.
#[test]
fn external_var_resolves_to_seeded_pointer() {
    let (storage, value_ptr) = boxed_int(42);

    let var_id = external_var_id(0x5151);
    let mut env = ExternalEnv::new();
    env.insert(var_id, value_ptr);

    // The whole program: reference the external binder `x`.
    let tree = CoreExpr {
        nodes: vec![CoreFrame::Var(var_id)],
    };

    let result = compile_and_run_with_env(&tree, &env);

    // The Var-miss site emitted a fresh iconst of the seeded pointer — the JIT
    // returns exactly that pointer, and it reads back as the seeded value.
    assert_eq!(
        result, value_ptr,
        "external Var must resolve to the seeded heap pointer (not poison/trap)"
    );
    assert_ne!(
        result,
        host_fns::error_poison_ptr() as *const u8,
        "external Var must NOT resolve to the poison pointer"
    );
    unsafe {
        assert_eq!(read_lit_int(result), 42, "seeded boxed Int round-trips");
    }

    // Hold the backing storage live across the run/assertions.
    drop(storage);
}

/// NEGATIVE CONTROL: the SAME-shaped 0xFE-tagged Var, but absent from the
/// `ExternalEnv`, still falls through to `unresolved_var_trap` and yields the
/// poison pointer — proving the new arm fires on membership only (no regression
/// of the existing unresolved-var behavior).
#[test]
fn absent_external_var_still_traps() {
    let var_id = external_var_id(0x6262);
    let env = ExternalEnv::new(); // deliberately empty — id not seeded

    let tree = CoreExpr {
        nodes: vec![CoreFrame::Var(var_id)],
    };

    let result = compile_and_run_with_env(&tree, &env);

    assert_eq!(
        result,
        host_fns::error_poison_ptr() as *const u8,
        "an external Var absent from ExternalEnv must trap to the poison pointer"
    );
}
