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

fn worker_fixture_source() -> String {
    let request =
        ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved("unused-fixture-worker"));
    let argv = request.worker_argv();
    include_str!("../src/fixtures/build_products_worker.rs")
        .replace("@WORKER_REQUEST_FLAG@", argv[0].to_str().unwrap())
}

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
    fs::write(&source, worker_fixture_source()).unwrap();
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
    fs::write(&source, worker_fixture_source()).unwrap();
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
    let owned_command = |root: &Path| {
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
            .arg(root);
        command
    };
    let invocation = |root: &Path, snapshot: &Path, code: u8| {
        let mut command = owned_command(root);
        command
            .arg("--")
            .arg(&worker)
            .arg("--observe-owned-environment")
            .arg(root.join("owned-service.json"))
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
        assert_eq!(live["complete"], false);
        assert_eq!(live["action"]["exit_code"], serde_json::Value::Null);
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
            serde_json::from_slice(&fs::read(root.join("owned-service.json")).unwrap()).unwrap();
        assert_eq!(settled["complete"], true);
        assert_eq!(settled["action"]["exit_code"], code);
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
    let interrupted_root = dir.path().join("interrupted-owner");
    let interrupted = owned_command(&interrupted_root)
        .arg("--")
        .arg(std::env::var_os("TIDEPOOL_TEST_BASH").expect("declared test Bash is required"))
        .args([
            "-c",
            r#"kill "$TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PID"; exit 37"#,
        ])
        .output()
        .unwrap();
    assert!(!interrupted.status.success());
    let interrupted_lifecycle: serde_json::Value =
        serde_json::from_slice(&fs::read(interrupted_root.join("owned-service.json")).unwrap())
            .unwrap();
    assert_eq!(interrupted_lifecycle["action"]["exit_code"], 37);
    assert_eq!(interrupted_lifecycle["action"]["status"], "exited");
    assert_eq!(interrupted_lifecycle["complete"], false);
    assert!(interrupted_root.join("cache").exists());
    let diagnostic = String::from_utf8_lossy(&interrupted.stderr);
    assert!(diagnostic.contains("owned compiler action"), "{diagnostic}");
    assert!(
        diagnostic.contains("retirement not observed") || diagnostic.contains("service control"),
        "{diagnostic}"
    );
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

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).into()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn frame_path(bytes: &mut Vec<u8>, path: &Path) {
    let path = path.as_os_str().as_encoded_bytes();
    bytes.extend_from_slice(&(path.len() as u32).to_le_bytes());
    bytes.extend_from_slice(path);
}

