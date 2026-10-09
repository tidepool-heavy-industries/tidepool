//! Extractor-free frontend ownership witness using one immutable fake worker.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "focused integration assertions"
)]
use std::fs;
use std::path::Path;
use std::process::Command;
use tidepool_extract_cmd::{ExtractCmd, ResolvedExtractBin};

struct RestoreEnv(Vec<(&'static str, Option<std::ffi::OsString>)>);
impl Drop for RestoreEnv {
    fn drop(&mut self) {
        for (name, previous) in self.0.drain(..) {
            if let Some(value) = previous {
                std::env::set_var(name, value);
            } else {
                std::env::remove_var(name);
            }
        }
    }
}

#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "compile one immutable fixture and exercise its frontend"
)]
fn direct_frontends_report_placement_and_reclaim_scratch_before_reply() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("worker.rs");
    fs::write(
        &source,
        include_str!("../src/fixtures/build_products_worker.rs"),
    )
    .unwrap();
    let worker = dir.path().join("worker");
    assert!(Command::new("rustc")
        .arg("--edition=2021")
        .arg(source)
        .arg("-o")
        .arg(&worker)
        .status()
        .unwrap()
        .success());
    let input = dir.path().join("Expr.hs");
    let products = dir.path().join("products");
    fs::write(&input, "alpha").unwrap();
    fs::create_dir(&products).unwrap();
    fs::write(products.join("caller-owned"), "retain").unwrap();
    // The counted runner isolates this case. Child commands and bound endpoint
    // launch share this setup.
    let _restore = RestoreEnv(
        [
            "TIDEPOOL_EXTRACT_WORKER",
            "TIDEPOOL_GHC_LIBDIR",
            "TIDEPOOL_EXTRACT_NO_DAEMON",
        ]
        .iter()
        .map(|name| (*name, std::env::var_os(name)))
        .collect(),
    );
    std::env::set_var("TIDEPOOL_EXTRACT_WORKER", &worker);
    std::env::set_var("TIDEPOOL_GHC_LIBDIR", "unused-by-fixture");
    std::env::set_var("TIDEPOOL_EXTRACT_NO_DAEMON", "1");
    let frontend = env!("CARGO_BIN_EXE_tidepool-extract");
    let cli = Command::new(frontend)
        .arg(&input)
        .arg("--build-products-dir")
        .arg(&products)
        .output()
        .unwrap();
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let cli_output = String::from_utf8(cli.stdout).unwrap();
    let cli_path = cli_output.lines().next().unwrap();
    assert!(!Path::new(cli_path).exists());
    assert!(String::from_utf8(cli.stderr)
        .unwrap()
        .lines()
        .any(|line| line.starts_with("tidepool-build-products ")));

    let mut command = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved(frontend));
    command.input(&input).build_products_dir(&products);
    let bound = command.bind().unwrap().execute(&command).unwrap();
    assert!(bound.success());
    let bound_output = String::from_utf8(bound.output.stdout).unwrap();
    let bound_path = bound_output.lines().next().unwrap();
    assert_ne!(bound_path, cli_path);
    assert!(
        !Path::new(bound_path).exists(),
        "single direct reply must follow scratch retirement"
    );
    assert!(String::from_utf8(bound.output.stderr)
        .unwrap()
        .lines()
        .any(|line| line.starts_with("tidepool-build-products ")));
    assert_eq!(fs::read_dir(&products).unwrap().count(), 1);
    assert_eq!(
        fs::read_to_string(products.join("caller-owned")).unwrap(),
        "retain"
    );
}

