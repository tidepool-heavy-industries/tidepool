//! Prior bug: `run_pure_and_bind`'s error paths (bare `?` / `return Err`)
//! used to bypass `_guard.arm_reclaim(...)`. `RegistryGuard::drop` with
//! `reclaim: None` frees the retained session buffer (`clear_run_scratch`)
//! but never writes back `session.heap`/`session.cursor` — so
//! `session.cursor` is left pointing at whatever high-water mark a PRIOR
//! (possibly GC-grown) buffer had reached, while `session.heap` stays
//! `None`. The next run's `install_registries` then falls back to the
//! machine's original, small, fixed-size `Nursery` buffer, and
//! `make_session_vmctx` computes
//! `alloc_ptr = nursery.start() + <stale, possibly much larger> cursor` — an
//! out-of-bounds allocation pointer.
//!
//! A session machine driven turn-by-turn via `add_function` + a bind
//! primitive, with a reference fragment reading back a tenured value to
//! prove correctness: an ordinary, `Reusable`-disposition runtime error in a
//! bind turn (division by zero, not a test bug and not a heap-shape
//! integrity failure), immediately followed by an allocating turn, which
//! must succeed cleanly (no crash, correct result) rather than computing a
//! bad alloc pointer from a stale cursor.

use serial_test::serial;
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::host_fns::{heap_verify_run_count, set_heap_verify};
use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_repr::datacon::DataCon;
use tidepool_repr::types::{Alt, AltCon, DataConId, Literal, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, DataConTable, PrimOpKind, TreeBuilder};

use crate::session_scaffold;
use crate::session_scaffold_expect;
use crate::session_scaffold_gc_forcing;
use crate::session_scaffold_reference;
use crate::session_scaffold_value;
use session_scaffold::C1;
use session_scaffold_expect::expect_int;
use session_scaffold_gc_forcing::build_gc_forcing_fragment;
use session_scaffold_reference::build_reference_fragment;
use session_scaffold_value::build_value_fragment;

fn table() -> DataConTable {
    let mut table = DataConTable::new();
    table.insert(DataCon {
        id: C1,
        name: "C1".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    table
}

/// `1 \`quot\` 0` — a genuine runtime division-by-zero
/// (`RuntimeError::DivisionByZero`), not a compile-time or test-construction
/// error. Deliberately NOT a case-miss trap (`RuntimeError::CaseTrap`): since
/// `71f23ffda` (2026-09-12), `CaseTrap` maps to `MachineDisposition::Unavailable`
/// (`host_fns/errors.rs`, "shape ... failures mean the live machine can no
/// longer prove heap integrity"), which makes the machine permanently refuse
/// further `add_function` calls -- a genuinely different, and correct,
/// contract this test must not fight. `DivisionByZero` stays `Reusable`
/// (an ordinary runtime error, not a heap-shape integrity failure), matching
/// `head []`'s `PatternMatchFailure`/`UserError` shape and this test's actual
/// target: proving `run_pure_and_bind`'s error path still arms
/// `_guard.arm_reclaim` so the FOLLOWING bind turn computes a correct
/// `alloc_ptr`, not whether the machine survives an integrity failure.
fn build_error_fragment() -> CoreExpr {
    let mut b = TreeBuilder::new();
    let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    let zero = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntQuot,
        args: vec![one, zero],
    });
    b.build()
}

