//! Targeted proof for cluster F (D6, jit-chain-2): does removing the
//! per-branch-point `EmitContext::declare_env` sweep (formerly called at
//! every case alt-body entry and merge block, see the commit that removed
//! it) leave a live heap pointer unrooted across a case's internal GC
//! safepoints?
//!
//! Contract established from the pinned `cranelift-frontend` 0.129.1 source
//! (`declare_value_needs_stack_map` inserts into a per-Function set consumed
//! ONCE, at `finalize()`, by a whole-function backward-liveness dataflow —
//! see `frontend.rs:554-563`/`722-729` and `frontend/safepoints.rs:455-516`):
//! marking a value ONCE, anywhere in its defining Cranelift `Function`, is
//! enough for Cranelift's own dataflow to recognize it as live at every
//! safepoint between its definition and its last use — regardless of which
//! block the safepoint sits in. So `declare_env`'s branch-point re-sweep is
//! redundant PROVIDED every value it used to touch is already marked at its
//! own point of creation (an allocation, a load, or a block param) — which is
//! the pervasive pattern audited in `emit/expr.rs`/`case.rs`/`join.rs`.
//!
//! This test builds a value (`clo`, a closure) that is allocated (and
//! mark-at-creation'd) BEFORE a `Case`, never touched anywhere inside the
//! `Case` (not the scrutinee, not a captured free variable of anything
//! inside it, never re-bound), and applied AFTER the `Case`'s merge block.
//! The `Case`'s single alt body allocates a large noisy `Con` (100 literal
//! fields), forcing a GC while control sits inside the alt block and the
//! merge block — exactly the blocks `declare_env` used to re-sweep at. If
//! `clo`'s Cranelift `Value` were only kept live by that removed sweep (the
//! block-sensitive hypothesis), the GC would drop it from the root set, its
//! from-space page would later read back as `TIDEPOOL_GC_POISON`'s 0xDD
//! filler, and applying `clo` afterward would hit `debug_app_check`'s
//! `RuntimeError::BadFunPtrTag(0xDD)` ("application of non-closure
//! (tag=221)") — deterministically, not intermittently, because of the
//! poison fill.
//!
//! Nursery sizes are swept (rather than hand-computing the exact
//! bump-allocator offset at which the noise Con's construction straddles a
//! GC) — same rationale as `apply_acceptance.rs`'s
//! `apply_gc_during_application_relocates_forced_callee` and
//! `proptest_gc_recursion.rs`.
//!
//! Mutation-both-directions proof (Step 6), performed by hand against
//! `closure_survives_case_branch_gc_on_creation_mark_alone` and reverted —
//! not committed as a toggle, since the mark lives in `emit_alloc_zeroed`
//! (`emit/expr.rs:75`), the ONE allocation helper shared by every Con/
//! Closure/Thunk allocation, not something this file can scope narrower than
//! that without a source edit.
//!
//! Removing `builder.declare_value_needs_stack_map(ptr);` at `emit/expr.rs:75`
//! (the allocation-result mark `clo`'s closure allocation depends on) made
//! this test fail deterministically, at `nursery_size=256` (the first swept
//! size), with the exact predicted signature on stderr: `[JIT] App:
//! fun_ptr=0x... has tag 221 (UNKNOWN) — expected Closure!` and
//! `result.result_ptr == host_fns::error_poison_ptr()`. Restoring the line
//! made the test pass again (all 77 swept nursery sizes, 1-4 real GCs each
//! per `gc_trigger_call_count`).
//!
//! This mutation is necessarily coarser than "one value class" — it disables
//! marking for every allocation, not just closures — but it durably confirms
//! the allocation-result class specifically, `clo`'s own class, is
//! load-bearing. The block-param and capture-load classes (case.rs:317/330,
//! join.rs:163, expr.rs:1175/1212/1440/2522/2564 etc.) were not separately
//! mutated — each follows the identical one-line-before-use
//! `declare_value_needs_stack_map` shape audited in the commit message's
//! 14-site table, but this file does not carry an independent test per class.

