//! Shared test support for the self-iterating harness's integration tests.

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
