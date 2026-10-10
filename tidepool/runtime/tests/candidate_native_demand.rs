//! Optional originals satisfy native demand without expanding fresh work.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::compiler_test_support::{selected_lexical_closure, OwnedEnvironment};
use tidepool_testing::eval_harness::{prelude_path, EvalHarness};
use tidepool_toolchain::artifacts::{
    compile_invocation, compile_invocation_in_context, CompileInvocation, CompiledArtifacts,
};
use tidepool_toolchain::cache::ProductAvailability;
use tidepool_toolchain::certified_products::ProductOrigin;
use tidepool_toolchain::declaration_join::{ExactDeclarationContext, ExactModuleIdentity};
use tidepool_toolchain::recovery_artifacts::materialize_certified_products;

const CONSUMER: &str = include_str!("fixtures/CandidateDemandConsumer.hs");
const WARMER: &str = include_str!("fixtures/CandidateDemandWarmer.hs");
const SIBLING: &str = include_str!("fixtures/CandidateDemandSibling.hs");

fn require_validation_only(artifacts: &CompiledArtifacts, name: &str) {
    let owner = artifacts
        .module_inventory
        .as_ref()
        .expect("complete checked source graph")
        .iter()
        .find(|owner| !owner.boot && owner.module == name)
        .expect("unused imported source was checked");
    assert_eq!(owner.product, ProductAvailability::InterfaceOnly);
    assert!(!artifacts
        .certified_groups
        .iter()
        .any(|group| group.owner().module == name));
    assert!(!artifacts
        .module_products
        .iter()
        .any(|product| product.module == name));
}

fn ghc_result(root: &Path, include: &[PathBuf], name: &str, source: &str) -> serde_json::Value {
    let source_path = root.join(format!("{name}.hs"));
    std::fs::write(&source_path, source).unwrap();
    let libdir = std::env::var("TIDEPOOL_GHC_LIBDIR")
        .expect("native test declares the matching GHC toolchain");
    #[allow(
        clippy::disallowed_methods,
        reason = "bounded independent language oracle using the declared test toolchain"
    )]
    let oracle = std::process::Command::new("ghc")
        .current_dir(root)
        .arg(format!("-B{libdir}"))
        .args(include.iter().map(|path| format!("-i{}", path.display())))
        .arg(&source_path)
        .args(["-v0", "-e", &format!("print {name}.result")])
        .output()
        .expect("execute the native target's pinned GHC oracle");
    assert!(
        oracle.status.success(),
        "{}",
        String::from_utf8_lossy(&oracle.stderr)
    );
    serde_json::from_slice(&oracle.stdout).expect("GHC Int result")
}