#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "real owned frontend/provider refusal and post-acquisition settlement controls"
)]
fn configured_startup_failures_retain_acquisition_and_settle_exact_owner() {
    let temporary = tempfile::tempdir().unwrap();
    let retained_directory = std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT").map(|root| {
        std::path::PathBuf::from(root).join("owned-compiler-control-configured-source")
    });
    if let Some(directory) = &retained_directory {
        fs::create_dir(directory).unwrap();
    }
    let directory = retained_directory
        .as_deref()
        .unwrap_or_else(|| temporary.path());
    let worker_source = directory.join("worker.rs");
    fs::write(&worker_source, worker_fixture_source()).unwrap();
    let worker = directory.join("worker");
    assert!(Command::new("rustc")
        .args(["--edition=2021"])
        .arg(&worker_source)
        .arg("-o")
        .arg(&worker)
        .status()
        .unwrap()
        .success());
    let python = fs::canonicalize(
        std::env::var_os("TIDEPOOL_TEST_PYTHON").expect("declared provider Python is required"),
    )
    .unwrap();
    assert!(python.starts_with("/nix/store"));
    let python_object = Path::new("/nix/store").join(
        python
            .strip_prefix("/nix/store")
            .unwrap()
            .components()
            .next()
            .unwrap(),
    );
    let python_sha = sha256(&fs::read(&python).unwrap());
    let provider = directory.join("provider.py");
    let provider_bytes = include_bytes!("fixtures/service_provider.py");
    fs::write(&provider, provider_bytes).unwrap();
    let frontend = env!("CARGO_BIN_EXE_tidepool-extract");
    for mode in ["preboot-refusal", "profile-failure", "release-refusal"] {
        let owner = directory.join(mode);
        let record = directory.join(format!("{mode}-source.json"));
        // The executable deliberately is not a TPSPF001 package profile. Its
        // valid immutable digest lets acquisition succeed before profile decode.
        let record_bytes=serde_json::to_vec(&serde_json::json!({"schema":2,"kind":"compiler-service-package-source","source_id":hex(&[2;32]),"profile_id":hex(&[3;32]),"provider_guard_store_path":python_object,"profile_store_path":python,"profile_sha256":hex(&python_sha),"acquisition_roots":[python_object],"fixture_mode":mode})).unwrap();
        fs::write(&record, &record_bytes).unwrap();
        let mut config = b"TPSCF002".to_vec();
        config.push(0);
        frame_path(&mut config, &python);
        config.extend_from_slice(&python_sha);
        frame_path(&mut config, &provider);
        config.extend_from_slice(&sha256(provider_bytes));
        frame_path(&mut config, &record);
        config.extend_from_slice(&sha256(&record_bytes));
        config.extend_from_slice(&[2; 32]);
        config.extend_from_slice(&[3; 32]);
        let config_path = directory.join(format!("{mode}-launch.bin"));
        fs::write(&config_path, &config).unwrap();
        let marker = owner.join("workload-ran");
        let mut command = Command::new(frontend);
        for key in [
            "TIDEPOOL_EXTRACT_DAEMON_SOCKET",
            "TIDEPOOL_EXTRACT_REQUIRED_DAEMON_ENDPOINT",
            "TIDEPOOL_EXTRACT_NO_DAEMON",
            "TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PID",
            "TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PRODUCER",
            "TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_EPOCH",
            "TIDEPOOL_OWNED_COMPILER_SERVICE",
            "LD_PRELOAD",
            "LD_AUDIT",
            "LD_LIBRARY_PATH",
        ] {
            command.env_remove(key);
        }
        let output = command
            .env("TIDEPOOL_EXTRACT", frontend)
            .env("TIDEPOOL_EXTRACT_WORKER", &worker)
            .env("TIDEPOOL_GHC_LIBDIR", "unused-by-fixture")
            .env("TIDEPOOL_COMPILER_DEPLOYMENT", "configured-source-fixture")
            .env("TIDEPOOL_COMPILER_SERVICE_SOURCE_CONFIG", &config_path)
            .env(
                "TIDEPOOL_COMPILER_SERVICE_SOURCE_CONFIG_SHA256",
                hex(&sha256(&config)),
            )
            .arg("--owned-daemon-run")
            .arg(&owner)
            .arg("--")
            .arg(std::env::var_os("TIDEPOOL_TEST_BASH").unwrap())
            .args(["-c", r#"touch "$1""#, "fixture"])
            .arg(&marker)
            .output()
            .unwrap();
        fs::write(
            directory.join(format!("{mode}.frontend.stdout")),
            &output.stdout,
        )
        .unwrap();
        fs::write(
            directory.join(format!("{mode}.frontend.stderr")),
            &output.stderr,
        )
        .unwrap();
        assert!(
            !output.status.success(),
            "{mode} must retain startup failure"
        );
        assert!(
            !marker.exists(),
            "workload cannot start before source admission"
        );
        let report: serde_json::Value =
            serde_json::from_slice(&fs::read(owner.join("owned-service.json")).unwrap()).unwrap();
        assert_eq!(report["schema"], 2);
        assert_eq!(report["action"]["status"], "failed");
        assert_eq!(report["daemon_reap"]["status"], "reaped");
        assert_eq!(report["daemon_reap"]["success"], false);
        assert_eq!(report["service_terminal"]["workers_retired"], true);
        assert_eq!(report["service_terminal"]["socket_retired"], true);
        assert!(
            report["endpoint"].is_null(),
            "terminal before Ready remains valid"
        );
        assert_eq!(
            report["complete"],
            mode != "release-refusal",
            "mode={mode}; report={report}; frontend stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            report["source_settlement"]["status"],
            if mode == "release-refusal" {
                "retained"
            } else {
                "settled"
            }
        );
        let roots: Vec<serde_json::Value> =
            serde_json::from_slice(&fs::read(owner.join("fixture-registrations.json")).unwrap())
                .unwrap();
        assert_eq!(roots.len(), 1);
        for root in roots {
            let path = Path::new(root["path"].as_str().unwrap());
            assert_eq!(path.symlink_metadata().is_ok(), mode == "release-refusal");
        }
        if mode == "preboot-refusal" {
            assert_eq!(report["acquisition"]["status"], "refused");
            assert_eq!(report["acquisition"]["settlement"], "preboot_released");
            assert!(String::from_utf8_lossy(&output.stderr)
                .contains("injected refusal after fixture root registrations"));
        } else {
            assert_eq!(report["acquisition"]["status"], "acquired");
            assert!(!report["service_terminal"]["failures"]
                .as_array()
                .unwrap()
                .is_empty());
            assert!(owner.join("package-source/receipt.json").is_file());
        }
        if mode == "release-refusal" {
            assert!(!report["settlement_failures"].as_array().unwrap().is_empty());
            assert!(String::from_utf8_lossy(&output.stderr)
                .contains("service package source release failed"));
        }
    }
}
