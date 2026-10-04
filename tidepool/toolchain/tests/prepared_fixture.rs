//! Executed contracts for immutable fixture actions through the production CLI.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tidepool_repr::execution_schema::DecodeLimits;
use tidepool_toolchain::prepared_artifact::PreparedArtifact;

fn configured(name: &str) -> std::ffi::OsString {
    std::env::var_os(name).unwrap_or_else(|| panic!("missing declared compiler resource {name}"))
}

#[allow(
    clippy::disallowed_methods,
    reason = "integration test invokes the owning build-action CLI"
)]
fn compiler(source: &Path, output: &Path, targets: &[&str], roots: &[PathBuf]) -> Command {
    let mut command = Command::new(configured("TIDEPOOL_PREPARED_FIXTURE_COMPILER"));
    command.current_dir(source.parent().unwrap());
    command.args([
        "--frontend",
        configured("TIDEPOOL_EXTRACT").to_str().unwrap(),
        "--worker",
        configured("TIDEPOOL_EXTRACT_WORKER").to_str().unwrap(),
        "--deployment",
        configured("TIDEPOOL_COMPILER_DEPLOYMENT").to_str().unwrap(),
        "--ghc-libdir",
        configured("TIDEPOOL_GHC_LIBDIR").to_str().unwrap(),
        "--runtime-libraries",
        configured("TIDEPOOL_EXTRACT_RUNTIME_LIBRARIES")
            .to_str()
            .unwrap(),
    ]);
    command
        .arg("--source")
        .arg(source)
        .arg("--output")
        .arg(output);
    for target in targets {
        command.arg("--target").arg(target);
    }
    for root in roots {
        command.arg("--include").arg(root);
    }
    command
}

fn require_success(output: Output) {
    assert!(
        output.status.success(),
        "fixture action failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn names(directory: &Path) -> Vec<String> {
    let mut names = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn build_fixture_exports_complete_target_set_without_inherited_runtime_authority() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("Fixture.hs");
    std::fs::write(
        &source,
        "module Fixture where\nfirst :: Int\nfirst = 42\nsecond :: Int\nsecond = 43\n",
    )
    .unwrap();
    let cache = root.path().join("runtime-cache");
    std::fs::create_dir(&cache).unwrap();
    std::fs::write(cache.join("sentinel"), b"retained private cache").unwrap();
    let output = root.path().join("prepared");
    let mut command = compiler(&source, &output, &["first", "second"], &[]);
    command
        .env("TMPDIR", root.path().join("absent-ambient-scratch"))
        .env("GHCRTS", "invalid inherited runtime options")
        .env("XDG_CACHE_HOME", &cache)
        .env("TIDEPOOL_COMPILE_CACHE_DIR", &cache)
        .env("TIDEPOOL_BUILD_PRODUCTS_DIR", &cache)
        .env(
            "TIDEPOOL_COMPILER_MODULES",
            root.path().join("invalid-runtime-catalog"),
        )
        .env(
            "TIDEPOOL_EXTRACT_DAEMON_SOCKET",
            root.path().join("invalid-runtime-daemon"),
        );
    require_success(command.output().unwrap());
    assert_eq!(names(&cache), ["sentinel"]);
    assert_eq!(
        std::fs::read(cache.join("sentinel")).unwrap(),
        b"retained private cache"
    );
    assert_eq!(
        names(&output),
        [
            "first.asks.json",
            "first.prepared.cbor",
            "meta.cbor",
            "second.asks.json",
            "second.prepared.cbor"
        ]
    );
    for target in ["first", "second"] {
        let bytes = std::fs::read(output.join(format!("{target}.prepared.cbor"))).unwrap();
        PreparedArtifact::parse(bytes, DecodeLimits::default()).unwrap();
    }
}

#[test]
fn build_fixture_obeys_ordered_source_roots_and_refuses_partial_target_sets() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("Fixture.hs");
    std::fs::write(
        &source,
        "module Fixture where\nimport Support\nresult :: Int\nresult = value\n",
    )
    .unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    for (directory, value) in [(&first, 41), (&second, 42)] {
        std::fs::create_dir(directory).unwrap();
        std::fs::write(
            directory.join("Support.hs"),
            format!("module Support where\nvalue :: Int\nvalue = {value}\n"),
        )
        .unwrap();
    }
    let first_output = root.path().join("first-prepared");
    let second_output = root.path().join("second-prepared");
    require_success(
        compiler(
            &source,
            &first_output,
            &["result"],
            &[first.clone(), second.clone()],
        )
        .output()
        .unwrap(),
    );
    require_success(
        compiler(
            &source,
            &second_output,
            &["result"],
            &[second, first.clone()],
        )
        .output()
        .unwrap(),
    );
    assert_ne!(
        std::fs::read(first_output.join("result.prepared.cbor")).unwrap(),
        std::fs::read(second_output.join("result.prepared.cbor")).unwrap()
    );
    let refused_output = root.path().join("refused-prepared");
    let refusal = compiler(&source, &refused_output, &["result", "missing"], &[first])
        .output()
        .unwrap();
    assert!(
        !refusal.status.success(),
        "missing requested target must fail the complete action"
    );
    assert!(
        !refused_output.exists(),
        "refused action must export no partial target set"
    );
}
