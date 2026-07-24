//! Segment 40 — THE ADVERSARIAL SUITE: a parent suspended at a typed yield
//! (`Ask`) hosts nested CHILD fragment runs on the SAME machine while its stowed
//! continuation is a REGISTERED GC ROOT, and resumes correctly afterward.
//!
//! This is the memory-safety deliverable. Each test forces the exact exposure
//! the registered root defends against, deterministically, and deep-verifies the
//! resumed continuation's captured values. Poison + verify knobs
//! (`TIDEPOOL_GC_POISON`/`TIDEPOOL_HEAP_VERIFY`) turn a missed-root into a
//! deterministic failure rather than a timing-dependent SIGSEGV.
//!
//! Coverage (spec step 5):
//!   (a) child allocates until GC fires with a live suspended parent, then the
//!       parent resumes and the continuation's captured value deep-verifies;
//!   (b) child triggers heap DOUBLING mid-run, parent resumes;
//!   (c) child defines new decls (`add_function`) then the parent resumes and
//!       module accretion is inert for the parent;
//!   (d) `resume`-with-a-bottom answer does NOT consume the continuation;
//!   (e) nested-mode misuse (a plain run entry while suspended) errors cleanly;
//!   plus the value-plane tenure-across-suspend: a parent binds a value, suspends,
//!   a child forces GC, the parent resumes and reads the binding.

use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::{JitEffectMachine, ResumeInput, SuspendableOutcome};
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_effect::dispatch::EffectContext;
use tidepool_effect::error::EffectError;
use tidepool_effect::Response;
use tidepool_eval::value::Value;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::*;
use tidepool_repr::{CoreExpr, Literal, TreeBuilder};

use serial_test::serial;

#[path = "support/session_scaffold.rs"]
mod session_scaffold;
use session_scaffold::{build_gc_forcing_fragment, build_reference_fragment, build_value_fragment};
use session_scaffold::{expect_int, C1};

// ─── freer-simple constructor IDs (must match ConTags::from_table lookup) ────
const VAL_ID: DataConId = DataConId(10);
const E_ID: DataConId = DataConId(11);
const UNION_ID: DataConId = DataConId(12);
const LEAF_ID: DataConId = DataConId(13);
const NODE_ID: DataConId = DataConId(14);

/// The `Ask` union tag the suspend driver intercepts. (0 in our synthetic
/// stack — the Union's first slot is `W# 0`.)
const ASK_TAG: u64 = 0;

/// A 2-field pair constructor used to make the resumed continuation's result a
/// deeply-verifiable structure: `Pair captured answerWrapped`.
const PAIR_ID: DataConId = DataConId(2);

