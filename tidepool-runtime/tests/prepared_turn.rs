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

use tidepool_codegen::binding_table::BoundValue;
use tidepool_extract_cmd::{resolve_bin, ResolvedExtractBin};
use tidepool_repr::Literal;
use tidepool_runtime::session::{RealmId, SessionTurns, TurnForm};

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
    // same shape as `ImportConsumer.hs`'s `consumerResult`. `producerFn` is
    // turn 2's binding, installed as a SEPARATE program, so this is a
    // direct cross-program call to an import (`ValueRef::Global` callee)
    // resolved through the machine-wide table at run time.
    let it = turns
        .run(
            &mut runtime,
            TurnForm::Expr("producerFn (length producerValue)"),
            "it",
        )
        .expect("turn 3 applies turn 2's binding directly and installs");
    assert_ne!(it, producer_fn);

    let BoundValue::Prepared {
        origin: Some(origin),
        ..
    } = &runtime
        .bindings()
        .get(it)
        .expect("`it` is a live binding")
        .value
    else {
        panic!("`it` was bound from turn 3's own program entry");
    };
    let (program, entry) = (origin.program, origin.value);
    let result = runtime
        .run_entry_in(program, entry, &[], true, RealmId::ROOT)
        .expect("turn 3's entry runs");
    assert_eq!(result.values.len(), 1);
    assert_eq!(
        observed_int(&result.values[0]),
        6,
        "producerFn (length producerValue) == 6, per ImportConsumerOracle.hs"
    );
}
