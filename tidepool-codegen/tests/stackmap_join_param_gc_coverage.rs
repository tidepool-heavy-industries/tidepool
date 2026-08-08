//! Closes a PRE-EXISTING GC-coverage gap found while verifying cluster F
//! (D6, jit-chain-2): the GC gate (`gc_frame_walker`, `frame_walker_hardening`,
//! `continuation_gc_root`, `gc_fault_recovery`, `nested_child_gc_rooting`,
//! `gc_audit`, `gc_write_barrier`, `array_gc_safety`, `con_midfill_gc_safety`,
//! `nested_child_response_materialization_gc`, `heap_verify_lane`) had ZERO
//! tests that actually EXECUTE a `Join`/`Jump` under a forced collection.
//! `gc_audit::test_stack_map_join_safepoints` compiles a Join and asserts
//! `!pipeline.stack_maps.is_empty()` but never runs the program — a purely
//! structural check, not a runtime one.
//!
//! This is NOT a regression guard for the `declare_env` removal
//! (`emit/join.rs:154`/`199`, see that commit): `join.rs:163`'s
//! `declare_value_needs_stack_map(val) // CRITICAL` mark on the join block
//! param is untouched by that change — `declare_env` never uniquely covered
//! it, and the removal is safe by the function-wide-liveness contract
//! established from the pinned `cranelift-frontend` 0.129.1 source
//! (`declare_value_needs_stack_map` marks a Value once into a per-Function
//! set; Cranelift's own whole-function backward-liveness dataflow, not
//! `declare_env`, decides where it's live). This test exists because the
//! gap itself — no runtime proof that a join-point block param survives a
//! real collection — predates and is independent of that change, and a hole
//! this lane found should leave a test behind, not a paragraph.
//!
//! Program: `join j(p) = case Con(NOISE_TAG, [0..100)) of NOISE_TAG -> p in
//! jump j(42)`. `p` is bound to the join block's own Cranelift block param
//! (marked at `join.rs:163`, the value under test) when `jump j(42)` lands.
//! `rhs` immediately builds a 100-field noisy `Con` as its `Case` scrutinee
//! (forced eagerly by the hylomorphism, no thunk/DCE indirection to route
//! around), allocating enough to force a real GC while `p` is live only via
//! its join-block-param mark, then the matching `DataAlt` returns `p`
//! untouched. Swept nursery sizes, same rationale as
//! `apply_acceptance.rs`'s `apply_gc_during_application_relocates_forced_callee`.

use tidepool_codegen::host_fns;
use tidepool_heap::layout;
use tidepool_repr::*;
use tidepool_testing::jit_run::{compile_and_run, read_lit_int};

fn build_tree(n_noise_fields: i64) -> CoreExpr {
    let j = JoinId(1);
    let p = VarId(0x2001);
    let scrut_b = VarId(0x2002);
    let noise_tag = DataConId(1);

    let mut nodes = Vec::new();

    // rhs: case Con(NOISE_TAG, [0..n)) of NOISE_TAG -> p
    let p_ref = nodes.len();
    nodes.push(CoreFrame::Var(p));

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

    let rhs = nodes.len();
    nodes.push(CoreFrame::Case {
        scrutinee: noise_con,
        binder: scrut_b,
        alts: vec![Alt {
            con: AltCon::DataAlt(noise_tag),
            binders: vec![],
            body: p_ref,
        }],
    });

    // body: jump j(42)
    let lit_42 = nodes.len();
    nodes.push(CoreFrame::Lit(Literal::LitInt(42)));
    let jump = nodes.len();
    nodes.push(CoreFrame::Jump {
        label: j,
        args: vec![lit_42],
    });

    let join = nodes.len();
    nodes.push(CoreFrame::Join {
        label: j,
        params: vec![p],
        rhs,
        body: jump,
    });
    debug_assert_eq!(join, nodes.len() - 1, "root must be the last node");

    RecursiveTree { nodes }
}

/// The proof: `p` (the join block param, marked only at `join.rs:163`)
/// survives a real GC forced inside `rhs`'s scrutinee construction, under
/// both `TIDEPOOL_GC_POISON` and `TIDEPOOL_HEAP_VERIFY`.
#[test]
fn join_param_survives_gc_forced_in_rhs() {
    host_fns::set_gc_poison(true);
    host_fns::set_heap_verify(true);

    let mut any_gc_fired = false;
    for nursery_size in (256..=4096).step_by(48) {
        host_fns::reset_test_counters();
        let tree = build_tree(100);
        let result = compile_and_run(&tree, nursery_size);
        assert_ne!(
            result.result_ptr,
            host_fns::error_poison_ptr() as *const u8,
            "nursery_size={nursery_size}: jump j(42) short-circuited to the \
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
                "nursery_size={nursery_size}: join param p=42 must survive \
                 the GC rhs's noisy-Con scrutinee construction forces"
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

/// Sanity check on the harness itself: with no GC pressure, `p` obviously
/// survives regardless of marking — isolates the test above to genuinely
/// GC-forced runs.
#[test]
fn join_param_survives_when_no_gc_pressure() {
    let tree = build_tree(100);
    let result = compile_and_run(&tree, 1 << 20);
    assert_ne!(result.result_ptr, host_fns::error_poison_ptr() as *const u8);
    unsafe {
        assert_eq!(layout::read_tag(result.result_ptr), layout::TAG_LIT);
        assert_eq!(read_lit_int(result.result_ptr), 42);
    }
}