fn adversarial_table() -> DataConTable {
    let mut table = DataConTable::new();
    // Payload + pair constructors.
    table.insert(DataCon {
        id: C1,
        name: "C1".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    table.insert(DataCon {
        id: PAIR_ID,
        name: "Pair".to_string(),
        tag: 2,
        rep_arity: 2,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    // freer-simple constructors (ConTags resolves by qualified name).
    for (id, name, qual, arity) in [
        (VAL_ID, "Val", "Control.Monad.Freer.Val", 1u32),
        (E_ID, "E", "Control.Monad.Freer.E", 2),
        (UNION_ID, "Union", "Data.OpenUnion.Union", 2),
        (LEAF_ID, "Leaf", "Data.FTCQueue.Leaf", 1),
        (NODE_ID, "Node", "Data.FTCQueue.Node", 2),
    ] {
        table.insert(DataCon {
            id,
            name: name.to_string(),
            tag: 0,
            rep_arity: arity,
            field_bangs: vec![],
            qualified_name: Some(qual.to_string()),
            type_name: String::new(),
        });
    }
    table
}

/// Build the SUSPENDING parent entry:
///
/// ```text
/// let captured = C1 CAPTURED_N in
///   E (Union (W# ASK_TAG) (I# req)) (Leaf (\v -> Val (Pair captured (C1 v))))
/// ```
///
/// Driven via `run_suspendable(ASK_TAG)`, the loop sees the effect tag ==
/// ASK_TAG and SUSPENDS, stowing the continuation `\v -> Val (Pair captured
/// (C1 v))`. The continuation CLOSES OVER `captured` — a heap object allocated
/// BEFORE the suspension — so a resume after the child has forced GC must read
/// both the (relocated) captured value and the answer correctly.
fn build_suspending_parent(captured_n: i64, req: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();

    // captured = C1 captured_n
    let cap_lit = b.push(CoreFrame::Lit(Literal::LitInt(captured_n)));
    let captured = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![cap_lit],
    });

    // \v -> Val (Pair captured (C1 v))
    let var_v = b.push(CoreFrame::Var(VarId(0)));
    let c1_v = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![var_v],
    });
    let var_captured = b.push(CoreFrame::Var(VarId(1))); // the let-bound captured
    let pair = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![var_captured, c1_v],
    });
    let val = b.push(CoreFrame::Con {
        tag: VAL_ID,
        fields: vec![pair],
    });
    let lam = b.push(CoreFrame::Lam {
        binder: VarId(0),
        body: val,
    });
    let leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![lam],
    });

    // Union (W# ASK_TAG) (I# req)
    let tag_word = b.push(CoreFrame::Lit(Literal::LitWord(ASK_TAG)));
    let request = b.push(CoreFrame::Lit(Literal::LitInt(req)));
    let union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![tag_word, request],
    });
    let e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![union, leaf],
    });

    // let captured = ... in E ...
    b.push(CoreFrame::LetNonRec {
        binder: VarId(1),
        rhs: captured,
        body: e,
    });
    b.build()
}

/// A handler that never actually runs for the ask tag (we suspend before
/// dispatch), but the drive loop needs a `DispatchEffect`. Panics if called —
/// reaching dispatch means the suspend branch was NOT taken (a regression).
struct NoDispatch;
impl DispatchEffect<()> for NoDispatch {
    fn dispatch(
        &mut self,
        tag: u64,
        _request: &Value,
        _cx: &EffectContext<'_, ()>,
    ) -> Result<Response, EffectError> {
        panic!("handler dispatched tag {tag} — the ask should have suspended instead");
    }
}

/// Deep-verify the resumed continuation's result: `Pair (C1 captured) (C1
/// answer)`. This walks the WHOLE structure — a missed/dangling root would land
/// a garbage tag or wrong payload here.
fn assert_pair_result(v: &Value, expect_captured: i64, expect_answer: i64) {
    match v {
        Value::Con(id, fields) if id.0 == PAIR_ID.0 && fields.len() == 2 => {
            assert_eq!(
                expect_int(&fields[0]),
                expect_captured,
                "captured value (closed over BEFORE the suspension) must survive child GC"
            );
            assert_eq!(
                expect_int(&fields[1]),
                expect_answer,
                "resumed answer must be threaded through the continuation"
            );
        }
        other => panic!("expected Pair(C1 captured, C1 answer), got {other:?}"),
    }
}

/// Drive `build_suspending_parent` to its suspension and return the machine
/// parked at the ask (its continuation stowed). Uses a per-test nursery size.
fn suspend_parent(
    table: &DataConTable,
    nursery: usize,
    captured_n: i64,
    req: i64,
) -> JitEffectMachine {
    let entry = build_suspending_parent(captured_n, req);
    let mut machine =
        JitEffectMachine::compile_session(&entry, table, nursery).expect("compile_session parent");
    let mut handler = NoDispatch;
    let outcome = machine
        .run_suspendable(table, &mut handler, &(), ASK_TAG)
        .expect("parent run_suspendable");
    match outcome {
        SuspendableOutcome::Suspended { request } => {
            // The bridged ask request carries the I# req literal.
            assert_eq!(
                expect_int(&request),
                req,
                "the suspension request must carry the ask payload"
            );
        }
        SuspendableOutcome::Completed(_) => panic!("parent should suspend at the ask, not complete"),
    }
    assert!(machine.is_suspended(), "machine must be suspended after the ask");
    assert_eq!(
        machine.stowed_roots_count(),
        0,
        "no stowed root while idle-suspended (registered only during a child run)"
    );
    machine
}

