//! Canonical on-disk path resolution for Tidepool.
//!
//! Three scopes:
//! - regenerable **cache** ([`cache_dir`]) — the materialized bundled stdlib, the
//!   generated `Tidepool.Effects` module, and the compiled-artifact memo cache;
//! - user-global **config** ([`config_dir`]) — the authored verb `lib/`,
//!   `secrets/`, and `config.toml`;
//! - **project-local** state — `.tidepool/` discovered by walking up from the
//!   launch CWD ([`find_project_root`]), git-style.
//!
//! The launch CWD remains the Fs/Exec sandbox and is intentionally NOT resolved
//! here. Env overrides honored: `XDG_CACHE_HOME`, `XDG_CONFIG_HOME`,
//! `TIDEPOOL_CONFIG_DIR`. The legacy single-home root `~/.tidepool` is honored if
//! it exists, so setups that predate the XDG split keep working.

use std::path::{Path, PathBuf};

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Regenerable cache root: `$XDG_CACHE_HOME/tidepool` → `~/.cache/tidepool` →
/// `$TMPDIR/tidepool` (last resort). NOT `$TMPDIR` proper — macOS reaps that out
/// from under a long-running server.
pub fn cache_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(d).join("tidepool");
    }
    if let Some(h) = home() {
        return h.join(".cache").join("tidepool");
    }
    std::env::temp_dir().join("tidepool")
}

/// User-global config root: `$TIDEPOOL_CONFIG_DIR` → `$XDG_CONFIG_HOME/tidepool`
/// → `~/.config/tidepool` → `$TMPDIR/tidepool-config` (last resort). Holds the
/// global verb `lib/`, `secrets/`, and `config.toml`.
pub fn config_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("TIDEPOOL_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    if let Some(d) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(d).join("tidepool");
    }
    if let Some(h) = home() {
        return h.join(".config").join("tidepool");
    }
    std::env::temp_dir().join("tidepool-config")
}

/// Legacy single-home root (`~/.tidepool`), honored only if it exists so setups
/// predating the XDG split keep resolving their `lib/`/`secrets/`.
fn legacy_dir() -> Option<PathBuf> {
    home().map(|h| h.join(".tidepool")).filter(|d| d.is_dir())
}

/// Content-addressed dir for the materialized bundled stdlib. Keyed on the
/// embedded content hash so a changed binary writes a fresh tree and an identical
/// one reuses it — no version-stamp staleness.
pub fn stdlib_dir(content_hash: &str) -> PathBuf {
    cache_dir().join("stdlib").join(content_hash)
}

/// Staging dir for the generated `Tidepool.Effects` module.
pub fn effects_dir() -> PathBuf {
    cache_dir().join("effects")
}

/// Existing user-global verb-library dirs, in search precedence (canonical config
/// first, then legacy `~/.tidepool/lib`). Only existing dirs are returned.
pub fn global_lib_dirs() -> Vec<PathBuf> {
    existing_roots("lib")
}

/// Existing user-global secrets dirs (canonical config first, then legacy).
pub fn global_secrets_dirs() -> Vec<PathBuf> {
    existing_roots("secrets")
}

fn existing_roots(leaf: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let primary = config_dir().join(leaf);
    if primary.is_dir() {
        out.push(primary);
    }
    if let Some(legacy) = legacy_dir().map(|d| d.join(leaf)) {
        if legacy.is_dir() && !out.contains(&legacy) {
            out.push(legacy);
        }
    }
    out
}

/// Walk up from `start` to the filesystem root, returning the nearest ancestor
/// that contains a `.tidepool/` directory (git-style project discovery). `None`
/// if launched outside any project.
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        if dir.join(".tidepool").is_dir() {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

/// What [`load_secrets`] did — callers log with their own subscriber
/// (this crate has no tracing dependency).
#[derive(Debug, Default)]
pub struct SecretsReport {
    /// Env-var names set from secrets files.
    pub loaded: Vec<String>,
    /// Files skipped: bad name, empty contents, or the var was already set.
    pub ignored: Vec<String>,
}

/// Load `*_API_KEY` secrets files into the process environment: project-local
/// `.tidepool/secrets/` (walk-up from CWD) first, then the user-global dirs.
/// An already-set env var wins, so the FIRST source to provide a key takes
/// precedence — project overrides global. Shared by BOTH server binaries
/// (`tidepool` and `tidepool-repl`) so the effect stacks see the same keys.
pub fn load_secrets() -> SecretsReport {
    let mut report = SecretsReport::default();
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(root) = find_project_root(&cwd) {
            load_secrets_from(&root.join(".tidepool").join("secrets"), &mut report);
        }
    }
    for dir in global_secrets_dirs() {
        load_secrets_from(&dir, &mut report);
    }
    report
}

fn load_secrets_from(dir: &Path, report: &mut SecretsReport) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return; // no secrets dir — nothing to do
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let valid_name = name.ends_with("_API_KEY")
            && name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        let already_set = std::env::var_os(&name).is_some_and(|v| !v.is_empty());
        if !valid_name || already_set {
            report.ignored.push(format!("{}/{name}", dir.display()));
            continue;
        }
        match std::fs::read_to_string(entry.path()) {
            Ok(contents) if !contents.trim().is_empty() => {
                std::env::set_var(&name, contents.trim());
                report.loaded.push(name);
            }
            _ => report.ignored.push(format!("{}/{name}", dir.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_project_root_walks_up() {
        let tmp = std::env::temp_dir().join(format!("tp-paths-{}", std::process::id()));
        let nested = tmp.join("a").join("b").join("c");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(tmp.join(".tidepool")).unwrap();
        // From a deep subdir, the nearest ancestor with .tidepool/ is `tmp`.
        assert_eq!(find_project_root(&nested), Some(tmp.clone()));
        // No .tidepool above a bare temp path → None.
        let orphan = std::env::temp_dir().join(format!("tp-orphan-{}", std::process::id()));
        std::fs::create_dir_all(&orphan).unwrap();
        assert_eq!(find_project_root(&orphan), None);
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&orphan);
    }
}
