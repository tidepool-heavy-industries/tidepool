//! Focused real-GHC witness. Run with the matched TIDEPOOL_EXTRACT_WORKER in
//! the repository's pinned dev shell. The frontend comes from this test build.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "focused integration assertions"
)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::time::{Duration, Instant};

struct OwnedDaemon(Child);
impl Drop for OwnedDaemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "exercise the compiled frontend's process boundary"
)]
fn compile(socket: Option<&Path>, input: &Path, out: &Path, products: &Path) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tidepool-extract"));
    if let Some(socket) = socket {
        command.arg("--connect").arg(socket);
    }
    command
        .arg(input)
        .arg("--output-dir")
        .arg(out)
        .arg("--target")
        .arg("result")
        .arg("--build-products-dir")
        .arg(products);
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn copy_products(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_products(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "launch an owned test daemon and wait for readiness"
)]
fn concurrent_same_module_products_match_direct_and_survive_rotation() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let socket = dir.path().join("daemon.sock");
    let trace = dir.path().join("trace.jsonl");
    let products = dir.path().join("products");
    let mut daemon = OwnedDaemon(
        Command::new(env!("CARGO_BIN_EXE_tidepool-extract"))
            .args([
                "--daemon",
                "--persistent",
                "--workers",
                "2",
                "--rotate-after",
                "1",
                "--rss-ceiling-mb",
                "8192",
                "--request-deadline-secs",
                "60",
            ])
            .arg("--socket")
            .arg(&socket)
            .arg("--log-path")
            .arg(trace.with_extension("log"))
            .spawn()
            .unwrap(),
    );
    let started = Instant::now();
    while tidepool_extract_cmd::preflight_compiler_daemon(&socket).is_err() {
        assert!(
            daemon.0.try_wait().unwrap().is_none(),
            "daemon exited before readiness"
        );
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "daemon readiness expired"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let inputs: Vec<PathBuf> = [11, 29]
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let root = dir.path().join(index.to_string());
            fs::create_dir_all(&root).unwrap();
            let input = root.join("Expr.hs");
            fs::write(
                &input,
                format!("module Expr where\nresult :: Int\nresult = {value}\n"),
            )
            .unwrap();
            input
        })
        .collect();
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let handles: Vec<_> = inputs
            .iter()
            .enumerate()
            .map(|(i, input)| {
                let barrier = &barrier;
                let socket = &socket;
                let products = &products;
                let out = dir.path().join(format!("daemon-{i}"));
                scope.spawn(move || {
                    barrier.wait();
                    compile(Some(socket), input, &out, products)
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
    });
    let daemon_artifacts: Vec<_> = (0..2)
        .map(|i| fs::read(dir.path().join(format!("daemon-{i}/result.prepared.cbor"))).unwrap())
        .collect();
    assert_ne!(
        daemon_artifacts[0], daemon_artifacts[1],
        "different source literals must remain distinct"
    );
    for (i, input) in inputs.iter().enumerate() {
        let out = dir.path().join(format!("direct-{i}"));
        let direct = compile(None, input, &out, &products);
        assert!(String::from_utf8_lossy(&direct.stderr)
            .lines()
            .any(|line| line.starts_with("tidepool-build-products ")));
        assert_eq!(
            daemon_artifacts[i],
            fs::read(out.join("result.prepared.cbor")).unwrap()
        );
    }
    // Both one-request slots have rotated. Repeating a request must use a
    // previously written slot directory rather than create a cold namespace.
    compile(
        Some(&socket),
        &inputs[0],
        &dir.path().join("rotated"),
        &products,
    );
    assert_eq!(
        daemon_artifacts[0],
        fs::read(dir.path().join("rotated/result.prepared.cbor")).unwrap()
    );
    let products_snapshot = dir.path().join("products-before-stop");
    copy_products(&products, &products_snapshot);
    let stop = Command::new(env!("CARGO_BIN_EXE_tidepool-extract"))
        .arg("--stop-daemon")
        .arg("--socket")
        .arg(&socket)
        .status()
        .unwrap();
    assert!(stop.success());
    assert!(daemon.0.wait().unwrap().success());
    let placements: Vec<serde_json::Value> = fs::read_to_string(&trace)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|row| row["fields"]["message"] == "compiler build products placed")
        .collect();
    assert_eq!(placements.len(), 3);
    let physical: Vec<&str> = placements
        .iter()
        .map(|row| {
            row["fields"]["physical_build_products_dir"]
                .as_str()
                .unwrap()
        })
        .collect();
    assert_ne!(
        physical[0], physical[1],
        "concurrent slots need exclusive products"
    );
    assert!(
        physical[..2].contains(&physical[2]),
        "rotation must retain its slot's directory"
    );
    for path in &physical {
        assert!(
            !Path::new(path).exists(),
            "retired daemon must reclaim its scratch"
        );
        let relative = Path::new(path).strip_prefix(&products).unwrap();
        assert!(products_snapshot.join(relative).join("Expr.hi").is_file());
    }
    let prior = physical[..2]
        .iter()
        .position(|path| *path == physical[2])
        .unwrap();
    assert_ne!(
        placements[prior]["span"]["worker_pid"],
        placements[2]["span"]["worker_pid"]
    );
    assert_eq!(
        fs::read_dir(&products).unwrap().count(),
        0,
        "all retired owners must reclaim their private namespaces"
    );
    // Certificates and prepared artifacts are materialized under each caller's
    // outDir, and must remain usable after private compiler scratch is gone.
    for (i, expected) in daemon_artifacts.iter().enumerate() {
        assert_eq!(
            expected,
            &fs::read(dir.path().join(format!("daemon-{i}/result.prepared.cbor"))).unwrap()
        );
    }
    for row in placements {
        assert_eq!(
            row["fields"]["logical_build_products_root"],
            products.display().to_string()
        );
        assert!(row["span"]["compile_request"].as_str().is_some());
        eprintln!("placement: {}", row);
    }
    eprintln!("real GHC witness: concurrent 2, direct comparisons 2, after rotation 1");
    if std::env::var_os("TIDEPOOL_KEEP_TEST_LOGS").is_some() {
        eprintln!("retained witness files: {}", dir.keep().display());
    }
}