use tidepool_codegen::host_fns;
use tidepool_heap::layout;
use tidepool_repr::*;
use tidepool_testing::jit_run::{compile_and_run, read_lit_int};

/// Build the CoreExpr described in the module doc:
///   let clo = \y -> y + 1
///   in let _noise = case Trigger of Trigger -> Con(NOISE_TAG, [0..100)) in
///      clo 41
fn build_tree(n_noise_fields: i64) -> CoreExpr {
    let clo_var = VarId(0x1001);
    let y_var = VarId(0x1002);
    let noise_result_var = VarId(0x1003);
    let scrut_b = VarId(0x1004);
    let outer_scrut_b = VarId(0x1005);
    let trigger_tag = DataConId(1);
    let noise_tag = DataConId(2);

    let mut nodes = Vec::new();

    // clo = \y -> y + 1
    let y_v = nodes.len();
    nodes.push(CoreFrame::Var(y_var));
    let one_lit = nodes.len();
    nodes.push(CoreFrame::Lit(Literal::LitInt(1)));
    let y_plus_1 = nodes.len();
    nodes.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![y_v, one_lit],
    });
    let clo_lam = nodes.len();
    nodes.push(CoreFrame::Lam {
        binder: y_var,
        body: y_plus_1,
    });

    // Noise Con built INSIDE the case alt body, so its allocation (and any
    // GC it forces) happens with control inside `alt_block`/`merge_block` —
    // the blocks the removed `declare_env` sweeps used to fire in. Each field
    // is its own boxed Lit allocation (`LIT_TOTAL_SIZE` = 24 bytes), so
    // `n_noise_fields` of them plus the Con header itself is real allocation
    // pressure, not just arithmetic (`IntAdd` returns an unboxed `SsaVal::Raw`
    // — an add chain would allocate almost nothing).
    let mut noise_field_idxs = Vec::new();
    for i in 0..n_noise_fields {
        nodes.push(CoreFrame::Lit(Literal::LitInt(i)));
        noise_field_idxs.push(nodes.len() - 1);
    }
    let noise_con = nodes.len();
    nodes.push(CoreFrame::Con {
        tag: noise_tag,
        fields: noise_field_idxs,
    });

    let trigger_con = nodes.len();
    nodes.push(CoreFrame::Con {
        tag: trigger_tag,
        fields: vec![],
    });
    let the_case = nodes.len();
    nodes.push(CoreFrame::Case {
        scrutinee: trigger_con,
        binder: scrut_b,
        alts: vec![Alt {
            con: AltCon::DataAlt(trigger_tag),
            binders: vec![],
            body: noise_con,
        }],
    });

    // clo is applied AFTER the case merges — `clo`'s Value is untouched by
    // (not free in, not captured by, not re-bound within) the case at all.
    let clo_ref = nodes.len();
    nodes.push(CoreFrame::Var(clo_var));
    let lit_41 = nodes.len();
    nodes.push(CoreFrame::Lit(Literal::LitInt(41)));
    let app_clo = nodes.len();
    nodes.push(CoreFrame::App {
        fun: clo_ref,
        arg: lit_41,
    });

    // `noise_result_var` (bound to `the_case`, the noisy Con) must stay FREE
    // in the outer body or the LetNonRec DCE path (`is_ok()` binary search on
    // free vars) skips evaluating it entirely — no allocation, no GC, a
    // vacuous test. `Case` is non-trivial (`is_trivial_field`'s `_ => false`
    // arm), so `the_case` is bound as a THUNK, not evaluated eagerly — it
    // must actually be FORCED to run at all. A `Default`-only outer case does
    // NOT force its scrutinee (`emit_case`'s "Default only" arm emits
    // `alt.body` directly, never touching `scrut`/`scrut_ptr` — confirmed by
    // this test initially going vacuous with `Default`), so the outer case
    // here matches on the real `noise_tag` instead: a `DataAlt` forces
    // through `emit_data_dispatch`'s `heap_force`, which is what actually
    // runs the thunked `the_case` and, as a bonus, exercises
    // `emit_data_dispatch`'s own removed sweep a second time.
    let noise_ref = nodes.len();
    nodes.push(CoreFrame::Var(noise_result_var));
    let outer_case = nodes.len();
    nodes.push(CoreFrame::Case {
        scrutinee: noise_ref,
        binder: outer_scrut_b,
        alts: vec![Alt {
            con: AltCon::DataAlt(noise_tag),
            binders: vec![],
            body: app_clo,
        }],
    });

    let inner_let = nodes.len();
    nodes.push(CoreFrame::LetNonRec {
        binder: noise_result_var,
        rhs: the_case,
        body: outer_case,
    });
    let outer_let = nodes.len();
    nodes.push(CoreFrame::LetNonRec {
        binder: clo_var,
        rhs: clo_lam,
        body: inner_let,
    });
    debug_assert_eq!(outer_let, nodes.len() - 1, "root must be the last node");

    RecursiveTree { nodes }
}

