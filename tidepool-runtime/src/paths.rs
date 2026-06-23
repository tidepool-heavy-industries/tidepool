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
