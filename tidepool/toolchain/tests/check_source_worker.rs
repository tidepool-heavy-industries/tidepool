//! Execute with the matched frontend/worker in the repository pinned shell.
use std::path::{Path, PathBuf};

use tidepool_extract_cmd::ExtractCmd;
use tidepool_toolchain::artifacts::{check_source, SourceCheckRequest};
use tidepool_toolchain::{diag, toolchain::AdmittedCompilerEndpoint, CompileError};

const DEPENDENCY: &str = include_str!("fixtures/check-source/CheckDependency.hs");
const MIDDLE: &str = include_str!("fixtures/check-source/CheckMiddle.hs");
const CONSUMER: &str = include_str!("fixtures/check-source/CheckConsumer.hs");

fn descendants(root: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            result.extend(descendants(&path));
        } else {
            result.push(path);
        }
    }
    result
}

#[test]
fn complete_source_check_uses_current_transitive_graph_without_native_products() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    let dependency = root.join("CheckDependency.hs");
    std::fs::write(&dependency, DEPENDENCY).unwrap();
    std::fs::write(root.join("CheckMiddle.hs"), MIDDLE).unwrap();
    let source = root.join("CheckConsumer.hs");
    std::fs::write(&source, CONSUMER).unwrap();
    let includes = [root.to_owned()];
    let request = SourceCheckRequest {
        source: CONSUMER,
        include: &includes,
        fallback_module_name: "CheckConsumer",
    };
    let mut closes = Vec::new();
    let mut settlement = |close| closes.push(close);
    check_source(&request, &mut settlement).expect("complete original module checks");

    // An unchanged consumer must see a newly invalid transitive source.
    std::fs::write(
        &dependency,
        DEPENDENCY.replace("value = 41", "value = True"),
    )
    .unwrap();
    let rejection =
        check_source(&request, &mut settlement).expect_err("transitive type error must reject");
    let CompileError::Diagnostics(diagnostics) = rejection else {
        panic!("expected real source diagnostics, got {rejection:?}");
    };
    assert!(diagnostics.iter().any(|diagnostic| diagnostic
        .span
        .as_ref()
        .is_some_and(|span| span.file.ends_with("CheckDependency.hs"))));

    // Retrying after repair checks the current graph, without negative caching.
    std::fs::write(&dependency, DEPENDENCY).unwrap();
    check_source(&request, &mut settlement).expect("repaired transitive source checks");
    drop(settlement);
    assert_eq!(closes.len(), 3);
    assert!(closes
        .iter()
        .all(tidepool_extract_cmd::CompilerTransactionClose::is_clean));

    let products = root.join("checking-products");
    std::fs::create_dir(&products).unwrap();
    let mut command = ExtractCmd::new().unwrap();
    command
        .input(&source)
        .check_source()
        .includes(&includes)
        .build_products_dir(&products);
    let endpoint = AdmittedCompilerEndpoint::from_bound(command.bind().unwrap()).unwrap();
    let run = endpoint.execute(&command).unwrap();
    diag::decode_extract_result(run.success(), &run.output.stdout, &run.output.stderr).unwrap();
    assert!(
        descendants(root).iter().all(|path| !matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("cbor" | "o" | "dyn_o" | "tpmod")
        )),
        "checking must not emit prepared/native/module products"
    );

    command.output_dir(root.join("forbidden-output"));
    let endpoint = AdmittedCompilerEndpoint::from_bound(command.bind().unwrap()).unwrap();
    let run = endpoint.execute(&command).unwrap();
    let rejection =
        diag::decode_extract_result(run.success(), &run.output.stdout, &run.output.stderr)
            .expect_err("checking cannot carry output authority");
    let CompileError::WorkerFailure(diagnostics) = rejection else {
        panic!("expected invalid-mode diagnostics, got {rejection:?}");
    };
    assert!(diagnostics.iter().any(|diagnostic| diagnostic
        .message
        .contains("source checking cannot carry product or notebook authority")));
    assert!(!root.join("forbidden-output").exists());
}
