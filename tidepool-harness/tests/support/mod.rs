//! Shared test support for the self-iterating harness's integration tests.

#![allow(dead_code)]

use tempfile::TempDir;

/// Isolate `XDG_CACHE_HOME` into a fresh temp directory for the CURRENT test
/// process, so a `SelfHarnessDriver`'s checkpoint/transcript/log writes (and
/// its nested `Harness`'s KV path) — everything
/// `tidepool_runtime::paths::cache_dir` resolves — land under the returned
/// `TempDir` instead of the real user cache (`~/.cache/tidepool`).
///
/// Call this FIRST in any test that touches `SelfHarnessDriver`/`Harness`,
/// before constructing either: `cache_dir()` re-reads the env var on every
/// call (no caching), but a driver's `checkpoint_path` field is captured at
/// `SelfHarnessDriver::new` time, so isolating late is too late.
/// `env::set_var` is safe here because nextest gives each test its own
/// process. Keep the returned `TempDir` alive for the test's duration —
/// dropping it deletes the directory.
pub fn isolate_cache() -> TempDir {
    let scratch = tempfile::tempdir().expect("scratch tempdir for XDG_CACHE_HOME");
    std::env::set_var("XDG_CACHE_HOME", scratch.path());
    scratch
}

/// True iff `TIDEPOOL_EXTRACT` is set or a `tidepool-extract` binary is on
/// `PATH`.
fn extract_available() -> bool {
    std::env::var("TIDEPOOL_EXTRACT").is_ok()
        || std::process::Command::new("tidepool-extract")
            .arg("--help")
            .output()
            .is_ok()
}

/// Panic loudly instead of skipping (which nextest reports as PASS) when
/// `TIDEPOOL_EXTRACT` isn't reachable. This GHC-tier suite is excluded from
/// the default nextest filter, so this only fires on a direct
/// `--ignore-default-filter` invocation missing the environment.
pub fn require_extract() {
    if !extract_available() {
        panic!(
            "TIDEPOOL_EXTRACT not set — this GHC-tier test cannot run vacuously. Set \
             TIDEPOOL_EXTRACT (or run inside `nix develop`), or run via scripts/battery.sh, \
             which derives it."
        );
    }
}
