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
#[allow(
    clippy::disallowed_methods,
    reason = "integration control compiles with the declared pinned GHC executable"
)]
fn build_fixture_uses_declared_packages_despite_poisoned_user_databases() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("Fixture.hs");
    std::fs::write(
        &source,
        "module Fixture where\nimport Data.Text (pack, unpack)\nresult :: String\nresult = unpack (pack \"declared package stack\")\n",
    )
    .unwrap();
    let home = root.path().join("home");
    let xdg = root.path().join("data");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&xdg).unwrap();
    let native = || {
        let mut command = Command::new("ghc");
        command
            .current_dir(root.path())
            .args(["-fno-code", "-fforce-recomp", "-v0", "-outputdir"])
            .arg(root.path().join("native-control"))
            .arg(&source)
            .env("HOME", &home)
            .env("XDG_DATA_HOME", &xdg)
            .env("GHC_ENVIRONMENT", "-")
            .env_remove("GHC_PACKAGE_PATH")
            .env_remove("GHCRTS");
        command.output().unwrap()
    };
    require_success(native());
    let clean = root.path().join("clean-prepared");
    require_success(
        compiler(&source, &clean, &["result"], &[])
            .env("HOME", &home)
            .env("XDG_DATA_HOME", &xdg)
            .output()
            .unwrap(),
    );
    // Both GHC 9.12's legacy HOME and current XDG discovery locations are
    // poisoned. The plain GHC refusal establishes that the witness is live.
    for directory in [
        home.join(".ghc/x86_64-linux-9.12.2/package.conf.d"),
        xdg.join("ghc/x86_64-linux-9.12.2/package.conf.d"),
    ] {
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("package.cache"),
            b"invalid ambient package database",
        )
        .unwrap();
    }
    let poisoned = native();
    assert!(
        !poisoned.status.success(),
        "default GHC must observe the poisoned user database control"
    );
    let direct = root.path().join("direct-compiler");
    require_success(
        Command::new(configured("TIDEPOOL_EXTRACT"))
            .current_dir(root.path())
            .arg(&source)
            .arg("--output-dir")
            .arg(&direct)
            .args(["--target", "result"])
            .env("HOME", &home)
            .env("XDG_DATA_HOME", &xdg)
            .env("GHC_ENVIRONMENT", "-")
            .env("GHC_PACKAGE_PATH", root.path().join("undeclared-database"))
            .output()
            .unwrap(),
    );
    PreparedArtifact::parse(
        std::fs::read(direct.join("result.prepared.cbor")).unwrap(),
        DecodeLimits::default(),
    )
    .unwrap();
    let isolated = root.path().join("isolated-prepared");
    require_success(
        compiler(&source, &isolated, &["result"], &[])
            .env("HOME", &home)
            .env("XDG_DATA_HOME", &xdg)
            .env("GHC_PACKAGE_PATH", root.path().join("undeclared-database"))
            .output()
            .unwrap(),
    );
    assert_eq!(names(&clean), names(&isolated));
    for artifact in names(&clean) {
        assert_eq!(
            std::fs::read(clean.join(&artifact)).unwrap(),
            std::fs::read(isolated.join(&artifact)).unwrap(),
            "ambient package discovery changed {artifact}"
        );
    }
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
    let action_scratch = root.path().join("action-scratch");
    std::fs::create_dir(&action_scratch).unwrap();
    let refusal = compiler(&source, &refused_output, &["result", "missing"], &[first])
        .current_dir(root.path())
        .env("BUCK_SCRATCH_PATH", "action-scratch")
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
    let stderr = String::from_utf8_lossy(&refusal.stderr);
    let retained = stderr
        .lines()
        .find_map(|line| line.strip_prefix("fixture action scratch retained at "))
        .map(PathBuf::from)
        .expect("failed CLI action names its retained diagnostic owner");
    assert!(retained.starts_with(&action_scratch));
    let failures = std::fs::read_dir(retained.join("compiler-failures"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(failures.len(), 1);
    let failure = &failures[0];
    let report = std::fs::read(failure.join("compiler.stdout")).unwrap();
    let report = tidepool_toolchain::diag::parse_extract_report(&report, &[]).unwrap();
    assert!(!report.diagnostics.is_empty());
    for diagnostic in report.diagnostics {
        // Debug formatting escapes newlines but retains every diagnostic byte.
        assert!(
            stderr.contains(&format!("{:?}", diagnostic.message)),
            "CLI omitted a compiler diagnostic: {stderr}"
        );
    }
    for artifact in [
        "compiler.stderr",
        "compiler-status.json",
        "compiler-request.bin",
    ] {
        assert!(failure.join(artifact).is_file(), "missing {artifact}");
    }
    // The test owns this failed action and has observed the child exit.
    std::fs::remove_dir_all(retained).unwrap();
}
