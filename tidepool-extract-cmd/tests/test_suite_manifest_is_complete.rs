//! Keeps the machine-readable GHC-heavy suite manifest synchronized with
//! Cargo's integration-test discovery. The authoritative validator lives in
//! `scripts/` beside the manifest runner; this test makes it part of the
//! ordinary default nextest tier without duplicating its parsing rules.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn test_suite_manifest_is_complete() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate is inside the workspace")
        .to_path_buf();
    let output = Command::new(root.join("scripts/test-suite-check.sh"))
        .current_dir(&root)
        .output()
        .expect("run scripts/test-suite-check.sh");

    assert!(
        output.status.success(),
        "suite manifest validation failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
