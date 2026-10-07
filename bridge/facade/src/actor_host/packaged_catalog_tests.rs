//! Native consumption of a configured immutable deployment catalog.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use tidepool_toolchain::certified_products::ProductOrigin;

#[test]
#[ignore = "requires the matched native deployment catalog and empty user caches"]
fn packaged_cohort_executes_and_displays_without_build_inputs() {
    assert!(
        !Path::new(env!("CARGO_MANIFEST_DIR")).exists(),
        "run this retained binary with checkout and build outputs inaccessible"
    );
    assert!(
        std::env::var_os(tidepool_extract_cmd::DAEMON_SOCKET_ENV).is_none(),
        "the package gate must not adopt an existing daemon"
    );
    for variable in ["TIDEPOOL_COMPILE_CACHE_DIR", "TIDEPOOL_BUILD_PRODUCTS_DIR"] {
        let directory = PathBuf::from(std::env::var_os(variable).expect("explicit empty cache"));
        assert!(directory.is_absolute());
        assert_eq!(std::fs::read_dir(directory).unwrap().count(), 0);
    }
    let package = tidepool_toolchain::toolchain::configured_module_package()
        .unwrap()
        .expect("configured immutable module catalog");
    let selection = package.source_selection();
    assert!(selection.snapshot_root.starts_with("/nix/store"));
    let source_roots = selection.include_roots();
    assert_eq!(
        tidepool_mcp::ensure_effects_core_module().unwrap(),
        selection.root(tidepool_toolchain::toolchain::NativeSourceRole::StableEffects),
    );
    assert_eq!(
        crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
        selection.root(tidepool_toolchain::toolchain::NativeSourceRole::Actors),
    );
    assert_eq!(
        crate::haskell_sources::ensure_embedded_stdlib().unwrap(),
        selection.root(tidepool_toolchain::toolchain::NativeSourceRole::Stdlib),
    );
    let catalog_path = PathBuf::from(
        std::env::var_os(tidepool_toolchain::toolchain::ENV_COMPILER_MODULES).unwrap(),
    );
    let catalog: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&catalog_path).unwrap()).unwrap();
    let root = catalog_path.parent().unwrap();
    let inventory: BTreeSet<(String, String)> = catalog["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|module| {
            let owner: serde_json::Value = serde_json::from_slice(
                &std::fs::read(root.join(module["owner"]["path"].as_str().unwrap())).unwrap(),
            )
            .unwrap();
            (
                owner["unit"].as_str().unwrap().to_owned(),
                owner["module"].as_str().unwrap().to_owned(),
            )
        })
        .collect();

    for (name, row, imported_row) in [
        (
            "PackagedCatalogRootDisplay",
            "RootEffects",
            "Tidepool.Actors.Internal.ExomonadDriver",
        ),
        (
            "PackagedCatalogActorDisplay",
            "ActorEffects",
            "Tidepool.Actors.Role",
        ),
    ] {
        let source = tidepool_runtime::session::assemble_expression_module(
        &format!("{{-# LANGUAGE DataKinds, FlexibleContexts, FlexibleInstances, MultiParamTypeClasses, NoImplicitPrelude, TypeOperators, UndecidableInstances #-}}\nmodule {name} where\nimport Tidepool.Prelude\nimport Control.Monad.Freer (Eff)\nimport {imported_row} ({row})\n"),
        "result",
        row,
        "show (length (sort [41, 2, 3]) + 39 :: Int)",
        tidepool_runtime::session::ExpressionLift::Pure,
    );
        let target = tidepool_runtime::session::PREPARED_SCAFFOLD_TARGET;
        let compile_start = std::time::Instant::now();
        let compiled = tidepool_runtime::compile_targets(
            &source,
            &[target],
            &source_roots,
            |stage, elapsed, bytes| {
                eprintln!(
                    "deployment compile {row}: stage={stage} elapsed_ns={} payload_bytes={bytes}",
                    elapsed.as_nanos(),
                );
            },
        )
        .expect("package-backed expression compilation");
        eprintln!(
            "deployment compile {row}: total_elapsed_ns={}",
            compile_start.elapsed().as_nanos(),
        );
        let accepted: BTreeSet<_> = compiled
            .certified_groups
            .iter()
            .filter(|group| group.origin() == ProductOrigin::Cached)
            .map(|group| (group.owner().unit.clone(), group.owner().module.clone()))
            .collect();
        eprintln!(
            "deployment catalog {}: {row}: inventory={} accepted_cached_owners={}",
            package.catalog_identity(),
            serde_json::to_string(&inventory).unwrap(),
            serde_json::to_string(&accepted).unwrap(),
        );
        let cached: BTreeSet<_> = compiled
            .certified_groups
            .iter()
            .filter(|group| group.origin() == ProductOrigin::Cached)
            .map(|group| group.owner().module.clone())
            .collect();
        assert!(cached.contains("Tidepool.Prelude"));
        assert!(cached.contains("Tidepool.Render"));
        assert!(cached.contains("Tidepool.FilePath"));
        assert!(cached.contains(imported_row));
        assert!(cached.contains("Tidepool.Effects.Core"));
        // The worker's canonicalLoadPhase refuses accepted candidates at
        // T_Hsc with CandidateFrontendReplayRefused. The assertion below also
        // rejects catalog owners reported as freshly extracted in the groups.
        let replayed = compiled
            .certified_groups
            .iter()
            .filter(|group| {
                inventory.contains(&(group.owner().unit.clone(), group.owner().module.clone()))
                    && group.origin() == ProductOrigin::Fresh
            })
            .count();
        assert_eq!(replayed, 0, "catalog owners must not be re-extracted");
        let execution_start = std::time::Instant::now();
        let value = tidepool_runtime::run_compiled_target(
            &compiled,
            target,
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            &mut frunk::HNil,
            &(),
            |_| {},
        )
        .expect("actual native execution of the package-backed target");
        eprintln!(
            "deployment execution {row}: elapsed_ns={}",
            execution_start.elapsed().as_nanos(),
        );
        let displayed = tidepool_runtime::value_to_json(&value, &compiled.table, 0);
        assert_eq!(displayed, serde_json::json!("42"));
        eprintln!(
        "deployment catalog {}: {row}: {} cached owners, {replayed} re-extracted; native display {displayed}",
        package.catalog_identity(),
        cached.len(),
    );
    }
}
