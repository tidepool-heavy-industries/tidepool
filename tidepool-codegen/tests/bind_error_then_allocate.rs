//! Finding 4 (repo-review-2026-07-06/01-gc-memory-safety.md): `run_pure_and_bind`'s
//! error paths (bare `?` / `return Err`) used to bypass `_guard.arm_reclaim(...)`.
//! `RegistryGuard::drop` with `reclaim: None` frees the retained session buffer
//! (`clear_run_scratch`) but never writes back `session.heap`/`session.cursor` —
//! so `session.cursor` is left pointing at whatever high-water mark a PRIOR
//! (possibly GC-grown) buffer had reached, while `session.heap` stays `None`.
//! The next run's `install_registries` then falls back to the machine's
//! original, small, fixed-size `Nursery` buffer, and `make_session_vmctx`
//! computes `alloc_ptr = nursery.start() + <stale, possibly much larger> cursor`
//! — an out-of-bounds allocation pointer.
//!
//! This mirrors the harness style of `converge_proof.rs` (same crate): a
//! session machine driven turn-by-turn via `add_function` + a bind primitive,
//! with a reference fragment reading back a tenured value to prove
//! correctness. It reproduces the plan's exact scenario: an ordinary runtime
//! error in a bind turn (`head []`-shaped — a genuine case-miss trap, not a
//! test bug), immediately followed by an allocating turn, which must succeed
//! cleanly (no crash, correct result) rather than computing a bad alloc
//! pointer from a stale cursor.
//!
//! Per the DEV AGENT PROTOCOL boundary for this task: this is a NEW test file
//! (not an edit to `tidepool-repl/tests/it_binding.rs`, which lives outside
//! `tidepool-codegen`/`tidepool-heap` and is owned by a concurrent worker) that
//! drives the actual buggy function (`run_pure_and_bind`, in this crate)
//! directly, rather than through the full REPL/GHC-extract stack `it_binding.rs`
//! needs — `converge_proof.rs`'s harness is the closest in-boundary analog to
//! its turn-by-turn session pattern.

use serial_test::serial;
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::host_fns::{heap_verify_run_count, set_heap_verify};
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::types::{Alt, AltCon, DataConId, Literal, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, DataConTable, TreeBuilder};

#[path = "support/session_scaffold.rs"]
mod session_scaffold;
use session_scaffold::{build_gc_forcing_fragment, build_reference_fragment, build_value_fragment};
use session_scaffold::{expect_int, C1};

/// The value actually constructed by the error fragment's scrutinee (arity 0).
const ERR_SCRUT: DataConId = DataConId(60);
/// The case's lone alternative — deliberately a DIFFERENT tag than `ERR_SCRUT`,
/// so the scrutinee matches no alt (case-miss trap), exactly like `head []`'s
/// pattern-match-failure-on-`[]` shape.
const ERR_ALT: DataConId = DataConId(61);

fn table() -> DataConTable {
    let mut table = DataConTable::new();
    table.insert(DataCon {
        id: C1,
        name: "C1".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
    });
    table.insert(DataCon {
        id: ERR_SCRUT,
        name: "ErrScrut".to_string(),
        tag: 2,
        rep_arity: 0,
        field_bangs: vec![],
        qualified_name: None,
    });
    table.insert(DataCon {
        id: ERR_ALT,
        name: "ErrAlt".to_string(),
        tag: 3,
        rep_arity: 0,
        field_bangs: vec![],
        qualified_name: None,
    });
    table
}

/// `case ErrScrut of { ErrAlt -> 0 }` — the scrutinee's tag matches no
/// alternative: a genuine runtime case-miss trap (`RuntimeError::CaseTrap`),
/// not a compile-time or test-construction error. Mirrors the `head []`
/// scenario named in the plan.
fn build_error_fragment() -> CoreExpr {
    let mut b = TreeBuilder::new();
    let scrut = b.push(CoreFrame::Con {
        tag: ERR_SCRUT,
        fields: vec![],
    });
    let body = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    b.push(CoreFrame::Case {
        scrutinee: scrut,
        binder: VarId(999),
        alts: vec![Alt {
            con: AltCon::DataAlt(ERR_ALT),
            binders: vec![],
            body,
        }],
    });
    b.build()
}

/// `x <- pure (head ([] :: [Int]))` (error turn) followed by an allocating
/// turn: the error turn must fail cleanly (not crash, not corrupt session
/// state), and the FOLLOWING bind turn must succeed with a correct value.
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

            // Tiny 2 KiB nursery — matches converge_proof's GC-forcing setup.
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

            // Turn 2 (error): a genuine runtime case-miss trap in a BIND turn
            // — the exact function under test, `run_pure_and_bind`.
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

            // Read back through a reference fragment (converge_proof pattern)
            // to prove the tenured value is intact, not just non-null.
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