#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "exercise the native process owner with an immutable fixture"
)]
fn owned_daemon_publishes_exact_measurement_identity_and_refuses_inherited_coordinates() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("worker.rs");
    fs::write(
        &source,
        include_str!("../src/fixtures/build_products_worker.rs"),
    )
    .unwrap();
    let worker = dir.path().join("worker");
    assert!(Command::new("rustc")
        .args(["--edition=2021"])
        .arg(&source)
        .arg("-o")
        .arg(&worker)
        .status()
        .unwrap()
        .success());
    let frontend = env!("CARGO_BIN_EXE_tidepool-extract");
    let inherited = [
        "TIDEPOOL_EXTRACT_DAEMON_SOCKET",
        "TIDEPOOL_EXTRACT_REQUIRED_DAEMON_ENDPOINT",
        "TIDEPOOL_EXTRACT_NO_DAEMON",
        "TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PID",
        "TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PRODUCER",
        "TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_EPOCH",
    ];
    let invocation = |root: &Path, snapshot: &Path, code: u8| {
        let mut command = Command::new(frontend);
        for name in inherited {
            command.env_remove(name);
        }
        command
            .env("TIDEPOOL_EXTRACT", frontend)
            .env("TIDEPOOL_EXTRACT_WORKER", &worker)
            .env("TIDEPOOL_GHC_LIBDIR", "unused-by-fixture")
            .env("TIDEPOOL_COMPILER_DEPLOYMENT", "owned-measurement-fixture")
            .arg("--owned-daemon-run")
            .arg(root)
            .arg("--")
            .arg(&worker)
            .arg("--observe-owned-environment")
            .arg(root.join("lifecycle.json"))
            .arg(snapshot)
            .arg(code.to_string());
        command
    };
    for code in [0, 7] {
        let root = dir.path().join(format!("owner-{code}"));
        let snapshot = dir.path().join(format!("live-{code}.json"));
        let output = invocation(&root, &snapshot, code).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(i32::from(code)),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let fields: std::collections::BTreeMap<_, _> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| {
                let (key, value) = line.split_once('=').unwrap();
                (key.to_owned(), value.to_owned())
            })
            .collect();
        let live: serde_json::Value = serde_json::from_slice(&fs::read(snapshot).unwrap()).unwrap();
        assert_eq!(live["cleanup_confirmed"], false);
        assert_eq!(live["exit_code"], serde_json::Value::Null);
        for (key, lifecycle_key) in [
            ("TIDEPOOL_EXTRACT_DAEMON_SOCKET", "socket_path"),
            ("TIDEPOOL_EXTRACT_REQUIRED_DAEMON_ENDPOINT", "endpoint"),
            ("TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PRODUCER", "producer"),
            ("TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_EPOCH", "daemon_epoch"),
        ] {
            assert_eq!(fields[key], live[lifecycle_key].as_str().unwrap());
        }
        assert_eq!(
            fields["TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PID"],
            live["daemon_pid"].to_string()
        );
        assert_eq!(
            fields["TIDEPOOL_PERFORMANCE_COMPILER_TRACE"],
            root.join("compiler.jsonl").display().to_string()
        );
        let records: Vec<serde_json::Value> = fs::read_to_string(root.join("compiler.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let ready: Vec<_> = records
            .iter()
            .filter(|record| record["fields"]["message"] == "compiler daemon ready")
            .collect();
        assert_eq!(ready.len(), 1);
        for key in ["producer", "daemon_pid", "daemon_epoch"] {
            assert_eq!(ready[0]["fields"][key], live[key]);
        }
        let settled: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("lifecycle.json")).unwrap()).unwrap();
        assert_eq!(settled["cleanup_confirmed"], true);
        assert_eq!(settled["exit_code"], code);
        for key in [
            "producer",
            "endpoint",
            "daemon_pid",
            "daemon_epoch",
            "socket_path",
        ] {
            assert_eq!(settled[key], live[key]);
        }
        assert!(!Path::new(&fields["TIDEPOOL_EXTRACT_DAEMON_SOCKET"]).exists());
        assert_eq!(root.join("cache").exists(), code != 0);
    }
    for key in inherited {
        let root = dir.path().join(format!("refused-{key}"));
        let snapshot = dir.path().join(format!("refused-{key}.json"));
        let output = invocation(&root, &snapshot, 0)
            .env(key, "foreign-owner")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8(output.stderr)
            .unwrap()
            .contains(&format!("refuses inherited {key}")));
        assert!(!root.exists());
        assert!(!snapshot.exists());
    }
}
