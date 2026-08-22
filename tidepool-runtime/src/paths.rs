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
//! The launch CWD remains the Fs sandbox root — and Exec's initial working
//! directory only; Exec itself is not filesystem-sandboxed (see
//! `tidepool-handlers/CLAUDE.md`'s Sandboxing section) — and is intentionally
//! NOT resolved here. Env overrides honored: `XDG_CACHE_HOME`, `XDG_CONFIG_HOME`,
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

/// Where the content-addressed compiled-artifact memo (and the `binfp-*`
/// binary-fingerprint sidecars) live: `$TIDEPOOL_COMPILE_CACHE_DIR` if set,
/// else [`cache_dir`]. **The default layout is unchanged** — nobody's existing
/// cache moves.
///
/// The override exists so a process can isolate its MUTABLE state
/// (`XDG_CACHE_HOME` → a private tempdir: checkpoints, transcripts, logs, the
/// generated effects module, the materialized stdlib) while still SHARING the
/// memo. That split is only sound because the memo is content-addressed: two
/// writers reach the same entry only when every input that can reach the
/// output bytes is identical, in which case they are the same compilation and
/// are entitled to the same bytes. `tidepool-harness/tests/support`'s
/// `isolate_cache` is the caller that wants exactly this — see
/// `plans/compile-memo.md`.
pub fn compile_cache_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("TIDEPOOL_COMPILE_CACHE_DIR") {
        return PathBuf::from(d);
    }
    cache_dir()
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

/// Persistent, shared `-fwrite-interface` output dir: module-granular GHC
/// recompilation avoidance ACROSS `tidepool-extract` spawns (spike-verified
/// 2026-08-20, `plans/turn-latency-state-injection.md`'s "Direction: toward
/// a resident compile daemon" section) — GHC's own `checkOldIface`
/// recompilation checking skips an unchanged home module (typically every
/// stdlib module a turn doesn't itself edit) when its interface is already
/// sitting in this dir from a PRIOR spawn, instead of redoing
/// parse/typecheck/desugar for it every single time.
///
/// `$TIDEPOOL_BUILD_PRODUCTS_DIR` if set (an isolated dir for a test that
/// needs a genuinely COLD measurement, mirroring [`compile_cache_dir`]'s own
/// override); else content-addressed under [`compile_cache_dir`] — so a test
/// suite that already shares the compile memo via
/// `$TIDEPOOL_COMPILE_CACHE_DIR` shares this dir too, with no extra wiring —
/// keyed by `toolchain_fingerprint` (the resolved extract binary's own
/// content fingerprint, see `toolchain::extract_fingerprint`) so a rebuilt
/// extract binary gets a FRESH directory: staleness is structurally
/// impossible, never validate it by mtime. A stdlib-only edit does NOT need
/// its own fresh directory — GHC's per-module interface hash already detects
/// that (spike-verified: an edited module is selectively recompiled while
/// every OTHER module in the same directory stays skipped) — so this key
/// deliberately does not fold in stdlib content; folding it in would cost a
/// full content walk of the stdlib tree on every compile for a property GHC
/// already guarantees per-module.
pub fn build_products_dir(toolchain_fingerprint: &str) -> PathBuf {
    if let Some(d) = std::env::var_os("TIDEPOOL_BUILD_PRODUCTS_DIR") {
        return PathBuf::from(d);
    }
    compile_cache_dir()
        .join("build-products")
        .join(toolchain_fingerprint)
}

/// Wire the default build-products dir (see [`build_products_dir`]) onto
/// `cmd`, on by default for EVERY `tidepool-extract` spawn in this crate —
/// not just the ones built through `artifacts::compile_invocation`.
/// `tidepool-runtime` has more than one spawn site (`session/turn.rs`'s
/// `run_turn`/`classify_block`/`compile_session_turn` and `session/mod.rs`'s
/// `validate_candidate` build their own `ExtractCmd`s directly, bypassing
/// `compile_invocation`'s memo — a session turn has on-disk side effects and
/// mutable-session dependencies a content-addressed cache would get wrong,
/// see `session/turn.rs`'s module doc), and the module-granular GHC
/// recompilation win this dir gives is orthogonal to that memo: applying it
/// everywhere a `tidepool-extract` gets spawned is what makes "on by
/// default" actually mean every real caller, not just the one with the
/// fanciest doc comment. A no-op if the directory can't be created.
pub fn apply_build_products_dir(cmd: &mut tidepool_extract_cmd::ExtractCmd) {
    let fingerprint = crate::toolchain::extract_fingerprint(Path::new(cmd.launcher().program()));
    let bp_dir = build_products_dir(&fingerprint);
    if std::fs::create_dir_all(&bp_dir).is_ok() {
        cmd.build_products_dir(&bp_dir);
    }
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

    /// `load_secrets_from` loads valid `*_API_KEY` files (trimmed), ignores
    /// bad names / non-key files, and lets an already-set env var win.
    #[test]
    fn load_secrets_from_loads_and_respects_precedence() {
        let dir = std::env::temp_dir().join(format!("tp-secrets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::remove_var("TIDEPOOL_TEST_DUMMY_API_KEY");
        std::fs::write(dir.join("TIDEPOOL_TEST_DUMMY_API_KEY"), "sk-test-123\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "not a key").unwrap();
        std::fs::write(dir.join("lower_api_key"), "nope").unwrap();
        std::env::set_var("TIDEPOOL_TEST_PRESET_API_KEY", "from-env");
        std::fs::write(dir.join("TIDEPOOL_TEST_PRESET_API_KEY"), "from-file").unwrap();

        let mut report = SecretsReport::default();
        load_secrets_from(&dir, &mut report);

        assert_eq!(
            std::env::var("TIDEPOOL_TEST_DUMMY_API_KEY").unwrap(),
            "sk-test-123"
        );
        // already-set var wins over the file
        assert_eq!(
            std::env::var("TIDEPOOL_TEST_PRESET_API_KEY").unwrap(),
            "from-env"
        );
        assert!(std::env::var("notes.txt").is_err());
        assert!(report
            .loaded
            .iter()
            .any(|n| n == "TIDEPOOL_TEST_DUMMY_API_KEY"));

        std::env::remove_var("TIDEPOOL_TEST_DUMMY_API_KEY");
        std::env::remove_var("TIDEPOOL_TEST_PRESET_API_KEY");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_project_root_walks_up() {
        let tmp = std::env::temp_dir().join(format!("tp-paths-{}", std::process::id()));
        let nested = tmp.join("a").join("b").join("c");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(tmp.join(".tidepool")).unwrap();
        // From a deep subdir, the nearest ancestor with .tidepool/ is `tmp`.
        assert_eq!(find_project_root(&nested), Some(tmp.clone()));
        // A bare temp path adds NO project root of its own: its answer is
        // whatever the environment's answer for temp_dir already is (usually
        // None — but a stray `/tmp/.tidepool` left by an unrelated process
        // must not fail THIS test, which pins the walk-up rule, not the
        // box's hygiene; observed live 2026-08-11).
        let orphan = std::env::temp_dir().join(format!("tp-orphan-{}", std::process::id()));
        std::fs::create_dir_all(&orphan).unwrap();
        assert_eq!(
            find_project_root(&orphan),
            find_project_root(&std::env::temp_dir())
        );
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&orphan);
    }
}
