//! Feasibility spike — Option-C binder-resolution BACK-HALF.
//!
//! Proves the production claim of `plans/ghci-implementation-plan.md` §2/§3: a
//! GHCi-style session reference reaches codegen as an **external**
//! `NVar(stableVarId)` (0xFE-tagged), and when the JIT's `ExternalEnv` maps that
//! id to a live binding, the Var-miss site resolves it to the bound value — NOT
//! a poison / `unresolved_var_trap`.
//!
//! Crucially, this is the **GC-safe slot-indirection** mechanism we'll ship: the
//! `ExternalEnv` carries the binding table's stable `root_slot: *mut *mut u8`
//! (the GC-updated persistent root), and resolution emits a **load from that
//! slot** at run time — reading the live, GC-current heap pointer — NOT an
//! `iconst` snapshot of the pointer value (which goes stale when a major
//! old-space compaction relocates the value, leaving an already-compiled
//! fragment dereferencing freed memory).
//!
//! Single compile+run proves the mechanism; the relocation test proves the load
//! is live (a fragment compiled BEFORE the slot is repointed still sees the NEW
//! value). Plus the negative control (id absent → trap). Multi-fragment re-entry
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
/// pointer to it. Stands in for a tenured, persistently-rooted session value.
fn boxed_int(n: i64) -> (Vec<u64>, *mut u8) {
    // 3 × u64 = 24 bytes = LIT_TOTAL_SIZE, naturally 8-aligned.
    let mut storage = vec![0u64; (layout::LIT_TOTAL_SIZE / 8) as usize];
    let ptr = storage.as_mut_ptr() as *mut u8;
    unsafe {
        tidepool_heap::layout::write_header(ptr, layout::TAG_LIT, layout::LIT_TOTAL_SIZE as u16);
        *ptr.add(layout::LIT_TAG_OFFSET as usize) = layout::LIT_TAG_INT as u8;
        *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *mut i64) = n;
    }
    (storage, ptr)
}

/// Compile `tree` with the seeded `ExternalEnv`, run `between` after the code is
/// finalized but BEFORE the call (the seam where a GC relocation would happen),
/// then run once and return the raw result pointer. Mirrors the
/// `emit_expr.rs::compile_and_run` harness, threading a non-empty env + the
/// post-finalize mutation hook.
fn compile_then_run(tree: &CoreExpr, env: &ExternalEnv, between: impl FnOnce()) -> *const u8 {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols()).unwrap();
    let func_id = compile_expr(&mut pipeline, tree, "spike_fn", env).expect("compile_expr failed");
    pipeline.finalize().expect("failed to finalize");

    let mut nursery = vec![0u8; 65536];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(nursery.len()) };
    let mut vmctx = VMContext::new(start, end, host_fns::gc_trigger);

    host_fns::set_gc_state(start, nursery.len());
    host_fns::set_stack_map_registry(&pipeline.stack_maps);

    // Simulate whatever happens between fragment compilation and its run —
    // e.g. a GC that relocates the bound value and rewrites *root_slot.
    between();

    let fp = pipeline.get_function_ptr(func_id);
    let func: unsafe extern "C" fn(*mut VMContext) -> i64 = unsafe { std::mem::transmute(fp) };
    let result = unsafe { func(&mut vmctx as *mut VMContext) };
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
/// `ExternalEnv` to a stable root slot holding a live boxed `Int`, resolves to
/// that value by loading through the slot.
#[test]
fn external_var_resolves_via_slot() {
    let (storage, value_ptr) = boxed_int(42);
    // The stable GC root slot the binding table would own; *slot is the live ptr.
    let mut slot: *mut u8 = value_ptr;
    let slot_addr: *mut *mut u8 = std::ptr::addr_of_mut!(slot);

    let var_id = external_var_id(0x5151);
    let mut env = ExternalEnv::new();
    env.insert(var_id, slot_addr);

    let tree = CoreExpr {
        nodes: vec![CoreFrame::Var(var_id)],
    };

    let result = compile_then_run(&tree, &env, || {});

    assert_eq!(
        result, value_ptr as *const u8,
        "external Var must resolve (via slot load) to the seeded heap pointer"
    );
    assert_ne!(
        result,
        host_fns::error_poison_ptr() as *const u8,
        "external Var must NOT resolve to the poison pointer"
    );
    unsafe {
        assert_eq!(read_lit_int(result), 42, "seeded boxed Int round-trips");
    }

    drop(storage);
}

/// THE REAL PROOF (GC-safety): the slot is read LIVE, not snapshotted. A fragment
/// is compiled while the slot points at value A (42); then — simulating a GC
/// old-space compaction that relocates the binding and rewrites `*root_slot` —
/// the slot is repointed to value B (99) BEFORE the fragment runs. The running
/// fragment must observe B, proving it loads through the slot at run time rather
/// than baking the pointer at compile time.
#[test]
fn slot_is_read_live_not_snapshotted() {
    let (storage_a, ptr_a) = boxed_int(42);
    let (storage_b, ptr_b) = boxed_int(99);

    let mut slot: *mut u8 = ptr_a; // slot points at A at COMPILE time
    let slot_addr: *mut *mut u8 = std::ptr::addr_of_mut!(slot);

    let var_id = external_var_id(0x7373);
    let mut env = ExternalEnv::new();
    env.insert(var_id, slot_addr);

    let tree = CoreExpr {
        nodes: vec![CoreFrame::Var(var_id)],
    };

    // Repoint the slot to B after finalize, before the call — a GC relocation.
    let result = compile_then_run(&tree, &env, || unsafe {
        *slot_addr = ptr_b;
    });

    assert_eq!(
        result, ptr_b as *const u8,
        "fragment must read the slot LIVE (see relocated value B), not a baked snapshot of A"
    );
    assert_ne!(
        result, ptr_a as *const u8,
        "a stale snapshot of A would be a GC use-after-free — must not happen"
    );
    unsafe {
        assert_eq!(
            read_lit_int(result),
            99,
            "live-read sees the post-relocation value"
        );
    }

    drop(storage_a);
    drop(storage_b);
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

    let result = compile_then_run(&tree, &env, || {});

    assert_eq!(
        result,
        host_fns::error_poison_ptr() as *const u8,
        "an external Var absent from ExternalEnv must trap to the poison pointer"
    );
}