/// `x <- pure (1 \`quot\` 0)` (error turn) followed by an allocating turn:
/// the error turn must fail cleanly (not crash, not corrupt session state),
/// and the FOLLOWING bind turn must succeed with a correct value.
#[test]
#[serial]
fn error_bind_turn_then_allocating_bind_turn_stays_sane() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tidepool_codegen::host_fns::reset_test_counters();
            set_heap_verify(true);
            let verify_before = heap_verify_run_count();
            let table = table();

            // Tiny 2 KiB nursery.
            let dummy = build_value_fragment(0);
            let mut machine =
                JitEffectMachine::compile_session(&dummy, &table, 2048).expect("compile_session");

            // Turn 1 (success): force a real GC (likely heap growth) so
            // `session.cursor` reflects a grown buffer's high-water mark, not
            // the original small nursery's.
            let gc_before = tidepool_codegen::host_fns::gc_trigger_call_count();
            let filler = machine
                .add_function(
                    "filler",
                    &build_gc_forcing_fragment(80),
                    &table,
                    &ExternalEnv::new(),
                )
                .expect("add_function filler");
            let _ = machine.run_fragment_pure(filler).expect("run filler");
            let gc_after = tidepool_codegen::host_fns::gc_trigger_call_count();
            assert!(
                gc_after > gc_before,
                "filler fragment must have triggered at least one real GC \
                 (before={gc_before}, after={gc_after})"
            );

            // Turn 2 (error): a genuine runtime division-by-zero in a BIND
            // turn — the exact function under test, `run_pure_and_bind`.
            let err_frag = machine
                .add_function(
                    "err_turn",
                    &build_error_fragment(),
                    &table,
                    &ExternalEnv::new(),
                )
                .expect("add_function err_turn");
            let err_result = machine.run_pure_and_bind(err_frag);
            assert!(
                err_result.is_err(),
                "error turn must fail cleanly, got {err_result:?}"
            );
            assert_eq!(
                machine.persistent_roots_count(),
                0,
                "a failed bind turn must not have tenured/rooted anything"
            );

            // Turn 3 (must stay sane): an ordinary allocating bind turn right
            // after the error. Pre-fix, `arm_reclaim` was skipped on turn 2's
            // error path, leaving `session.cursor` stale against the
            // original (small) nursery — this turn would compute an
            // out-of-bounds `alloc_ptr` and crash or corrupt the heap.
            let good_frag = machine
                .add_function(
                    "good_turn",
                    &build_value_fragment(42),
                    &table,
                    &ExternalEnv::new(),
                )
                .expect("add_function good_turn");
            let slot = machine
                .run_pure_and_bind(good_frag)
                .expect("bind turn immediately after an error turn must succeed");
            assert_eq!(
                machine.persistent_roots_count(),
                1,
                "the successful bind turn must register exactly one persistent root"
            );

            // Read back through a reference fragment to prove the tenured
            // value is intact, not just non-null.
            let x = VarId((0xFEu64 << 56) | 0x9999);
            let mut env = ExternalEnv::new();
            env.insert(x, slot.addr());
            let read_frag = machine
                .add_function("read_good", &build_reference_fragment(x), &table, &env)
                .expect("add_function read_good");
            let result = machine
                .run_fragment_pure(read_frag)
                .expect("run_fragment_pure read_good");
            assert_eq!(
                expect_int(&result),
                42,
                "post-error bind turn's tenured value must read back intact"
            );

            let verify_after = heap_verify_run_count();
            assert!(
                verify_after > verify_before,
                "heap_verify_run_count did not increase ({verify_before} -> {verify_after}) — \
                 the verifier never ran, so this test guarded nothing"
            );

            drop(machine);
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Value constructed by the case-miss scrutinee (arity 0), and the case's
/// lone alternative — deliberately a DIFFERENT tag, so the scrutinee matches
/// no alt: a genuine runtime case-miss trap (`RuntimeError::CaseTrap`).
const CASE_TRAP_SCRUT: DataConId = DataConId(60);
const CASE_TRAP_ALT: DataConId = DataConId(61);

fn case_trap_table() -> DataConTable {
    let mut table = DataConTable::new();
    table.insert(DataCon {
        id: CASE_TRAP_SCRUT,
        name: "CaseTrapScrut".to_string(),
        tag: 2,
        rep_arity: 0,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    table.insert(DataCon {
        id: CASE_TRAP_ALT,
        name: "CaseTrapAlt".to_string(),
        tag: 3,
        rep_arity: 0,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    table
}

/// `case CaseTrapScrut of { CaseTrapAlt -> 0 }` — the scrutinee's tag
/// matches no alternative: a genuine runtime case-miss trap.
fn build_case_trap_fragment() -> CoreExpr {
    let mut b = TreeBuilder::new();
    let scrut = b.push(CoreFrame::Con {
        tag: CASE_TRAP_SCRUT,
        fields: vec![],
    });
    let body = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    b.push(CoreFrame::Case {
        scrutinee: scrut,
        binder: VarId(999),
        alts: vec![Alt {
            con: AltCon::DataAlt(CASE_TRAP_ALT),
            binders: vec![],
            body,
        }],
    });
    b.build()
}

/// Pins the disposition contract `build_error_fragment` above deliberately
/// avoids exercising: a case-trap is a heap-shape integrity failure, not an
/// ordinary runtime error, so it must leave the machine `Unavailable` —
/// permanently refusing further `add_function` calls with
/// `JitError::MachineUnavailable { failure: Some(MachineFailure { cause:
/// CaseTrap, .. }) }` — rather than staying `Reusable` the way
/// `DivisionByZero`/`PatternMatchFailure`/`UserError` do.
#[test]
#[serial]
fn case_trap_makes_the_machine_permanently_unavailable() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tidepool_codegen::host_fns::reset_test_counters();
            let table = case_trap_table();

            let dummy = build_value_fragment(0);
            let mut machine =
                JitEffectMachine::compile_session(&dummy, &table, 2048).expect("compile_session");

            let trap_frag = machine
                .add_function(
                    "trap_turn",
                    &build_case_trap_fragment(),
                    &table,
                    &ExternalEnv::new(),
                )
                .expect("add_function trap_turn");
            let trap_result = machine.run_pure_and_bind(trap_frag);
            assert!(
                trap_result.is_err(),
                "a case trap must fail cleanly, got {trap_result:?}"
            );

            // The machine must now permanently refuse further add_function
            // calls: the exact contract `bind_error_then_allocate` above must
            // NOT trigger with its own (Reusable-class) error turn.
            let refusal = machine.add_function(
                "after_trap",
                &build_value_fragment(1),
                &table,
                &ExternalEnv::new(),
            );
            match refusal {
                Err(JitError::MachineUnavailable { failure }) => {
                    let cause = failure.map(|f| f.cause);
                    assert_eq!(
                        cause,
                        Some(tidepool_codegen::host_fns::RuntimeError::CaseTrap),
                        "machine refused for the wrong cause: {cause:?}"
                    );
                }
                other => panic!(
                    "expected JitError::MachineUnavailable after a case trap, got {other:?}"
                ),
            }

            drop(machine);
        })
        .unwrap()
        .join()
        .unwrap();
}
