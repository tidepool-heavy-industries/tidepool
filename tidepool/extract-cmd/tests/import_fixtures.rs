//! One-off generator for the S5 executable-import fixtures
//! (`bridge/haskell/test-prepared-stg/fixtures/import-producer.cbor`,
//! `import-consumer.cbor`, and `import-consumer-result.cbor`), pinned for
//! S6. Not part of the standing test
//! battery: fixtures are pinned artifacts, generated once by a human/agent
//! action and thereafter only READ (via `include_bytes!`) — see the other
//! `fixtures/*.cbor` in this tree. `#[ignore]`d so a bare `cargo test`/
//! `cargo nextest run` never rewrites them.
//!
//! Needs a resolvable `tidepool-extract` binary (`$TIDEPOOL_EXTRACT`) and its
//! Haskell worker (`$TIDEPOOL_EXTRACT_WORKER`), and a GHC on `PATH` that can
//! load it (see bridge/haskell/CLAUDE.md). Run explicitly, e.g.:
//!
//! ```text
//! source scripts/lib-extract.sh && resolve_tidepool_extract
//! cargo test --config 'build.rustc-wrapper=""' -p tidepool-extract-cmd \
//!   --test import_fixtures -- --ignored --nocapture
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests assert on known-good values; .clippy.toml allows this in test code"
)]
use std::path::{Path, PathBuf};

use tidepool_extract_cmd::{resolve_bin, ExtractCmd, ResolvedExtractBin, SymbolIdentity};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-extract-cmd has a source-root parent")
        .parent()
        .expect("tidepool source root has a workspace parent")
        .to_path_buf()
}

fn stdlib_lib_dir(root: &Path) -> PathBuf {
    root.join("bridge/haskell/lib")
}

