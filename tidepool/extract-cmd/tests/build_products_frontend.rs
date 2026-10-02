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
    // This binary has exactly one test; environment changes cannot race a
    // second test. Child commands and bound endpoint launch share this setup.
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
