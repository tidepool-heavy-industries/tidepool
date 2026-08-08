//! Acceptance coverage for the `runtime_apply`/`runtime_tail_apply` refactor
//! (cluster D, jit-chain-2). New tests here fill gaps in the pre-existing
//! suite for the non-tail application protocol specifically; see the
//! refactor's commit messages for the full acceptance table mapping all
//! seven cases (including the ones already covered by pre-existing tests
//! such as `emit_expr.rs::test_adversarial_thunked_closure_in_app_fun` and
//! `tco.rs`/`tco_advanced.rs`) to their covering tests.
//!
//! `compile_and_run`/`compile_expr`'s top-level root is always emitted
//! `TailCtx::NonTail` (see `compile_expr` in `emit/expr.rs`), so every test in
//! this file exercises `runtime_apply`, not `runtime_tail_apply`.

use tidepool_codegen::context::VMContext;
use tidepool_codegen::emit::expr::compile_expr;
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::host_fns;
use tidepool_codegen::machine_state::MachineState;
use tidepool_codegen::pipeline::CodegenPipeline;
use tidepool_heap::layout;
use tidepool_repr::*;
use tidepool_testing::jit_run::compile_and_run;

/// (c) PARTIAL application: applying a curried 2-arg function to only ONE
/// argument must return a Closure (capturing the first arg), not attempt to
/// call further or misinterpret the result.
#[test]
fn apply_partial_application_returns_closure() {
    let x = VarId(1);
    let y = VarId(2);
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Var(x), // 0
            CoreFrame::Var(y), // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            }, // 2: x + y
            CoreFrame::Lam { binder: y, body: 2 }, // 3: \y -> x + y
            CoreFrame::Lam { binder: x, body: 3 }, // 4: \x -> \y -> x + y
            CoreFrame::Lit(Literal::LitInt(5)), // 5
            CoreFrame::App { fun: 4, arg: 5 }, // 6: (\x -> \y -> x+y) 5 (root)
        ],
    };
    let result = compile_and_run(&tree, 65536);
    unsafe {
        assert_eq!(
            layout::read_tag(result.result_ptr),
            layout::TAG_CLOSURE,
            "applying a curried 2-arg function to 1 arg must yield a Closure \
             (capturing x=5), not fully evaluate or crash"
        );
    }
}

/// (d) runtime-error poison: applying a non-Closure value (a bare boxed Int)
/// as if it were a function must short-circuit through `debug_app_check`'s
/// poison path — never dereference `CLOSURE_CODE_PTR_OFFSET` on garbage and
/// `call_indirect` through it.
#[test]
fn apply_poison_short_circuits_application_of_non_closure() {
    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(5)), // 0: "fun" — not a closure!
            CoreFrame::Lit(Literal::LitInt(3)), // 1: arg
            CoreFrame::App { fun: 0, arg: 1 },  // 2: 5 3 (root) — ill-typed
        ],
    };
    let result = compile_and_run(&tree, 65536);
    assert_eq!(
        result.result_ptr,
        host_fns::error_poison_ptr() as *const u8,
        "applying a non-Closure value must short-circuit to the poison \
         pointer via debug_app_check, never call_indirect through it"
    );
}

/// High-byte tag mimicking a real session external id (see
/// `external_env_resolution.rs`'s `EXTERNAL_TAG`); only used here as a
/// resolvable-but-distinguishable `VarId` for a hand-seeded binding.
const EXTERNAL_TAG: u64 = 0xFE;

fn external_var_id(key: u64) -> VarId {
    VarId((EXTERNAL_TAG << 56) | (key & ((1u64 << 56) - 1)))
}

/// Stands in for a compiled closure whose call returns null (0) without ever
/// engaging the tail-call protocol (`VMContext.tail_callee` stays unset) —
/// the `null_propagate_block` hazard (acceptance case (f)). A real compiled
/// Core program can't produce this from the outside (a genuine result is
/// always a nonzero heap pointer; only the tail-call trampoline legitimately
/// returns literal null, and only ever WITH `tail_callee` set), so this
/// hand-builds a closure whose code pointer is a bogus host fn — exactly the
/// same "hand-built heap object standing in for a compiled value" technique
/// `external_env_resolution.rs` uses for its `boxed_int` fixture, applied to
/// a Closure instead of a Lit.
unsafe extern "C" fn bogus_callee_returns_null_without_tail(
    _vmctx: *mut VMContext,
    _self_ptr: *mut u8,
    _arg: *mut u8,
) -> i64 {
    0
}

