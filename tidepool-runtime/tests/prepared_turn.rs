//! Card B2: a live "session turn" -- the same shape a notebook cell is --
//! compiled through the PREPARED-STG projection (`--target`, not the Core
//! `--turn` path `session::turn::run_turn` uses) and installed into a
//! running [`tidepool_runtime::session::PreparedRuntime`], with a LATER turn
//! importing an EARLIER turn's binding by retained generation.
//!
//! Mirrors `haskell/test-prepared-stg/ImportProducer.hs` /
//! `ImportConsumer.hs` / `ImportConsumerOracle.hs`'s source shape exactly, so
//! the expected final value (`6`) is independently GHC-verified there
//! (`ImportConsumerExpectations.json`'s `consumerResult`), not hand-derived
//! by this test.
//!
//! Needs a resolvable `$TIDEPOOL_EXTRACT` (and its Haskell worker) on the
//! GHC 9.12 toolchain this workspace pins — see `haskell/CLAUDE.md`:
//!
//! ```text
//! source scripts/lib-extract.sh && resolve_tidepool_extract
//! cargo nextest run -p tidepool-runtime --test prepared_turn --ignore-default-filter
//! ```

use std::path::{Path, PathBuf};

use tidepool_codegen::prepared_program::{CompileError, Unsupported};
use tidepool_extract_cmd::{resolve_bin, ResolvedExtractBin};
use tidepool_repr::Literal;
use tidepool_runtime::session::{
    PreparedRuntimeError, PreparedTurnError, RealmId, SessionTurns, TurnForm,
};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-runtime has a workspace parent")
        .to_path_buf()
}

fn resolved_bin() -> ResolvedExtractBin {
    ResolvedExtractBin::assume_resolved(
        resolve_bin()
            .expect(
                "resolve $TIDEPOOL_EXTRACT: source scripts/lib-extract.sh && resolve_tidepool_extract",
            )
            .path,
    )
}

/// An observed boxed `Int`: either a bare literal or an `I#` box around one
/// (mirrors `prepared_execution.rs`'s `observed_int_list` head-decoding, for
/// a lone scalar rather than a list spine).
fn observed_int(value: &tidepool_bridge::Value) -> i64 {
    match value {
        tidepool_bridge::Value::Lit(Literal::LitInt(n)) => *n,
        tidepool_bridge::Value::Con(_, fields) => match fields.as_slice() {
            [tidepool_bridge::Value::Lit(Literal::LitInt(n))] => *n,
            other => panic!("unexpected boxed Int shape {other:?}"),
        },
        other => panic!("unexpected Int value shape {other:?}"),
    }
}

#[test]
#[ignore = "needs a resolvable $TIDEPOOL_EXTRACT + Haskell worker; see module doc"]
fn later_turn_imports_an_earlier_turns_binding_by_retained_generation() {
    let root = workspace_root();
    let lib = root.join("haskell/lib");
    assert!(
        lib.join("Tidepool/Prelude.hs").is_file(),
        "stdlib root not found at {}",
        lib.display()
    );

    let session_dir = tempfile::tempdir().expect("create a session-root tempdir");
    let turns = SessionTurns::new(resolved_bin(), session_dir.path(), vec![lib]);

    // Turn 1 (Decl): introduces `producerValue`. The session's first turn,
    // so it also constructs the `PreparedRuntime`.
    let (mut runtime, producer_value) = turns
        .first(TurnForm::Decl("producerValue = [1, 2, 3]"), "producerValue")
        .expect("turn 1 (producerValue) projects and installs as the session's first program");

    // Turn 2 (Decl): introduces `producerFn`, referencing turn 1's
    // `producerValue` purely as data (`length`, never applying an imported
    // function) -- the shape `ImportConsumer.hs`'s `consumerValueAt` shows
    // installs and runs correctly today.
    let producer_fn = turns
        .run(
            &mut runtime,
            TurnForm::Decl("producerFn n = n + length producerValue"),
            "producerFn",
        )
        .expect("turn 2 (producerFn) imports producerValue by retained generation and installs");

    assert_ne!(producer_value, producer_fn);

    // Turn 3 (Expr, wrapped `it = producerFn (length producerValue)`): the
    // same shape as `ImportConsumer.hs`'s `consumerResult`, which that
    // module's own doc comment and
    // `prepared_execution.rs::s6_direct_global_call_is_not_yet_admitted`
    // document as a KNOWN, documented gap -- admission's whole-program check
    // (`tidepool-codegen/src/prepared_program/admission.rs`'s
    // `ExprFrame::Call` arm) has no case for a `Global` callee, so a program
    // whose reachable closure directly calls an imported function is
    // rejected at install, not just at compile. Turn 3 calls the
    // retained-generation import `producerFn` directly, so it is expected to
    // hit exactly that gap.
    let turn3 = turns.run(
        &mut runtime,
        TurnForm::Expr("producerFn (length producerValue)"),
        "it",
    );

    match turn3 {
        Ok(it) => {
            // The admission gap this test's doc comment describes has
            // closed: verify the value for real instead of just accepting
            // success silently.
            let result = runtime
                .run_entry(None, &[], true, RealmId::ROOT)
                .expect("turn 3's entry runs");
            assert_eq!(result.values.len(), 1);
            let observed = observed_int(&result.values[0]);
            assert_eq!(
                observed, 6,
                "producerFn (length producerValue) == 6, per ImportConsumerOracle.hs"
            );
            let _ = it;
        }
        Err(PreparedTurnError::Runtime(PreparedRuntimeError::Compile(
            CompileError::Unsupported(Unsupported::Expression { .. }),
        ))) => {
            // EXPECTED, DOCUMENTED FINDING: this is the exact shape
            // `prepared_execution.rs::s6_direct_global_call_is_not_yet_admitted`
            // pins for `ImportConsumer.hs`'s `consumerResult` (`producerFn
            // (length producerValue)` -- the identical source shape).
            // Admission's whole-program check
            // (`tidepool-codegen/src/prepared_program/admission.rs`'s
            // `ExprFrame::Call` arm) has no case for a `Global` callee, so a
            // program whose reachable closure directly calls an imported
            // function is rejected at compile-for-install, not merely at
            // link time. Turn 3 hits this because `producerFn` (turn 2's
            // binding) is itself installed as a SEPARATE program from turn
            // 3's, so calling it is necessarily a cross-program `Global`
            // call under this mechanism -- there is no local recompilation
            // path that would avoid it while still proving retained-
            // generation import of a CALLABLE binding.
        }
        Err(other) => panic!(
            "turn 3 failed for a DIFFERENT reason than the documented Global-callee admission \
             gap -- this is a genuine new finding, not the expected one: {other:#?}"
        ),
    }
}