/// Baseline: `clo`'s only stack-map mark is at its own allocation site
/// (`emit_alloc_zeroed`, `expr.rs:75`, via `emit_lam`). With `declare_env`
/// removed from every case branch point, this must still evaluate correctly
/// under both `TIDEPOOL_GC_POISON` and `TIDEPOOL_HEAP_VERIFY` — proving the
/// creation-site mark alone is sufficient across the case's internal
/// safepoints.
#[test]
fn closure_survives_case_branch_gc_on_creation_mark_alone() {
    host_fns::set_gc_poison(true);
    host_fns::set_heap_verify(true);

    let mut any_gc_fired = false;
    for nursery_size in (256..=4096).step_by(48) {
        host_fns::reset_test_counters();
        let tree = build_tree(100);
        let result = compile_and_run(&tree, nursery_size);
        // `compile_and_run`'s bare pipeline harness never registers a
        // `current_machine`, so `host_fns::take_runtime_error()` is always
        // `None` here regardless of outcome (confirmed empirically) — unlike
        // `JitEffectMachine::run_pure`'s tests, this harness's failure signal
        // is the result pointer itself: a poisoned/short-circuited App
        // returns `host_fns::error_poison_ptr()`, whose tag byte is neither
        // `TAG_LIT` nor 42's value, so the asserts below already detect it.
        assert_ne!(
            result.result_ptr,
            host_fns::error_poison_ptr() as *const u8,
            "nursery_size={nursery_size}: clo 41 short-circuited to the \
             error-poison pointer instead of returning 42"
        );
        unsafe {
            assert_eq!(
                layout::read_tag(result.result_ptr),
                layout::TAG_LIT,
                "nursery_size={nursery_size}"
            );
            assert_eq!(
                read_lit_int(result.result_ptr),
                42,
                "nursery_size={nursery_size}: clo 41 = 42, clo defined \
                 before and applied after a Case whose alt body allocates a \
                 100-field noisy Con"
            );
        }
        if host_fns::gc_trigger_call_count() > 0 {
            any_gc_fired = true;
        }
    }
    assert!(
        any_gc_fired,
        "sweep must include at least one nursery_size that actually forces \
         a GC, or this test passes vacuously"
    );
}

/// Sanity check on the harness itself (not a stack-map claim): with a nursery
/// large enough that no GC ever fires, `clo` obviously survives regardless of
/// any marking — this just confirms `build_tree`'s program is correct absent
/// GC pressure, isolating the sweep test above to genuinely GC-forced runs.
#[test]
fn closure_survives_case_branch_when_no_gc_pressure() {
    let tree = build_tree(100);
    let result = compile_and_run(&tree, 1 << 20);
    assert_ne!(result.result_ptr, host_fns::error_poison_ptr() as *const u8);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        assert_eq!(read_lit_int(result.result_ptr), 42);
    }
}