fn hand_built_closure_returning_null_without_tail() -> (Vec<u64>, *mut u8) {
    // 4 x u64 = 32 bytes: header(8) + code_ptr(8) + num_captured(8, over-wide
    // but harmless — only the tag byte and CLOSURE_CODE_PTR_OFFSET are read
    // by debug_app_check/call_indirect) + captured slot headroom.
    let mut storage = vec![0u64; 4];
    let ptr = storage.as_mut_ptr() as *mut u8;
    unsafe {
        tidepool_heap::layout::write_header(ptr, layout::TAG_CLOSURE, 32);
        *(ptr.add(layout::CLOSURE_CODE_PTR_OFFSET) as *mut usize) =
            bogus_callee_returns_null_without_tail as *const () as usize;
    }
    (storage, ptr)
}

fn compile_then_run(tree: &CoreExpr, env: &ExternalEnv) -> *const u8 {
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols()).unwrap();
    let func_id =
        compile_expr(&mut pipeline, tree, "apply_null_test_fn", env).expect("compile_expr failed");
    pipeline.finalize().expect("failed to finalize");

    let mut nursery = vec![0u8; 65536];
    let start = nursery.as_mut_ptr();
    let end = unsafe { start.add(nursery.len()) };
    let mut vmctx = VMContext::new(start, end, host_fns::gc_trigger);
    let machine_state = Box::new(MachineState::new());
    vmctx.machine_state = machine_state.as_ref() as *const MachineState as *mut MachineState;
    machine_state.set_gc_state(start, nursery.len());
    machine_state.set_stack_map_registry(&pipeline.stack_maps);

    let fp = pipeline.get_function_ptr(func_id);
    let func: unsafe extern "C" fn(*mut VMContext) -> i64 = unsafe { std::mem::transmute(fp) };
    let result = unsafe { func(&mut vmctx as *mut VMContext) };
    drop(nursery);
    result as *const u8
}

/// (f) null return WITHOUT a pending tail call: `runtime_apply`'s
/// `null_check_block` must distinguish "callee returned null because it
/// queued a tail call" (`tail_callee` set — resolve via the trampoline) from
/// "callee returned null and nothing is pending" (`null_propagate_block` —
/// propagate the null itself, the error case). This pins the second, easy-to
/// -lose branch.
#[test]
fn apply_null_without_pending_tail_propagates_as_null() {
    let (storage, closure_ptr) = hand_built_closure_returning_null_without_tail();
    let mut slot: *mut u8 = closure_ptr;
    let slot_addr: *mut *mut u8 = std::ptr::addr_of_mut!(slot);

    let var_id = external_var_id(0x9001);
    let mut env = ExternalEnv::new();
    env.insert(var_id, slot_addr);

    let tree = RecursiveTree {
        nodes: vec![
            CoreFrame::Var(var_id),             // 0: the hand-built closure
            CoreFrame::Lit(Literal::LitInt(0)), // 1: arg (ignored by the bogus callee)
            CoreFrame::App { fun: 0, arg: 1 },  // 2 (root)
        ],
    };

    let result = compile_then_run(&tree, &env);
    assert!(
        result.is_null(),
        "a callee that returns 0 without setting vmctx.tail_callee must \
         propagate as a literal null through null_propagate_block — not be \
         mistaken for a resolvable tail call, and not crash"
    );
    drop(storage);
}

