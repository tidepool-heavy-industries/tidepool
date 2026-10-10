//! Run in the pinned shell with the matched Rust frontend/Haskell worker and
//! optionally TIDEPOOL_JOIN_PROOF_BIN naming the compiled proof executable.
use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};
use tidepool_toolchain::declaration_join::*;

fn fixture(root: &Path, name: &str, source: &str) {
    std::fs::write(root.join(format!("{name}.hs")), source).unwrap();
}

#[test]
fn exact_join_round_trips_actual_worker_and_source_hidden_consumers() {
    use tidepool_extract_cmd::CompilerTransactionClose;
    let observation = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recipient = observation.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        exact_join_with_settlement(&mut |close| recipient.lock().unwrap().push(close))
    }));
    let closes = observation.lock().unwrap();
    assert!(
        closes.iter().all(|close| matches!(
            close,
            CompilerTransactionClose::Clean | CompilerTransactionClose::NotStarted
        )),
        "unconfirmed compiler close: {closes:?}"
    );
    drop(closes);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

fn exact_join_with_settlement(
    settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
) {
    let scratch = std::sync::Arc::new(tempfile::tempdir().unwrap());
    let root = scratch.path();
    fixture(root, "Common", include_str!("../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/exact-isolation/Common.hs"));
    fixture(root, "Old", include_str!("../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/exact-isolation/Old.hs"));
    fixture(root, "Public", include_str!("../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/exact-isolation/Public.hs"));
    fixture(root, "Consumer", include_str!("../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/exact-isolation/Consumer.hs"));
    fixture(root, "BadClass", include_str!("../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/exact-isolation/BadClass.hs"));
    fixture(root, "BadFamily", include_str!("../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/exact-isolation/BadFamily.hs"));
    fixture(root, "BadAssociated", include_str!("../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/exact-isolation/BadAssociated.hs"));
    fixture(root, "BadFD", include_str!("../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/exact-isolation/BadFD.hs"));
    fixture(root, "Next", include_str!("../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/exact-isolation/Next.hs"));
    fixture(root, "NextConsumer", include_str!("../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/exact-isolation/NextConsumer.hs"));
    fixture(root, "BadRetraction", include_str!("../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/exact-isolation/BadRetraction.hs"));
    fixture(root, "BadNextClass", include_str!("../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/exact-isolation/BadNextClass.hs"));
    fixture(root, "BadNextFamily", include_str!("../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/exact-isolation/BadNextFamily.hs"));
    let mut artifacts = Vec::new();
    for module in ["Common", "Old", "Public"] {
        let output = Command::new("ghc")
            .current_dir(root)
            .args(["-c", &format!("{module}.hs")])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let path = root.join(format!("{module}.hi"));
        artifacts.push(DeclarationArtifact {
            interface: ExactIfaceArtifact {
                unit: "main".into(),
                module: module.into(),
                sha256: format!("{:x}", Sha256::digest(std::fs::read(&path).unwrap())),
                path,
                requirements: if module == "Common" {
                    vec![]
                } else {
                    vec![("main".into(), "Common".into())]
                },
            },
            product: Some(
                ModuleSnapshot::capture(module.into(), root.join(format!("{module}.o"))).unwrap(),
            ),
        });
        std::fs::rename(
            root.join(format!("{module}.hs")),
            root.join(format!("{module}.hidden")),
        )
        .unwrap();
    }
    let includes = vec![root.to_owned()];
    let inventory =
        inspect_declaration_artifacts(&artifacts, &includes, root, scratch.clone(), settlement)
            .unwrap();
    assert_eq!(inventory.decision, JoinDecision::Accepted);
    let inventories = inventory.inventories.unwrap();
    assert_eq!(inventories.len(), 3);
    let mut exports = inventories[0].exports.clone();
    exports.extend(inventories[1].exports.clone());
    let mut instances = inventories[2].instances.clone();
    // The fixed fixture intentionally admits one dictionary from a module
    // whose other dictionaries remain available only to original code.
    let old_double = inventories[1]
        .instances
        .classes
        .iter()
        .find(|record| record.dfun.occurrence == "$fCDouble")
        .expect("Old C Double dfun")
        .clone();
    assert_eq!(old_double.dfun.unit, "main");
    assert_eq!(old_double.dfun.module, "Old");
    assert_eq!(old_double.dfun.namespace, ExportNamespace::Value);
    assert_eq!(
        old_double.class,
        ExportIdentity {
            unit: "main".into(),
            module: "Common".into(),
            namespace: ExportNamespace::Type,
            occurrence: "C".into(),
            record_parent: None,
        }
    );
    assert!(old_double.selected_axioms.is_empty());
    instances.classes.push(old_double);
    assert!(inventories[1]
        .instances
        .classes
        .iter()
        .all(|record| record.dfun.unit == "main"
            && record.dfun.module == "Old"
            && record.dfun.namespace == ExportNamespace::Value
            && record.class.unit == "main"
            && record.class.module == "Common"
            && record.class.namespace == ExportNamespace::Type));
    let associated = inventories[1]
        .instances
        .classes
        .iter()
        .find(|record| record.class.occurrence == "A")
        .expect("Old A Int typed instance");
    assert_eq!(associated.dfun.occurrence, "$fAInt");
    assert!(!associated.selected_axioms.is_empty());
    assert!(associated
        .selected_axioms
        .iter()
        .all(|axiom| axiom.unit == "main"
            && axiom.module == "Old"
            && axiom.namespace == ExportNamespace::Type
            && inventories[1].instances.families.contains(axiom)));
    assert!(inventories[1]
        .instances
        .families
        .iter()
        .all(|name| name.module == "Old"));
    let input = DeclarationJoinInput {
        expected_public_version: "paired-public-snapshot".into(),
        public_module: None,
        private_base: None,
        private_tip: None,
        writes: vec![],
        reserved: ReservedJoin {
            unit: "main".into(),
            module: "Joined".into(),
            path: root.join("Joined.hi"),
        },
        expected_exports: exports,
        expected_instances: instances,
        artifacts: artifacts.clone(),
        family_closure: inventory.family_closure.unwrap(),
    };
    let accepted =
        validate_declaration_join(&input, &includes, root, &[], scratch.clone(), settlement)
            .unwrap();
    assert_eq!(accepted.decision, JoinDecision::Accepted);
    assert_eq!(
        accepted.expected_public_version,
        input.expected_public_version
    );
    let joined = accepted.artifact.unwrap();
    assert_eq!(
        joined.sha256,
        format!("{:x}", Sha256::digest(std::fs::read(&joined.path).unwrap()))
    );
    let mut changed = input.clone();
    changed.artifacts[0].interface.sha256 = "0".repeat(64);
    let rejected =
        validate_declaration_join(&changed, &includes, root, &[], scratch.clone(), settlement)
            .unwrap();
    assert!(matches!(
        rejected.decision,
        JoinDecision::Rejected {
            reason: JoinRejection::ArtifactChanged,
            ..
        }
    ));
    assert_eq!(
        rejected.expected_public_version,
        input.expected_public_version
    );
    assert!(rejected.artifact.is_none());
    let proof = proof_binary();
    let output = Command::new(proof)
        .args(["--consumer"])
        .arg(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!("{}", String::from_utf8_lossy(&output.stdout));
}

fn proof_binary() -> std::ffi::OsString {
    if let Some(proof) = std::env::var_os("TIDEPOOL_JOIN_PROOF_BIN") {
        return proof;
    }
    let bridge = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bridge/haskell");
    // Compile just the owning proof target in the already admitted pinned shell.
    // Cabal shares the library products built for the matched worker.
    let output = Command::new("cabal")
        .current_dir(&bridge)
        .args(["build", "declaration-join-test", "--offline"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let location = Command::new("cabal")
        .current_dir(bridge)
        .args(["list-bin", "declaration-join-test"])
        .output()
        .unwrap();
    assert!(
        location.status.success(),
        "{}",
        String::from_utf8_lossy(&location.stderr)
    );
    String::from_utf8(location.stdout).unwrap().trim().into()
}
