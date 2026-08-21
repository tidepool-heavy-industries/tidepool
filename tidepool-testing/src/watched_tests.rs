//! Enforces the watched-tests manifest (`.config/watched-tests.toml`): every
//! GHC-heavy crate's `tests/*.rs` integration binary must be registered
//! there, one line per binary. A `kind(lib)` unit test rather than a
//! `tests/*.rs` integration binary of its own, so it runs on every quick-tier
//! `cargo nextest run` — see `.config/nextest.toml`'s default-filter, which
//! excludes tidepool-testing's `kind(test)` binaries but not its lib tests.
//!
//! Fails fast on any binary present on disk but absent from the manifest,
//! per plans/self-iterating-harness/trunk-battery-sweep-2026-08-18.md's
//! finding that 32 of 33 tidepool-harness binaries were reachable only
//! through the wholesale crate shard — unwatched by any narrower check.

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::{Path, PathBuf};

    /// GHC-heavy crates and the binaries excluded from the required set
    /// because they are NOT GHC-heavy — kept in sync with
    /// `.config/nextest.toml`'s per-binary exceptions (`provider_behavior`,
    /// `tidepool-web`'s non-`crash_recovery` binaries).
    const CRATES: &[(&str, &[&str])] = &[
        ("tidepool-harness", &["provider_behavior"]),
        ("tidepool-handlers", &[]),
        ("tidepool-runtime", &[]),
        ("tidepool-repl", &[]),
        ("tidepool-mcp", &[]),
        ("tidepool-testing", &[]),
        ("tidepool-web", &["operator_gate", "form_api", "tree_view"]),
    ];

    /// Test targets declared via Cargo.toml `[[test]] path = ...` that fall
    /// outside the flat `tests/*.rs` glob (a nested `tests/<name>/mod.rs`).
    const EXTRA_BINARIES: &[(&str, &str)] = &[("tidepool-testing", "haskell_verified")];

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .canonicalize()
            .expect("workspace root")
    }

    fn discovered_binaries(crate_name: &str, exclude: &[&str]) -> BTreeSet<String> {
        let tests_dir = workspace_root().join(crate_name).join("tests");
        let mut found = BTreeSet::new();
        if tests_dir.is_dir() {
            for entry in fs::read_dir(&tests_dir).expect("read tests dir") {
                let path = entry.expect("dir entry").path();
                if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
                    if !exclude.contains(&stem.as_str()) {
                        found.insert(stem);
                    }
                }
            }
        }
        for (c, bin) in EXTRA_BINARIES {
            if *c == crate_name {
                found.insert((*bin).to_string());
            }
        }
        found
    }

    fn manifest() -> BTreeMap<String, BTreeSet<String>> {
        let path = workspace_root().join(".config/watched-tests.toml");
        let text =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let mut sections: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut current: Option<String> = None;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                current = Some(name.to_string());
                sections.entry(name.to_string()).or_default();
                continue;
            }
            if let Some((key, _)) = line.split_once('=') {
                let section = current
                    .as_ref()
                    .unwrap_or_else(|| panic!("binary line before any [section]: {line}"));
                sections
                    .get_mut(section)
                    .unwrap()
                    .insert(key.trim().to_string());
            }
        }
        sections
    }

    #[test]
    fn watched_tests_manifest_covers_every_ghc_heavy_binary() {
        let manifest = manifest();
        let mut missing = Vec::new();
        for (crate_name, exclude) in CRATES {
            let registered = manifest.get(*crate_name).cloned().unwrap_or_default();
            for bin in discovered_binaries(crate_name, exclude) {
                if !registered.contains(&bin) {
                    missing.push(format!("{crate_name}/{bin}"));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "unregistered GHC-heavy test binaries — add a line for each to \
             .config/watched-tests.toml:\n  {}",
            missing.join("\n  ")
        );
    }
}