/// (g) GC occurring DURING application: force a GC to fire while
/// `runtime_apply` is mid-sequence (forcing a thunked function value, whose
/// forcing itself allocates a fresh Closure, under a nursery too small to
/// hold the noise + that closure at once). If `force_and_check_callee`'s
/// `declare_value_needs_stack_map` calls on `forced_fun`/`fun_ptr` (or
/// `runtime_apply`'s on `merged_val`) were dropped or misplaced by the
/// refactor, a GC landing here would relocate the closure without updating
/// the value the compiled code goes on to `call_indirect` through —
/// corruption or a wrong result, not a clean trap.
#[test]
fn apply_gc_during_application_relocates_forced_callee() {
    // The real payload: identical shape to
    // emit_expr.rs::test_adversarial_thunked_closure_in_app_fun (a thunked
    // App(identity, closure) Con field, forced via App in the case alt body,
    // whose forcing allocates the \y -> y+10 closure fresh) — exercises
    // heap_force triggering an allocation (the closure) mid-force.
    let mut nodes = Vec::new();
    let box_tag = DataConId(1);
    let x = VarId(0x100);
    let y = VarId(0x200);
    let f = VarId(0x300);
    let scrut_b = VarId(0x400);

    let identity_var = nodes.len();
    nodes.push(CoreFrame::Var(x));
    let identity_lam = nodes.len();
    nodes.push(CoreFrame::Lam {
        binder: x,
        body: identity_var,
    });
    let y_var = nodes.len();
    nodes.push(CoreFrame::Var(y));
    let ten_lit = nodes.len();
    nodes.push(CoreFrame::Lit(Literal::LitInt(10)));
    let y_plus_10 = nodes.len();
    nodes.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![y_var, ten_lit],
    });
    let closure_lam = nodes.len();
    nodes.push(CoreFrame::Lam {
        binder: y,
        body: y_plus_10,
    });
    let app_identity_closure = nodes.len();
    nodes.push(CoreFrame::App {
        fun: identity_lam,
        arg: closure_lam,
    });
    let box_con = nodes.len();
    nodes.push(CoreFrame::Con {
        tag: box_tag,
        fields: vec![app_identity_closure],
    });
    let f_var = nodes.len();
    nodes.push(CoreFrame::Var(f));
    let five_lit = nodes.len();
    nodes.push(CoreFrame::Lit(Literal::LitInt(5)));
    let f_app_5 = nodes.len();
    nodes.push(CoreFrame::App {
        fun: f_var,
        arg: five_lit,
    });
    let real_case = nodes.len();
    nodes.push(CoreFrame::Case {
        scrutinee: box_con,
        binder: scrut_b,
        alts: vec![Alt {
            con: AltCon::DataAlt(box_tag),
            binders: vec![f],
            body: f_app_5,
        }],
    });

    // Noise AFTER the App call, as a SIBLING PrimOp argument rather than
    // through a Case/Let binder: `IntAdd(real_case, noise_case)`. A PrimOp
    // argument is a bare SSA value passed straight through the hylomorphism
    // with no env insertion, so `real_case`'s result (15) is live ONLY
    // because of `runtime_apply`'s own stack-map declares (its
    // force/call-result marks) while its sibling operand (`noise_case`)
    // builds a large noisy Con — forcing a GC that must relocate it correctly
    // with no other creation-site mark to fall back on.
    let noise_tag = DataConId(2);
    let n_noise_fields = 100;
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
    let noise_result_binder = VarId(0x998);
    let zero_lit = nodes.len();
    nodes.push(CoreFrame::Lit(Literal::LitInt(0)));
    let noise_case = nodes.len();
    nodes.push(CoreFrame::Case {
        scrutinee: noise_con,
        binder: noise_result_binder,
        alts: vec![Alt {
            con: AltCon::Default,
            binders: vec![],
            body: zero_lit,
        }],
    });
    // Build BOTH operand orderings — the hylomorphism's actual sibling
    // evaluation order (`real_case` before `noise_case`, or vice versa) isn't
    // pinned by anything this test should assume, so cover both rather than
    // risk silently testing only the order that happens not to straddle the
    // hazard.
    let mut nodes_app_first = nodes.clone();
    nodes_app_first.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![real_case, noise_case],
    });
    let mut nodes_noise_first = nodes;
    nodes_noise_first.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![noise_case, real_case],
    });

    // Sweep nursery sizes rather than hand-computing the exact bump-allocator
    // offset at which the noise Con's construction (which runs on one side of
    // real_case's result being already in hand, as the sibling PrimOp
    // operand) straddles a GC — mirrors proptest_gc_recursion.rs's rationale
    // for sweeping nursery sizes instead of pinning one exact value.
    for nodes in [nodes_app_first, nodes_noise_first] {
        let tree = RecursiveTree { nodes };
        for nursery_size in (256..=4096).step_by(48) {
            let result = compile_and_run(&tree, nursery_size);
            unsafe {
                assert_eq!(
                    layout::read_tag(result.result_ptr),
                    layout::TAG_LIT,
                    "nursery_size={nursery_size}"
                );
                assert_eq!(
                    tidepool_testing::jit_run::read_lit_int(result.result_ptr),
                    15,
                    "thunked App(identity, \\y->y+10) applied to 5 = 15, added \
                     to 0 after its sibling PrimOp operand builds a \
                     {n_noise_fields}-field noisy Con (nursery_size=\
                     {nursery_size}) — a stale (pre-GC) merged_val pointer \
                     would read garbage or crash here"
                );
            }
        }
    }
}