#[test]
#[ignore = "one-off fixture generator; needs a built extractor + GHC, see module doc"]
fn generate_import_producer_and_consumer_fixtures() {
    let root = workspace_root();
    let bin = ResolvedExtractBin::assume_resolved(
        resolve_bin()
            .expect(
                "resolve $TIDEPOOL_EXTRACT: source scripts/lib-extract.sh && resolve_tidepool_extract",
            )
            .path,
    );
    let lib = stdlib_lib_dir(&root);
    assert!(
        lib.join("Tidepool/Prelude.hs").is_file(),
        "stdlib root not found at {}",
        lib.display()
    );
    let fixture_src = root.join("bridge/haskell/test-prepared-stg");
    let out = std::env::temp_dir().join(format!(
        "tidepool-import-fixtures-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos()
    ));
    let producer_out = out.join("producer");
    let consumer_out = out.join("consumer");
    std::fs::create_dir_all(&producer_out).expect("create producer output dir");
    std::fs::create_dir_all(&consumer_out).expect("create consumer output dir");

    // import-producer.cbor: an ordinary projection. producerFn's own body
    // references producerValue, so target=producerFn's reachability closure
    // carries both bindings into one WireProgram.
    let mut producer_cmd = ExtractCmd::with_bin(bin.clone());
    producer_cmd
        .input(fixture_src.join("ImportProducer.hs"))
        .output_dir(&producer_out)
        .target("producerFn")
        .include(&lib);
    let producer_result = producer_cmd
        .bind()
        .and_then(|endpoint| endpoint.execute(&producer_cmd))
        .expect("import-producer compile should complete");
    assert!(
        producer_result.output.status.success(),
        "import-producer compile failed: {}",
        String::from_utf8_lossy(&producer_result.output.stderr)
    );
    let producer_cbor = producer_out.join("producerFn.prepared.cbor");
    assert!(
        producer_cbor.is_file(),
        "expected {} to exist",
        producer_cbor.display()
    );

    // import-consumer.cbor / import-consumer-result.cbor: `consumerEntries`
    // and `consumerResultAt` do not reference each other (see
    // ImportConsumer.hs), so `.targets([a, b])` projects each from its own
    // reachability closure into its own `<target>.prepared.cbor` in one
    // compile (`writePreparedArtifacts` in bridge/haskell/app/Main.hs writes one
    // file per requested target). Kept as two separate artifacts, not
    // combined into one closure: `consumerResultAt`'s body calls the
    // imported `producerFn` directly by name, which
    // `admission.rs::check`'s `ExprFrame::Call` arm has no case for at all
    // (only a `ValueRef::Local` callee is matched; a `Global` callee falls
    // through to the wildcard rejection) -- so a program whose reachable
    // closure contains that call fails to install AT ALL ("admission is
    // whole-program"). Keeping it in its own artifact means that gap does
    // not also break `consumerValueAt`/`consumerEntries`, which install and
    // run correctly today; see ImportConsumer.hs's doc on
    // `consumerResultAt` and
    // `tidepool/runtime/tests/prepared_execution.rs`'s
    // `s6_direct_global_call_is_not_yet_admitted` for the pinned repro.
    // Both producer symbols are declared as executable imports at
    // generation 11 in both artifacts. The projection must exclude their
    // bodies from recovery and declare them as globals carrying that
    // generation, even though ImportProducer.hs is compiled alongside
    // ImportConsumer.hs as a home module (both are under the same
    // --include root).
    let producer_value_id = SymbolIdentity {
        unit: "main".to_owned(),
        module: "ImportProducer".to_owned(),
        namespace: "value".to_owned(),
        occurrence: "producerValue".to_owned(),
        record_parent: None,
    };
    let producer_fn_id = SymbolIdentity {
        unit: "main".to_owned(),
        module: "ImportProducer".to_owned(),
        namespace: "value".to_owned(),
        occurrence: "producerFn".to_owned(),
        record_parent: None,
    };
    let mut consumer_cmd = ExtractCmd::with_bin(bin);
    consumer_cmd
        .input(fixture_src.join("ImportConsumer.hs"))
        .output_dir(&consumer_out)
        .targets(["consumerEntries", "consumerResultAt"])
        .include(&lib)
        .include(&fixture_src)
        .retained_generation(producer_value_id, 11)
        .retained_generation(producer_fn_id, 11);
    let consumer_result = consumer_cmd
        .bind()
        .and_then(|endpoint| endpoint.execute(&consumer_cmd))
        .expect("import-consumer compile should complete");
    assert!(
        consumer_result.output.status.success(),
        "import-consumer compile failed: {}",
        String::from_utf8_lossy(&consumer_result.output.stderr)
    );
    let consumer_cbor = consumer_out.join("consumerEntries.prepared.cbor");
    assert!(
        consumer_cbor.is_file(),
        "expected {} to exist",
        consumer_cbor.display()
    );
    let consumer_result_cbor = consumer_out.join("consumerResultAt.prepared.cbor");
    assert!(
        consumer_result_cbor.is_file(),
        "expected {} to exist",
        consumer_result_cbor.display()
    );

    let fixtures_dir = fixture_src.join("fixtures");
    std::fs::create_dir_all(&fixtures_dir).expect("create fixtures dir");
    std::fs::copy(&producer_cbor, fixtures_dir.join("import-producer.cbor"))
        .expect("copy import-producer.cbor into fixtures/");
    std::fs::copy(&consumer_cbor, fixtures_dir.join("import-consumer.cbor"))
        .expect("copy import-consumer.cbor into fixtures/");
    std::fs::copy(
        &consumer_result_cbor,
        fixtures_dir.join("import-consumer-result.cbor"),
    )
    .expect("copy import-consumer-result.cbor into fixtures/");

    println!(
        "wrote {}, {}, and {}",
        fixtures_dir.join("import-producer.cbor").display(),
        fixtures_dir.join("import-consumer.cbor").display(),
        fixtures_dir.join("import-consumer-result.cbor").display()
    );
}