#[test]
#[serial_test::serial]
fn candidates_preserve_native_demand_and_complete_original_siblings() {
    tidepool_testing::eval_harness::require_extract();
    let _daemon = OwnedEnvironment::set("TIDEPOOL_EXTRACT_DAEMON_SOCKET", None);
    let cache = tempfile::tempdir().unwrap();
    let _cache = OwnedEnvironment::set("TIDEPOOL_COMPILE_CACHE_DIR", Some(cache.path()));
    let work = tempfile::tempdir().unwrap();
    for (name, source) in [
        (
            "CandidateDemandClass",
            include_str!("fixtures/CandidateDemandClass.hs"),
        ),
        (
            "CandidateDemandInstance",
            include_str!("fixtures/CandidateDemandInstance.hs"),
        ),
        (
            "CandidateDemandFacade",
            include_str!("fixtures/CandidateDemandFacade.hs"),
        ),
        (
            "OptionalSupport",
            include_str!("../../../bridge/haskell/test-source-boot/fixtures/OptionalSupport.hs"),
        ),
    ] {
        std::fs::write(work.path().join(format!("{name}.hs")), source).unwrap();
    }
    let include = [work.path().to_path_buf(), prelude_path()];
    let compile = |source, name| {
        tidepool_testing::with_settlement(|settlement| {
            compile_invocation(
                &CompileInvocation {
                    source,
                    targets: &["__prepared"],
                    include: &include,
                    fallback_module_name: name,
                },
                |_, _, _| {},
                settlement,
            )
        })
        .expect("compile through the production candidate issuer and consumer")
    };
    let evaluator = EvalHarness::new();
    let expected = ghc_result(work.path(), &include, "CandidateDemandConsumer", CONSUMER);
    let sibling_expected = ghc_result(work.path(), &include, "CandidateDemandSibling", SIBLING);
    assert_eq!(expected, serde_json::json!(42));
    assert_eq!(sibling_expected, serde_json::json!(42));

    let cold = compile(CONSUMER, "CandidateDemandConsumer");
    require_validation_only(&cold, "OptionalSupport");
    assert!(cold
        .certified_groups
        .iter()
        .filter(|group| group.owner().module.starts_with("CandidateDemand"))
        .all(|group| group.origin() == ProductOrigin::Fresh));
    let mut handlers = frunk::HNil;
    let cancelled = tidepool_runtime::run_compiled_target(
        &cold,
        "__prepared",
        1024 * 1024,
        &mut handlers,
        &(),
        |cancel| cancel.cancel(),
    )
    .expect_err("cancellation must reach the installed imported target");
    assert!(matches!(
        cancelled,
        tidepool_runtime::RuntimeError::Prepared(
            tidepool_runtime::session::prepared::PreparedRuntimeError::Cancelled
        )
    ));
    assert_eq!(
        evaluator
            .run_target(&cold, "__prepared", frunk::HNil)
            .json(),
        expected
    );

    let include_refs = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let executed = tidepool_testing::with_settlement(|settlement| {
        tidepool_runtime::compile_and_run_cancellable(
            CONSUMER,
            "result",
            &include_refs,
            &mut handlers,
            &(),
            1024 * 1024,
            |_| {},
            settlement,
        )
    })
    .expect("production one-shot execution preserves certified imported groups");
    assert_eq!(executed.to_json(), expected);

    // The warmer demands the facade and class anchors as well as the instance.
    // Their production-issued originals form a closed candidate cohort.
    let original = compile(WARMER, "CandidateDemandWarmer");
    require_validation_only(&original, "OptionalSupport");
    assert_eq!(
        evaluator
            .run_target(&original, "__prepared", frunk::HNil)
            .json(),
        ghc_result(work.path(), &include, "CandidateDemandWarmer", WARMER)
    );
    assert!(
        original.certified_groups.iter().any(|group| {
            group.owner().module == "CandidateDemandInstance"
                && group
                    .group()
                    .binders()
                    .iter()
                    .any(|binder| binder.occurrence == "sibling")
        }),
        "a demanded module must issue its unused supported sibling"
    );

    // Change only the target to avoid a whole-result cache hit. Every selected
    // home owner still has identical source and checked package dependencies.
    let warm = compile(
        &format!("{CONSUMER}\n-- consume the closed optional cohort\n"),
        "CandidateDemandConsumer",
    );
    for name in [
        "CandidateDemandClass",
        "CandidateDemandInstance",
        "CandidateDemandFacade",
    ] {
        let groups = warm
            .certified_groups
            .iter()
            .filter(|group| group.owner().module == name)
            .collect::<Vec<_>>();
        assert!(!groups.is_empty(), "expected genuinely accepted {name}");
        assert!(groups
            .iter()
            .all(|group| group.origin() == ProductOrigin::Cached));
    }
    require_validation_only(&warm, "OptionalSupport");
    assert_eq!(
        evaluator
            .run_target(&warm, "__prepared", frunk::HNil)
            .json(),
        expected
    );

    // Preserve the issued native inventory, including the sibling that neither
    // consumer demanded. Recovery uses public certified-product admission.
    let instance = ExactModuleIdentity {
        unit: "main".into(),
        module: "CandidateDemandInstance".into(),
    };
    let lexical = selected_lexical_closure(&original, vec![instance]);
    let products = original
        .recovery_products
        .iter()
        .filter(|product| {
            lexical.contains_key(&ExactModuleIdentity {
                unit: product.owner().unit.clone(),
                module: product.owner().module.clone(),
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(products.len(), 2, "complete class and instance originals");
    let producer = original
        .artifact_view
        .descriptors()
        .into_iter()
        .find(|descriptor| descriptor.owner.module == "CandidateDemandInstance")
        .expect("original artifact carries its authenticated producer")
        .producer_sha256;
    let recovery = tempfile::tempdir().unwrap();
    let references = materialize_certified_products(recovery.path(), producer, &products)
        .expect("retain genuine original native products");
    let context = ExactDeclarationContext::capture_recovery(
        recovery.path(),
        &references,
        &[],
        &[],
        lexical.values().cloned().collect(),
    )
    .expect("admit the original product and canonical interface closure");
    for owner in lexical.keys() {
        std::fs::remove_file(work.path().join(format!("{}.hs", owner.module))).unwrap();
    }
    let later = tidepool_testing::with_settlement(|settlement| {
        compile_invocation_in_context(
            &CompileInvocation {
                source: SIBLING,
                targets: &["__prepared"],
                include: &include,
                fallback_module_name: "CandidateDemandSibling",
            },
            Arc::new(context),
            |_, _, _| {},
            settlement,
        )
    })
    .expect("later sibling consumer uses the source-less original inventory");
    assert!(
        !later.certified_groups.iter().any(|group| {
            group.owner().module == "CandidateDemandInstance"
                && group.origin() == ProductOrigin::RetainedCore
        }),
        "the issued native sibling must not need Core recovery"
    );
    assert_eq!(
        evaluator
            .run_target(&later, "__prepared", frunk::HNil)
            .json(),
        sibling_expected
    );
}
