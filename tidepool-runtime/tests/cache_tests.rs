use serial_test::serial;
use std::env;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;
use tidepool_runtime::CompileResult;
use tidepool_testing::eval_harness::EvalHarness;

/// Helper to restore an environment variable after a test.
struct EnvGuard {
    key: &'static str,
    old_value: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: PathBuf) -> Self {
        let old_value = env::var(key).ok();
        env::set_var(key, value);
        Self { key, old_value }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(ref val) = self.old_value {
            env::set_var(self.key, val);
        } else {
            env::remove_var(self.key);
        }
    }
}

#[test]
#[serial]
fn test_cache_hit_same_source() {
    let cache_root = TempDir::new().unwrap();
    let _guard = EnvGuard::set("XDG_CACHE_HOME", cache_root.path().to_path_buf());
    let tidepool_cache = cache_root.path().join("tidepool");
    let harness = EvalHarness::new().with_stdlib();

    let src = "module Test where\nval = 42";
    let target = "val";

    let CompileResult { expr: expr1, .. } =
        harness.compile(src, target).expect("First compile failed");
    assert!(tidepool_cache.exists(), "Cache directory should be created");
    let count1 = fs::read_dir(&tidepool_cache).unwrap().count();
    assert!(
        count1 >= 2,
        "At least .cbor and .meta.cbor should be cached"
    );

    let CompileResult { expr: expr2, .. } =
        harness.compile(src, target).expect("Second compile failed");
    assert_eq!(expr1, expr2);

    let count2 = fs::read_dir(&tidepool_cache).unwrap().count();
    assert_eq!(count1, count2, "Cache hit should not create new files");
}

#[test]
#[serial]
fn test_cache_miss_different_source() {
    let cache_root = TempDir::new().unwrap();
    let _guard = EnvGuard::set("XDG_CACHE_HOME", cache_root.path().to_path_buf());
    let tidepool_cache = cache_root.path().join("tidepool");
    let harness = EvalHarness::new().with_stdlib();

    let src1 = "module Test where\nval = 1";
    let src2 = "module Test where\nval = 2";
    let target = "val";

    harness.compile(src1, target).expect("First compile failed");
    let count1 = fs::read_dir(&tidepool_cache).unwrap().count();

    harness
        .compile(src2, target)
        .expect("Second compile failed");
    let count2 = fs::read_dir(&tidepool_cache).unwrap().count();

    assert!(
        count2 > count1,
        "Different source should result in a cache miss and new files"
    );
}

#[test]
#[serial]
fn test_cache_miss_modified_include() {
    let cache_root = TempDir::new().unwrap();
    let _guard = EnvGuard::set("XDG_CACHE_HOME", cache_root.path().to_path_buf());
    let tidepool_cache = cache_root.path().join("tidepool");

    let include_dir = TempDir::new().unwrap();
    let hs_file = include_dir.path().join("Lib.hs");
    fs::write(&hs_file, "module Lib where\nfoo = 1").unwrap();

    let src = "module Test where\nimport Lib\nmain = foo";
    let target = "main";
    let harness = EvalHarness::new().with_include(include_dir.path());

    harness.compile(src, target).expect("First compile failed");
    let count1 = fs::read_dir(&tidepool_cache).unwrap().count();

    fs::write(&hs_file, "module Lib where\nfoo = 2").unwrap();

    harness.compile(src, target).expect("Second compile failed");
    let count2 = fs::read_dir(&tidepool_cache).unwrap().count();

    assert!(
        count2 > count1,
        "Modified include should result in a cache miss"
    );
}

#[test]
#[serial]
fn test_corrupted_cache_recovery() {
    let cache_root = TempDir::new().unwrap();
    let _guard = EnvGuard::set("XDG_CACHE_HOME", cache_root.path().to_path_buf());
    let tidepool_cache = cache_root.path().join("tidepool");
    let harness = EvalHarness::new().with_stdlib();

    let src = "module Test where\nval = 100";
    let target = "val";

    harness
        .compile(src, target)
        .expect("Initial compile failed");
    assert!(tidepool_cache.exists());

    for entry in fs::read_dir(&tidepool_cache).unwrap() {
        let path = entry.unwrap().path();
        fs::write(path, b"NOT CBOR DATA").unwrap();
    }

    let result = harness.compile(src, target);
    assert!(
        result.is_ok(),
        "Should recover and recompile when cache is corrupted"
    );
}
