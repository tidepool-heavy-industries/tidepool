use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tidepool_runtime::compile_haskell;

fn prelude_path() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().join("haskell").join("lib")
}

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
#[ignore = "mutates env vars, run with --test-threads=1"]
fn test_cache_hit_same_source() {
    let cache_root = TempDir::new().unwrap();
    let _guard = EnvGuard::set("XDG_CACHE_HOME", cache_root.path().to_path_buf());
    let tidepool_cache = cache_root.path().join("tidepool");
    let pp = prelude_path();

    let src = "module Test where\nval = 42";
    let target = "val";

    let (expr1, _) = compile_haskell(src, target, &[pp.as_path()]).expect("First compile failed");
    assert!(tidepool_cache.exists(), "Cache directory should be created");
    let count1 = fs::read_dir(&tidepool_cache).unwrap().count();
    assert!(count1 >= 2, "At least .cbor and .meta.cbor should be cached");

    let (expr2, _) = compile_haskell(src, target, &[pp.as_path()]).expect("Second compile failed");
    assert_eq!(expr1, expr2);

    let count2 = fs::read_dir(&tidepool_cache).unwrap().count();
    assert_eq!(count1, count2, "Cache hit should not create new files");
}

#[test]
#[ignore = "mutates env vars, run with --test-threads=1"]
fn test_cache_miss_different_source() {
    let cache_root = TempDir::new().unwrap();
    let _guard = EnvGuard::set("XDG_CACHE_HOME", cache_root.path().to_path_buf());
    let tidepool_cache = cache_root.path().join("tidepool");
    let pp = prelude_path();

    let src1 = "module Test where\nval = 1";
    let src2 = "module Test where\nval = 2";
    let target = "val";

    compile_haskell(src1, target, &[pp.as_path()]).expect("First compile failed");
    let count1 = fs::read_dir(&tidepool_cache).unwrap().count();

    compile_haskell(src2, target, &[pp.as_path()]).expect("Second compile failed");
    let count2 = fs::read_dir(&tidepool_cache).unwrap().count();

    assert!(count2 > count1, "Different source should result in a cache miss and new files");
}

#[test]
#[ignore = "mutates env vars, run with --test-threads=1"]
fn test_cache_miss_modified_include() {
    let cache_root = TempDir::new().unwrap();
    let _guard = EnvGuard::set("XDG_CACHE_HOME", cache_root.path().to_path_buf());
    let tidepool_cache = cache_root.path().join("tidepool");

    let include_dir = TempDir::new().unwrap();
    let hs_file = include_dir.path().join("Lib.hs");
    fs::write(&hs_file, "module Lib where\nfoo = 1").unwrap();

    let src = "module Test where\nimport Lib\nmain = foo";
    let target = "main";
    let includes = [include_dir.path()];

    compile_haskell(src, target, &includes).expect("First compile failed");
    let count1 = fs::read_dir(&tidepool_cache).unwrap().count();

    fs::write(&hs_file, "module Lib where\nfoo = 2").unwrap();

    compile_haskell(src, target, &includes).expect("Second compile failed");
    let count2 = fs::read_dir(&tidepool_cache).unwrap().count();

    assert!(count2 > count1, "Modified include should result in a cache miss");
}

#[test]
#[ignore = "mutates env vars, run with --test-threads=1"]
fn test_corrupted_cache_recovery() {
    let cache_root = TempDir::new().unwrap();
    let _guard = EnvGuard::set("XDG_CACHE_HOME", cache_root.path().to_path_buf());
    let tidepool_cache = cache_root.path().join("tidepool");
    let pp = prelude_path();

    let src = "module Test where\nval = 100";
    let target = "val";

    compile_haskell(src, target, &[pp.as_path()]).expect("Initial compile failed");
    assert!(tidepool_cache.exists());

    for entry in fs::read_dir(&tidepool_cache).unwrap() {
        let path = entry.unwrap().path();
        fs::write(path, b"NOT CBOR DATA").unwrap();
    }

    let result = compile_haskell(src, target, &[pp.as_path()]);
    assert!(result.is_ok(), "Should recover and recompile when cache is corrupted");
}
