use std::fs;
use std::env;
use std::path::PathBuf;
use tempfile::TempDir;
use tidepool_runtime::compile_haskell;

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
#[ignore] // Requires tidepool-extract on PATH
fn test_cache_hit_same_source() {
    let cache_root = TempDir::new().unwrap();
    let _guard = EnvGuard::set("XDG_CACHE_HOME", cache_root.path().to_path_buf());
    let tidepool_cache = cache_root.path().join("tidepool");

    let src = "module Test where\nval = 42";
    let target = "val";

    // First compile
    let (expr1, _) = compile_haskell(src, target, &[]).expect("First compile failed");
    assert!(tidepool_cache.exists(), "Cache directory should be created");
    let count1 = fs::read_dir(&tidepool_cache).unwrap().count();
    assert!(count1 >= 2, "At least .cbor and .meta.cbor should be cached");

    // Second compile
    let (expr2, _) = compile_haskell(src, target, &[]).expect("Second compile failed");
    
    // Results should be identical
    assert_eq!(expr1, expr2);
    
    // No new files should be created in cache
    let count2 = fs::read_dir(&tidepool_cache).unwrap().count();
    assert_eq!(count1, count2, "Cache hit should not create new files");
}

#[test]
#[ignore] // Requires tidepool-extract on PATH
fn test_cache_miss_different_source() {
    let cache_root = TempDir::new().unwrap();
    let _guard = EnvGuard::set("XDG_CACHE_HOME", cache_root.path().to_path_buf());
    let tidepool_cache = cache_root.path().join("tidepool");

    let src1 = "module Test where\nval = 1";
    let src2 = "module Test where\nval = 2";
    let target = "val";

    // Compile first version
    compile_haskell(src1, target, &[]).expect("First compile failed");
    let count1 = fs::read_dir(&tidepool_cache).unwrap().count();

    // Compile second version
    compile_haskell(src2, target, &[]).expect("Second compile failed");
    let count2 = fs::read_dir(&tidepool_cache).unwrap().count();

    assert!(count2 > count1, "Different source should result in a cache miss and new files");
}

#[test]
#[ignore] // Requires tidepool-extract on PATH
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

    // First compile
    compile_haskell(src, target, &includes).expect("First compile failed");
    let count1 = fs::read_dir(&tidepool_cache).unwrap().count();

    // Modify include file content to trigger fingerprint change
    fs::write(&hs_file, "module Lib where\nfoo = 2").unwrap();
    
    // Second compile
    compile_haskell(src, target, &includes).expect("Second compile failed");
    let count2 = fs::read_dir(&tidepool_cache).unwrap().count();

    assert!(count2 > count1, "Modified include should result in a cache miss");
}

#[test]
#[ignore] // Requires tidepool-extract on PATH
fn test_corrupted_cache_recovery() {
    let cache_root = TempDir::new().unwrap();
    let _guard = EnvGuard::set("XDG_CACHE_HOME", cache_root.path().to_path_buf());
    let tidepool_cache = cache_root.path().join("tidepool");

    let src = "module Test where\nval = 100";
    let target = "val";

    // Initial compile to populate cache
    compile_haskell(src, target, &[]).expect("Initial compile failed");
    assert!(tidepool_cache.exists());

    // Corrupt the cached files by overwriting them with garbage
    for entry in fs::read_dir(&tidepool_cache).unwrap() {
        let path = entry.unwrap().path();
        fs::write(path, b"NOT CBOR DATA").unwrap();
    }

    // Recompile - should detect corruption, ignore cache, and recompile successfully
    let result = compile_haskell(src, target, &[]);
    assert!(result.is_ok(), "Should successfully recover and recompile when cache is corrupted");
}