// ───────────────────────────────────────────────────────────────────────────
// (a) Child allocates until GC fires with a live suspended parent, then resume
//     and deep-verify the continuation's captured value.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn child_gc_then_parent_resumes_and_captured_survives() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            // Poison from-space so a missed root is a deterministic bad tag.
            tidepool_codegen::host_fns::set_gc_poison(true);
            tidepool_codegen::host_fns::set_heap_verify(true);
            tidepool_codegen::host_fns::reset_test_counters();

            let table = adversarial_table();
            // Tiny 2 KiB nursery + a deep filler so the child fragment forces a
            // real collection while the parent's continuation is stowed.
            let mut machine = suspend_parent(&table, 2048, 777, 5);

            // Run a CHILD fragment that heavily allocates → forces GC. The
            // parent's continuation is GC-rooted for the child's duration.
            let gc_before = tidepool_codegen::host_fns::gc_trigger_call_count();
            let child = machine
                .add_function("gc_child", &build_gc_forcing_fragment(150), &table, &ExternalEnv::new())
                .expect("add child fragment");
            // While the child runs, the stowed root is registered.
            let _ = machine
                .run_child_fragment_pure(child)
                .expect("child fragment runs against the suspended parent");
            let gc_after = tidepool_codegen::host_fns::gc_trigger_call_count();
            assert!(
                gc_after > gc_before,
                "the child must have forced at least one real GC while the parent \
                 was suspended (before={gc_before}, after={gc_after})"
            );

            // The child completed: the stowed root is deregistered again.
            assert_eq!(
                machine.stowed_roots_count(),
                0,
                "stowed root must be deregistered after the child completes"
            );
            assert!(machine.is_suspended(), "parent stays suspended across the child run");

            // Resume the parent with answer 5 and deep-verify: the continuation
            // must produce Pair(C1 777, C1 5) — captured=777 survived the child
            // GC (evacuated via the stowed root), answer=5 threaded through.
            let out = machine
                .resume_suspended(&table, &mut NoDispatch, &(), ASK_TAG, ResumeInput::Answer(Value::Lit(Literal::LitInt(5))))
                .expect("parent resumes after the child GC");
            match out {
                SuspendableOutcome::Completed(v) => assert_pair_result(&v, 777, 5),
                SuspendableOutcome::Suspended { .. } => panic!("resume should complete"),
            }

            tidepool_codegen::host_fns::set_gc_poison(false);
            tidepool_codegen::host_fns::set_heap_verify(false);
            drop(machine);
        })
        .unwrap()
        .join()
        .unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// (b) Child triggers heap DOUBLING mid-run, parent resumes.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn child_heap_doubling_then_parent_resumes() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tidepool_codegen::host_fns::set_gc_poison(true);
            tidepool_codegen::host_fns::reset_test_counters();

            let table = adversarial_table();
            // Tiny 2 KiB nursery + a deep filler => the child's live set stays
            // high after the first Cheney pass, tripping the doubling re-evacuate
            // (live*4 > size*3) — the same path continuation_gc_root.rs exercises.
            let mut machine = suspend_parent(&table, 2048, 12345, 9);

            let gc_before = tidepool_codegen::host_fns::gc_trigger_call_count();
            let child = machine
                .add_function("doubling_child", &build_gc_forcing_fragment(200), &table, &ExternalEnv::new())
                .expect("add doubling child");
            let _ = machine
                .run_child_fragment_pure(child)
                .expect("doubling child runs against the suspended parent");
            assert!(
                tidepool_codegen::host_fns::gc_trigger_call_count() > gc_before,
                "the doubling child must fire real GCs"
            );

            // Resume: captured=12345 survived doubling (each doubling pass
            // re-evacuates and re-updates every root, including the stowed one).
            let out = machine
                .resume_suspended(&table, &mut NoDispatch, &(), ASK_TAG, ResumeInput::Answer(Value::Lit(Literal::LitInt(9))))
                .expect("parent resumes after child heap doubling");
            match out {
                SuspendableOutcome::Completed(v) => assert_pair_result(&v, 12345, 9),
                SuspendableOutcome::Suspended { .. } => panic!("resume should complete"),
            }

            tidepool_codegen::host_fns::set_gc_poison(false);
            drop(machine);
        })
        .unwrap()
        .join()
        .unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// (c) Child defines new decls (add_function) then the parent resumes — module
