//! The property this crate exists to hold: NO workspace crate builds its own
//! `tidepool-extract` invocation.
//!
//! This is a source scan rather than a count so any new open-coded site fails.
//!
//! Scope, stated so the scan's silence means something:
//!
//! - Workspace members' `src/` trees — production code. Each file is
//!   truncated at its first `#[cfg(test)]`, because a test module legitimately
//!   probes the toolchain (`extract_available()` runs `<bin> --help` to decide
//!   whether to skip) and never performs an extraction.
//! - `tests/` trees are NOT scanned: an integration test that drives the
//!   extractor directly to pin its wire contract is exercising the extractor,
//!   not compiling through it.
//! - [`ALLOWLIST`] carries the one production exemption, with its reason.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Production files exempt from the scan, and why.
const ALLOWLIST: &[(&str, &str)] = &[(
    "tidepool-testing/src/eval_harness.rs",
    "the toolchain LOCATOR: it runs candidate binaries with NO arguments to \
     see which one prints the usage banner, then installs $TIDEPOOL_EXTRACT \
     for everyone else. It runs before resolution (it is what makes \
     resolution succeed), performs no extraction, and must not touch the \
     spawn counter — a probe never paid a compile.",
)];

/// Substrings that mark a file as being in the extract-spawning business.
const EXTRACT_MARKERS: &[&str] = &["TIDEPOOL_EXTRACT", "tidepool-extract", "extract_bin"];

/// Extract flags — used to pin that the allowlisted probe stays a probe.
const EXTRACT_FLAGS: &[&str] = &[
    "--target",
    "--targets",
    "--turn",
    "--classify",
    "--output-dir",
];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-extract-cmd sits directly under the workspace root")
        .to_path_buf()
}

/// The `members = [...]` list from the workspace manifest, so a new crate is
/// scanned the day it is added rather than the day someone remembers to.
fn workspace_members(root: &Path) -> Vec<PathBuf> {
    let manifest =
        std::fs::read_to_string(root.join("Cargo.toml")).expect("read workspace manifest");
    let body = manifest
        .split_once("members = [")
        .expect("workspace manifest has a members list")
        .1
        .split_once(']')
        .expect("members list is closed")
        .0;
    let members: Vec<PathBuf> = body
        .split(',')
        .filter_map(|entry| {
            let entry = entry.trim().trim_matches('"');
            (!entry.is_empty()).then(|| root.join(entry))
        })
        .collect();
    assert!(
        members.len() > 10,
        "parsed only {} workspace members — the manifest parse broke",
        members.len()
    );
    members
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// A file's production half: everything before its first `#[cfg(test)]`.
fn production_source(text: &str) -> &str {
    match text.find("#[cfg(test)]") {
        Some(i) => &text[..i],
        None => text,
    }
}

#[test]
fn no_workspace_crate_open_codes_an_extract_spawn() {
    let root = workspace_root();
    let this_crate = root.join("tidepool-extract-cmd");
    let allowed: BTreeSet<PathBuf> = ALLOWLIST.iter().map(|(p, _)| root.join(p)).collect();

    let mut scanned = 0usize;
    let mut saw_known_site = false;
    let mut offenders = Vec::new();

    for member in workspace_members(&root) {
        if member == this_crate {
            continue;
        }
        let mut files = Vec::new();
        rust_files(&member.join("src"), &mut files);
        for file in files {
            let text = std::fs::read_to_string(&file).expect("read source file");
            let src = production_source(&text);
            scanned += 1;
            if file == root.join("tidepool-runtime/src/lib.rs") {
                saw_known_site = true;
            }
            if allowed.contains(&file) {
                continue;
            }
            if !src.contains("Command::new") {
                continue;
            }
            if let Some(marker) = EXTRACT_MARKERS.iter().find(|m| src.contains(**m)) {
                offenders.push(format!(
                    "{}: builds a `Command` in a file that also names {marker:?}",
                    file.strip_prefix(&root).unwrap_or(&file).display()
                ));
            }
        }
    }

    // A scan that scanned nothing passes vacuously — pin that it did not.
    assert!(
        scanned > 100 && saw_known_site,
        "scan looks vacuous: {scanned} files, known-site seen = {saw_known_site}"
    );
    assert!(
        offenders.is_empty(),
        "every `tidepool-extract` spawn must go through `tidepool_extract_cmd::ExtractCmd`. \
         Open-coded site(s):\n  {}",
        offenders.join("\n  ")
    );
}

/// The allowlist is an exemption for a PROBE, not a licence to extract. If an
/// allowlisted file grows a real extract argument, the exemption no longer
/// describes it and this fails.
#[test]
fn allowlisted_files_stay_probes() {
    let root = workspace_root();
    for (rel, reason) in ALLOWLIST {
        let path = root.join(rel);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("allowlisted file {rel} is unreadable: {e}"));
        let src = production_source(&text);
        for flag in EXTRACT_FLAGS {
            assert!(
                !src.contains(flag),
                "{rel} carries the extract flag {flag:?}, so it is performing an \
                 extraction rather than the probe its exemption describes:\n  {reason}"
            );
        }
    }
}
