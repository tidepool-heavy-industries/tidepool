//! Write (or check) every file the effect schema owns.
//!
//! ```text
//! cargo run -p tidepool-protocol --bin tidepool-protocol-gen
//! cargo run -p tidepool-protocol --bin tidepool-protocol-gen -- --check
//! ```
//!
//! `--check` writes nothing and exits non-zero if any committed file differs,
//! naming each one. The authoritative guard is the `generated_files_are_current`
//! test, not this flag — a check nothing runs is not a check, and the test
//! runner runs tests by construction. `--check` exists for a human who wants
//! the answer without a test harness.

#![warn(clippy::unwrap_used, clippy::expect_used)]
use std::path::PathBuf;
use std::process::ExitCode;

/// The workspace root, from this crate's manifest directory.
#[allow(
    clippy::expect_used,
    reason = "tidepool-protocol must live one level under the workspace root"
)]
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-protocol must live one level under the workspace root")
        .to_path_buf()
}

fn main() -> ExitCode {
    let check_only = std::env::args().any(|a| a == "--check");
    let root = workspace_root();
    let mut stale = Vec::new();
    let mut wrote = 0usize;

    let files = tidepool_protocol::generated_files()
        .into_iter()
        .chain(tidepool_protocol::harness_generated_files())
        .chain(tidepool_protocol::runtime_generated_files())
        .chain(tidepool_protocol::actor_generated_files());
    for f in files {
        let path = root.join(&f.path);
        let current = std::fs::read_to_string(&path).ok();
        if current.as_deref() == Some(f.contents.as_str()) {
            continue;
        }
        if check_only {
            stale.push(f.path.clone());
            continue;
        }
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("could not create {}: {e}", parent.display());
                return ExitCode::FAILURE;
            }
        }
        if let Err(e) = std::fs::write(&path, &f.contents) {
            eprintln!("could not write {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
        println!("wrote {}", f.path);
        wrote += 1;
    }

    if check_only {
        if stale.is_empty() {
            println!("all generated files are current");
            return ExitCode::SUCCESS;
        }
        eprintln!("generated files are STALE:");
        for p in &stale {
            eprintln!("  {p}");
        }
        eprintln!("regenerate with: cargo run -p tidepool-protocol --bin tidepool-protocol-gen");
        return ExitCode::FAILURE;
    }

    if wrote == 0 {
        println!("all generated files were already current");
    }
    ExitCode::SUCCESS
}
