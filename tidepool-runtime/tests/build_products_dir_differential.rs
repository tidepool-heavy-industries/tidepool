//! Differential pin for the build-products dir (module-granular GHC
//! recompilation avoidance across `tidepool-extract` spawns — spike-verified
//! 2026-08-20, `plans/turn-latency-state-injection.md`): a COLD compile
//! (empty/no build-products dir) and a WARM compile (same source, same
//! extract binary, same include content, but GHC's own `checkOldIface` gets
//! to skip the unchanged `Tidepool.Prelude` closure via a pre-populated
//! build-products dir) must produce byte-identical Core + `DataConTable`.
//!
//! This used to be a known, documented gap (enabling `load'`'s warm-dir skip
//! perturbs GHC's session-wide `Unique` allocation trajectory, and
//! `Tidepool.Translate.localVarId` baked that raw `Unique` into the `VarId`
//! of every NESTED, non-top-level `Id`) — closed by
//! `Tidepool.Translate.stabilizeLocalUniques` (nested Ids) together with
//! `GhcPipeline.hs`'s `externalizeInternalTops` ordinal-based disambiguator
//! (internal top-level floats). See both functions' doc comments and
//! `plans/turn-latency-state-injection.md` for the full history.

use serial_test::serial;
use std::env;
use std::ffi::OsStr;
use tempfile::TempDir;
use tidepool_testing::eval_harness::EvalHarness;

/// Restores an environment variable on drop — same pattern as
/// `cache_tests.rs`'s own `EnvGuard`.
struct EnvGuard {
    key: &'static str,
    old_value: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let old_value = env::var(key).ok();
        env::set_var(key, value);
        Self { key, old_value }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.old_value {
            Some(v) => env::set_var(self.key, v),
            None => env::remove_var(self.key),
        }
    }
}

const SRC: &str = "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}\n\
module Test where\n\
import Tidepool.Prelude\n\
\n\
result :: Text\n\
result = \"hello \" <> toUpper \"world\"\n";

#[test]
#[serial]
fn build_products_dir_cold_warm_identical_output() {
    // This test's OWN private build-products dir — never the ambient shared
    // one (see the lane boundary: "cold measurements pin their own
    // TIDEPOOL_BUILD_PRODUCTS_DIR + TIDEPOOL_COMPILE_CACHE_DIR, never clear
    // ambient shared state").
    let bp_dir = TempDir::new().unwrap();
    let _bp_guard = EnvGuard::set("TIDEPOOL_BUILD_PRODUCTS_DIR", bp_dir.path());

    // COLD: a fresh eval-cache dir, and the build-products dir starts empty.
    let cold_cache = TempDir::new().unwrap();
    let cold = {
        let _cache_guard = EnvGuard::set("XDG_CACHE_HOME", cold_cache.path());
        EvalHarness::new()
            .with_stdlib()
            .compile(SRC, "result")
            .expect("cold compile failed")
    };

    // WARM: a DIFFERENT fresh eval-cache dir — so this is a REAL second
    // extract spawn, not an eval-cache hit short-circuiting the question —
    // but the SAME build-products dir, now populated by the cold compile
    // above, so Tidepool.Prelude's closure is skip-loadable this time.
    let warm_cache = TempDir::new().unwrap();
    let warm = {
        let _cache_guard = EnvGuard::set("XDG_CACHE_HOME", warm_cache.path());
        EvalHarness::new()
            .with_stdlib()
            .compile(SRC, "result")
            .expect("warm compile failed")
    };

    assert_eq!(
        cold.expr, warm.expr,
        "a warm build-products dir must not change the compiled Core"
    );
    assert_eq!(
        cold.table, warm.table,
        "a warm build-products dir must not change the DataConTable"
    );
    // MetaWarnings has no PartialEq — has_io is the field that would move if
    // extraction behaved differently, so compare it directly.
    assert_eq!(
        cold.warnings.has_io, warm.warnings.has_io,
        "a warm build-products dir must not change the has_io warning"
    );
}
