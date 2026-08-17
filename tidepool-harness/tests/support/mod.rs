//! Shared test support for the self-iterating harness's integration tests.

#![allow(dead_code)]

use tempfile::TempDir;

/// Isolate this test process's MUTABLE cache state, while SHARING the
/// content-addressed compile memo with every other test process.
///
/// Two jobs, deliberately split (`plans/compile-memo.md`):
///
/// - **Mutable state is per-test.** `XDG_CACHE_HOME` points at a fresh temp
///   directory, so a `SelfHarnessDriver`'s checkpoint/transcript/log writes
///   (and its nested `Harness`'s KV path, the generated effects module, the
///   materialized stdlib) — everything `tidepool_runtime::paths::cache_dir`
///   resolves — land under the returned `TempDir` instead of the real user
///   cache. That isolation is the whole point: those are the files one test
///   could genuinely observe another's writes through.
/// - **The compile memo is shared.** `TIDEPOOL_COMPILE_CACHE_DIR` points at a
///   stable directory derived from the AMBIENT cache home, read BEFORE the
///   isolation above — so `XDG_CACHE_HOME=$PWD/.cache scripts/battery.sh …`
///   puts it under `$PWD/.cache`, and an explicitly-set
///   `TIDEPOOL_COMPILE_CACHE_DIR` is honored as-is. Without this, every one of
///   the ~200 harness test processes recompiles the same boot/answerer/render
///   sources from scratch and re-hashes the ~79MB extract binary, which the
///   census measured as the dominant suite cost.
///
/// **Why sharing is safe:** the memo is content-addressed. Two tests reach the
/// same entry only when the source content, the allowlisted argv, the include
/// CONTENT and the extract binary content are all identical — in which case
/// they are the same compilation and are entitled to the same bytes. A test
/// that writes a different fixture gets different include content and
/// therefore a different key; there is no path, pid, or ordering input by
/// which one test can observe another's state. Concurrent processes are safe
/// for the reason `.config/nextest.toml` already records: artifacts are
/// persisted by atomic rename and the sentinel is written last and checked
/// first, so a reader racing a writer sees a clean miss, never a torn file.
///
/// Call this FIRST in any test that touches `SelfHarnessDriver`/`Harness`,
/// before constructing either: `cache_dir()` re-reads the env var on every
/// call (no caching), but a driver's `checkpoint_path` field is captured at
/// `SelfHarnessDriver::new` time, so isolating late is too late.
/// `env::set_var` is safe here because nextest gives each test its own
/// process. Keep the returned `TempDir` alive for the test's duration —
/// dropping it deletes the directory (the shared memo is NOT under it, and
/// deliberately outlives the test).
pub fn isolate_cache() -> TempDir {
    // Resolved from the ambient environment, BEFORE XDG_CACHE_HOME is
    // redirected below — otherwise the "shared" memo would land inside this
    // test's own private tempdir and be shared with nobody.
    // It is the ambient `cache_dir()` itself, not a test-only subdir, so the
    // isolating tests share one memo with the harness tests that never call
    // this (`finalize_type_pinning`, `acceptance_multi_target`, …) and with a
    // developer's real server — the entries are identical bytes for identical
    // compilations either way, and a second memo would only fragment the win.
    if std::env::var_os("TIDEPOOL_COMPILE_CACHE_DIR").is_none() {
        std::env::set_var(
            "TIDEPOOL_COMPILE_CACHE_DIR",
            tidepool_runtime::paths::cache_dir(),
        );
    }
    let scratch = tempfile::tempdir().expect("scratch tempdir for XDG_CACHE_HOME");
    std::env::set_var("XDG_CACHE_HOME", scratch.path());
    scratch
}

/// Force a COLD compile memo for the current test process: point
/// `TIDEPOOL_COMPILE_CACHE_DIR` at a fresh temp directory nothing else can
/// have written.
///
/// The inverse of [`isolate_cache`]'s sharing, and needed by exactly one kind
/// of test: one that MEASURES compile cost. `acceptance_boot_compile_count`
/// asserts how many `tidepool-extract` spawns a launch pays before the first
/// model call — a receipt about the BOOT PATH, which a shared memo would turn
/// into a receipt about cache state (observed 0 on a warm run, 2 on a cold
/// one). Its own constant already says "clean cache"; this makes the test
/// enforce that rather than assume it.
///
/// Keep the returned `TempDir` alive for the test's duration. Call it BEFORE
/// [`isolate_cache`] if a test wants both — `isolate_cache` honors an
/// already-set `TIDEPOOL_COMPILE_CACHE_DIR`.
pub fn isolate_compile_memo() -> TempDir {
    let scratch = tempfile::tempdir().expect("scratch tempdir for TIDEPOOL_COMPILE_CACHE_DIR");
    std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", scratch.path());
    scratch
}

/// A `$TIDEPOOL_EXTRACT` override that fails UN-RESCUABLY, on any machine —
/// a real, executable, readable file whose content is garbage, rather than a
/// nonexistent path.
///
/// `tidepool_runtime::toolchain::extract_command_name()` (what builds
/// `EngineConfig::extract_bin`) is STRICT: a NONEXISTENT path already fails
/// loudly at resolution, before a binary is ever spawned. This fixture is for
/// tests that want the failure to happen LATER, at spawn/exec time — a file
/// that EXISTS and is readable resolves cleanly (`resolve_bin` returns
/// `Ok(BinSource::Env)`), so the garbage content fails at spawn/exec time
/// instead (`ENOEXEC` or similar), deterministic on every machine. Keep the
/// returned `TempDir` alive for as long as the path must stay valid.
pub fn poisoned_extract_bin() -> (TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("scratch tempdir for a poisoned extract binary");
    let path = dir.path().join("tidepool-extract-poisoned");
    std::fs::write(&path, b"not a real executable\n").expect("write poisoned extract binary");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path)
            .expect("stat poisoned extract binary")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod +x poisoned extract binary");
    }
    (dir, path)
}

/// True iff `TIDEPOOL_EXTRACT` is set or a working toolchain is derivable —
/// delegates to the shared harness helper, which also derives + installs
/// `TIDEPOOL_EXTRACT` (via `cabal list-bin`) when it isn't already set.
pub fn extract_available() -> bool {
    tidepool_testing::eval_harness::extract_available()
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