//     accretion is inert for the parent.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn child_decl_accretion_is_inert_for_parent() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let table = adversarial_table();
            let mut machine = suspend_parent(&table, 1 << 14, 4242, 8);

            // Accrete SEVERAL child decls and run each, all while the parent is
            // suspended. Each is a fresh FuncId in the live module.
            for i in 0..4i64 {
                let child = machine
                    .add_function(
                        &format!("accrete_{i}"),
                        &build_value_fragment(1000 + i),
                        &table,
                        &ExternalEnv::new(),
                    )
                    .expect("add accretion child");
                let r = machine
                    .run_child_fragment_pure(child)
                    .expect("accretion child runs");
                assert_eq!(expect_int(&r), 1000 + i, "each child computes its own value");
                assert!(machine.is_suspended(), "parent stays suspended across accretion");
            }

            // The parent resumes: its continuation is untouched by the accreted
            // module functions (module accretion is inert for the parent).
            let out = machine
                .resume_suspended(&table, &mut NoDispatch, &(), ASK_TAG, ResumeInput::Answer(Value::Lit(Literal::LitInt(8))))
                .expect("parent resumes after child decl accretion");
            match out {
                SuspendableOutcome::Completed(v) => assert_pair_result(&v, 4242, 8),
                SuspendableOutcome::Suspended { .. } => panic!("resume should complete"),
            }
            drop(machine);
        })
        .unwrap()
        .join()
        .unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// (d) A bottom answer does NOT consume the continuation (A5 NF-force).
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn bottom_answer_does_not_consume_the_continuation() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let table = adversarial_table();
            let mut machine = suspend_parent(&table, 1 << 14, 555, 3);
            assert!(machine.is_suspended());

            // A "bottom" answer: a Value carrying an unforced thunk reference
            // (the shape a not-fully-forced ⊥ would take). The NF-force must
            // REJECT it WITHOUT consuming the continuation.
            let bottom = Value::Con(
                PAIR_ID,
                vec![
                    Value::Lit(Literal::LitInt(1)),
                    Value::ThunkRef(tidepool_eval::value::ThunkId(0)),
                ],
            );
            let err = match machine.resume_suspended(
                &table,
                &mut NoDispatch,
                &(),
                ASK_TAG,
                ResumeInput::Answer(bottom),
            ) {
                Ok(_) => panic!("a bottom answer must be rejected, not accepted"),
                Err(e) => e,
            };
            assert!(
                format!("{err}").contains("normal form") || format!("{err}").contains("bottom"),
                "rejection must name the NF/bottom cause, got: {err}"
            );

            // CRUCIAL: the continuation was NOT consumed — the machine is still
            // suspended and a VALID answer now resumes correctly.
            assert!(
                machine.is_suspended(),
                "a rejected bottom answer must leave the continuation stowed"
            );
            let out = machine
                .resume_suspended(&table, &mut NoDispatch, &(), ASK_TAG, ResumeInput::Answer(Value::Lit(Literal::LitInt(3))))
                .expect("a valid answer resumes after a rejected bottom");
            match out {
                SuspendableOutcome::Completed(v) => assert_pair_result(&v, 555, 3),
                SuspendableOutcome::Suspended { .. } => panic!("resume should complete"),
            }
            drop(machine);
        })
        .unwrap()
        .join()
        .unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// (e) Nested-mode misuse: a PLAIN run entry while suspended errors cleanly
