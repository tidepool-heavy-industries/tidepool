//! `scripts/battery-shard.sh`'s header comment documents, per GHC-heavy
//! crate, the `-E 'binary(...) or ...'` sub-shard groups that together walk
//! full test coverage within the environment's ~380s survivable-shard
//! budget. That roster is comment text, not code — nothing enforced it
//! against the crates' actual `tests/*.rs` binaries, so it drifted: a
//! developer follows the documented shard recipe, gets an all-green result,
//! and believes the crate has full coverage, while a newly-added binary was
//! never selected by any group and is silently untested by the "full
//! coverage" walk.
//!
//! This lives in `tidepool-extract-cmd` (a pure-Rust, GHC-free crate) rather
//! than the GHC-heavy crates it audits, and runs in the FAST default nextest
//! tier for exactly that reason: the guard must fire the moment a new
//! `tests/*.rs` binary lands, on an ordinary `cargo nextest run`, not only
//! on the rare direct `--ignore-default-filter` invocation that would
//! actually exercise the drifted shard.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The crates `battery-shard.sh` sub-shards, and their doc-block header
/// prefix in the script's comment (`grep`ped, not executed).
const SHARDED_CRATES: &[(&str, &str)] = &[
    ("tidepool-harness", "# tidepool-harness ("),
    ("tidepool-runtime", "# tidepool-runtime ("),
    ("tidepool-repl", "# tidepool-repl ("),
];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-extract-cmd sits directly under the workspace root")
        .to_path_buf()
}

/// Every top-level `tests/<name>.rs` file's `<name>` for `crate_dir` — each
/// is its own cargo-discovered integration test binary (no `[[test]]`
/// stanzas or `autotests = false` override any of the three sharded
/// crates' manifests, so the default one-binary-per-file discovery applies
/// uniformly).
fn actual_test_binaries(crate_dir: &Path) -> BTreeSet<String> {
    let tests_dir = crate_dir.join("tests");
    let mut out = BTreeSet::new();
    let Ok(entries) = std::fs::read_dir(&tests_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|e| e == "rs") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                out.insert(stem.to_string());
            }
        }
    }
    out
}

/// The `binary(name)` names documented for `header_prefix`'s crate — the
/// lines from that crate's `# tidepool-<crate> (N shards):` header up to
/// (not including) the next `# tidepool-` header or the script's first
/// executable line, matching how a human reads the comment block.
fn documented_binaries(script: &str, header_prefix: &str) -> BTreeSet<String> {
    let start = script
        .find(header_prefix)
        .unwrap_or_else(|| panic!("battery-shard.sh has no {header_prefix:?} header"));
    let after_header = &script[start + header_prefix.len()..];
    let end = after_header
        .find("\n# tidepool-")
        .or_else(|| after_header.find("\nset -euo pipefail"))
        .unwrap_or(after_header.len());
    let block = &after_header[..end];

    let mut out = BTreeSet::new();
    let mut rest = block;
    while let Some(i) = rest.find("binary(") {
        let after = &rest[i + "binary(".len()..];
        let close = after
            .find(')')
            .expect("battery-shard.sh has an unclosed binary(...) term");
        out.insert(after[..close].to_string());
        rest = &after[close..];
    }
    out
}

#[test]
fn battery_shard_groups_cover_every_test_binary() {
    let root = workspace_root();
    let script_path = root.join("scripts/battery-shard.sh");
    let script = std::fs::read_to_string(&script_path).expect("read scripts/battery-shard.sh");

    let mut problems = Vec::new();
    let mut total_actual = 0usize;

    for (crate_name, header_prefix) in SHARDED_CRATES {
        let actual = actual_test_binaries(&root.join(crate_name));
        assert!(
            actual.len() > 3,
            "{crate_name}: found only {} tests/*.rs binaries — the discovery glob broke",
            actual.len()
        );
        total_actual += actual.len();
        let documented = documented_binaries(&script, header_prefix);

        let missing: Vec<&String> = actual.difference(&documented).collect();
        if !missing.is_empty() {
            problems.push(format!(
                "{crate_name}: {} binary(ies) exist under tests/ but are absent from every \
                 documented shard group in scripts/battery-shard.sh — add each to a group \
                 (or a new one) so `scripts/battery-shard.sh {crate_name} -E '...'` full \
                 coverage actually reaches it: {:?}",
                missing.len(),
                missing
            ));
        }

        let stale: Vec<&String> = documented.difference(&actual).collect();
        if !stale.is_empty() {
            problems.push(format!(
                "{crate_name}: {} documented binary(ies) in scripts/battery-shard.sh no longer \
                 exist under tests/ (renamed or deleted) — update the shard group: {:?}",
                stale.len(),
                stale
            ));
        }
    }

    // A scan that scanned nothing passes vacuously — pin that it did not.
    assert!(
        total_actual > 50,
        "scan looks vacuous: only {total_actual} test binaries found across all sharded crates"
    );

    assert!(
        problems.is_empty(),
        "scripts/battery-shard.sh's documented shard groups have drifted from the actual \
         tests/*.rs binaries — a shard-by-shard walk following the documented groups would \
         silently skip (or reference nonexistent) test binaries:\n\n{}",
        problems.join("\n\n")
    );
}