//     (the L7 asserts stay intact — the illegal unregistered case still panics).
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn plain_run_entry_while_suspended_still_panics() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let table = adversarial_table();
            let mut machine = suspend_parent(&table, 1 << 14, 1, 1);
            assert!(machine.is_suspended());

            // A plain (NON-child) fragment run while suspended must panic — the
            // continuation is stowed and UNregistered; running a plain entry
            // would be the illegal state the L7 asserts reject.
            let frag = machine
                .add_function("plain_while_suspended", &build_value_fragment(0), &table, &ExternalEnv::new())
                .expect("add plain fragment");
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = machine.run_fragment_pure(frag);
            }));
            assert!(
                r.is_err(),
                "a plain run entry while suspended must panic (L7 assert intact)"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// Value-plane tenure across a suspension (segment-20-deferred): parent binds a
// value → suspends → child forces GC → parent resumes and reads the binding.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn value_plane_binding_survives_suspend_child_gc_resume() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tidepool_codegen::host_fns::set_gc_poison(true);
            tidepool_codegen::host_fns::reset_test_counters();

            let table = adversarial_table();

            // 1. Bootstrap a session machine with the SUSPENDING entry (so its
            //    ConTags seed and its run is the suspendable one).
            let entry = build_suspending_parent(9090, 6);
            let mut machine = JitEffectMachine::compile_session(&entry, &table, 2048)
                .expect("compile_session");

            // 2. BEFORE suspending, bind a value into old-space via the value
            //    plane (tenure → RootSlot → persistent root).
            let bind_frag = machine
                .add_function("bind_v", &build_value_fragment(31337), &table, &ExternalEnv::new())
                .expect("add bind fragment");
            let slot = machine.run_pure_and_bind(bind_frag).expect("bind the value");
            assert_eq!(machine.persistent_roots_count(), 1);
            let tenured_before = unsafe { slot.current() };

            // 3. Suspend the parent at the ask.
            let out = machine
                .run_suspendable(&table, &mut NoDispatch, &(), ASK_TAG)
                .expect("parent suspends");
            assert!(matches!(out, SuspendableOutcome::Suspended { .. }));
            assert!(machine.is_suspended());

            // 4. A child forces GC while the parent is suspended AND the bound
            //    value is tenured. Both the tenured value (persistent root) and
            //    the stowed continuation (stowed root) must survive.
            let gc_before = tidepool_codegen::host_fns::gc_trigger_call_count();
            let child = machine
                .add_function("v_gc_child", &build_gc_forcing_fragment(120), &table, &ExternalEnv::new())
                .expect("add child");
            let _ = machine.run_child_fragment_pure(child).expect("child GC run");
            assert!(
                tidepool_codegen::host_fns::gc_trigger_call_count() > gc_before,
                "child must force a real GC"
            );

            // Tenured value is in old-space (outside the minor-GC from-range), so
            // its slot address is unchanged; the persistent root survived.
            assert_eq!(unsafe { slot.current() }, tenured_before, "tenured value not relocated by minor GC");
            assert_eq!(machine.persistent_roots_count(), 1, "persistent root survives the child GC");

            // 5. Resume the parent; then read the binding through an ExternalEnv
            //    fragment against the retained heap — proving the value plane
            //    survived the whole suspend/child-GC/resume round-trip.
            let out = machine
                .resume_suspended(&table, &mut NoDispatch, &(), ASK_TAG, ResumeInput::Answer(Value::Lit(Literal::LitInt(6))))
                .expect("parent resumes");
            match out {
                SuspendableOutcome::Completed(v) => assert_pair_result(&v, 9090, 6),
                SuspendableOutcome::Suspended { .. } => panic!("resume should complete"),
            }
            assert!(!machine.is_suspended(), "parent idle after resume");

            let x = VarId((0xFEu64 << 56) | 0x9090);
            let mut env = ExternalEnv::new();
            env.insert(x, slot.addr());
            let read_frag = machine
                .add_function("read_v", &build_reference_fragment(x), &table, &env)
                .expect("add read fragment");
            let read = machine.run_fragment_pure(read_frag).expect("read the binding post-resume");
            assert_eq!(
                expect_int(&read),
                31337,
                "the value-plane binding must survive suspend → child GC → resume"
            );

            tidepool_codegen::host_fns::set_gc_poison(false);
            drop(machine);
        })
        .unwrap()
        .join()
        .unwrap();
}
