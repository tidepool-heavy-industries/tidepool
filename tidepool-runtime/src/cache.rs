//! Filesystem caching for compiled artifacts.

use std::fs;
use std::path::{Path, PathBuf};

/// Returns the platform-specific cache directory for Tidepool.
/// Following XDG conventions: `$XDG_CACHE_HOME/tidepool` or `~/.cache/tidepool`.
fn cache_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .map(|d| d.join("tidepool"))
}

/// Computes a unique cache key for a compilation request.
/// The key includes the source code, the target binder, and a fingerprint of
/// all include directories to ensure cache invalidation when dependencies change.
pub(crate) fn cache_key(source: &str, target: &str, include: &[&Path]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(source.as_bytes());
    hasher.update(b"\0");
    hasher.update(target.as_bytes());
    hasher.update(b"\0");

    // Fingerprint include directories to catch changes in dependency modules.
    let mut sorted_includes: Vec<&Path> = include.to_vec();
    sorted_includes.sort();
    for root in sorted_includes {
        hasher.update(root.as_os_str().as_encoded_bytes());
        hasher.update(b"\0");
        fingerprint_dir(root, &mut hasher);
    }

    extract_binary_fingerprint(&mut hasher);

    hasher.finalize().to_hex().to_string()
}

/// Fingerprints the compiler binary to ensure cache invalidation on upgrades.
/// If the resolved path is a shell wrapper script (e.g. ~/.cargo/bin/tidepool-extract),
/// also fingerprints the target binary it delegates to (e.g. ~/.local/bin/tidepool-extract-bin).
fn extract_binary_fingerprint(hasher: &mut blake3::Hasher) {
    let bin_name = std::env::var("TIDEPOOL_EXTRACT")
        .unwrap_or_else(|_| "tidepool-extract".to_string());

    if let Ok(path) = which::which(&bin_name) {
        fingerprint_single_binary(hasher, &path);

        // If this looks like a shell wrapper script, also fingerprint the target binary.
        if let Ok(contents) = fs::read_to_string(&path) {
            if contents.len() < 4096 && (contents.starts_with("#!") || contents.contains("exec "))
            {
                for line in contents.lines() {
                    if let Some(target) = extract_exec_target(line.trim()) {
                        let target_path = PathBuf::from(target);
                        if target_path.exists() {
                            if let Ok(resolved) = fs::canonicalize(&target_path) {
                                fingerprint_single_binary(hasher, &resolved);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Fingerprints a single binary by path, size, and mtime.
fn fingerprint_single_binary(hasher: &mut blake3::Hasher, path: &Path) {
    hasher.update(path.as_os_str().as_encoded_bytes());
    hasher.update(b"\0");
    if let Ok(meta) = fs::metadata(path) {
        hasher.update(&meta.len().to_le_bytes());
        if let Ok(mtime) = meta.modified() {
            if let Ok(dur) = mtime.duration_since(std::time::UNIX_EPOCH) {
                hasher.update(&dur.as_nanos().to_le_bytes());
            }
        }
    }
}

/// Extracts an absolute path from a shell exec line.
/// Handles patterns like `exec /path/to/bin "$@"` or bare `/path/to/bin "$@"`.
fn extract_exec_target(line: &str) -> Option<&str> {
    let line = line.strip_prefix("exec ").unwrap_or(line);
    if line.is_empty() || line.starts_with('#') || line.contains('=') {
        return None;
    }
    let token = line.split_whitespace().next()?;
    if token.starts_with('/') {
        Some(token)
    } else {
        None
    }
}

/// Recursively walks a directory to fingerprint its contents.
/// Considers file paths, sizes, and modification times of `.hs` and `.hs-boot` files.
fn fingerprint_dir(dir: &Path, hasher: &mut blake3::Hasher) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    paths.sort_by_key(|e| e.path());

    for entry in paths {
        let path = entry.path();
        if path.is_dir() {
            fingerprint_dir(&path, hasher);
            continue;
        }
        let Some(ext) = path.extension() else {
            continue;
        };
        if ext != "hs" && ext != "hs-boot" {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        hasher.update(path.as_os_str().as_encoded_bytes());
        hasher.update(&meta.len().to_le_bytes());
        if let Ok(mtime) = meta.modified() {
            if let Ok(dur) = mtime.duration_since(std::time::UNIX_EPOCH) {
                hasher.update(&dur.as_nanos().to_le_bytes());
            }
        }
    }
}

/// Attempts to load the Core expression and metadata from the cache.
/// Returns `Some((expr_bytes, meta_bytes))` on success.
/// Only returns data if the sentinel file exists, indicating a complete store.
pub(crate) fn cache_load(key: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    let dir = cache_dir()?;
    let sentinel = dir.join(format!("{}.ok", key));
    if !sentinel.exists() {
        return None;
    }

    let expr_path = dir.join(format!("{}.cbor", key));
    let meta_path = dir.join(format!("{}.meta.cbor", key));

    let expr = fs::read(&expr_path).ok()?;
    let meta = fs::read(&meta_path).ok()?;

    Some((expr, meta))
}

/// Stores the compilation results in the cache. Each file is replaced atomically
/// via rename. A sentinel file `{key}.ok` is written last to mark the entry as
/// complete — `cache_load` checks for this before reading.
pub(crate) fn cache_store(key: &str, expr_bytes: &[u8], meta_bytes: &[u8]) {
    let Some(dir) = cache_dir() else { return };
    if fs::create_dir_all(&dir).is_err() {
        return;
    }

    use std::io::Write;

    let Ok(mut tmp_expr) = tempfile::NamedTempFile::new_in(&dir) else {
        return;
    };
    let Ok(mut tmp_meta) = tempfile::NamedTempFile::new_in(&dir) else {
        return;
    };

    if tmp_expr.write_all(expr_bytes).is_err() {
        return;
    }
    if tmp_meta.write_all(meta_bytes).is_err() {
        return;
    }

    let final_expr = dir.join(format!("{}.cbor", key));
    let final_meta = dir.join(format!("{}.meta.cbor", key));
    let sentinel = dir.join(format!("{}.ok", key));

    // Remove sentinel first — marks the entry as incomplete during update.
    let _ = fs::remove_file(&sentinel);

    if tmp_expr.persist(&final_expr).is_err() {
        return;
    }
    if tmp_meta.persist(&final_meta).is_err() {
        return;
    }

    // Sentinel written last — entry is only valid when this exists.
    let _ = fs::write(&sentinel, b"");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    /// RAII guard to safely set and restore environment variables in tests.
    struct EnvGuard {
        key: &'static str,
        old_value: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn new(key: &'static str, new_value: impl AsRef<std::ffi::OsStr>) -> Self {
            let old_value = std::env::var_os(key);
            std::env::set_var(key, new_value);
            Self { key, old_value }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(ref old) = self.old_value {
                std::env::set_var(self.key, old);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    #[serial]
    fn test_cache_key_determinism() {
        let source = "main = print 42";
        let target = "main";
        let k1 = cache_key(source, target, &[]);
        let k2 = cache_key(source, target, &[]);
        assert_eq!(k1, k2);

        let k3 = cache_key("main = print 43", target, &[]);
        assert_ne!(k1, k3);
    }

    #[test]
    #[serial]
    fn test_cache_roundtrip() {
        let temp_dir = TempDir::new().unwrap();
        let _guard = EnvGuard::new("XDG_CACHE_HOME", temp_dir.path());

        let key = "test-key";
        let expr = b"expr-data";
        let meta = b"meta-data";

        // Before store, load should miss.
        assert!(cache_load(key).is_none());

        cache_store(key, expr, meta);

        // Sentinel must exist after store.
        let sentinel = temp_dir.path().join("tidepool").join(format!("{}.ok", key));
        assert!(sentinel.exists(), "sentinel file should exist after store");

        let loaded = cache_load(key).expect("cache should load after store");
        assert_eq!(loaded.0, expr);
        assert_eq!(loaded.1, meta);
    }

    #[test]
    #[serial]
    fn test_cache_load_fails_without_sentinel() {
        let temp_dir = TempDir::new().unwrap();
        let _guard = EnvGuard::new("XDG_CACHE_HOME", temp_dir.path());

        let key = "no-sentinel";
        let dir = temp_dir.path().join("tidepool");
        fs::create_dir_all(&dir).unwrap();

        // Write cbor files but no sentinel — simulates a crash mid-store.
        fs::write(dir.join(format!("{}.cbor", key)), b"expr").unwrap();
        fs::write(dir.join(format!("{}.meta.cbor", key)), b"meta").unwrap();

        assert!(
            cache_load(key).is_none(),
            "cache_load should return None without sentinel"
        );
    }

    #[test]
    #[serial]
    fn test_cache_key_include_fingerprint() {
        let include_dir = TempDir::new().unwrap();
        let hs_file = include_dir.path().join("Lib.hs");
        fs::write(&hs_file, "module Lib where").unwrap();

        let source = "import Lib\nmain = print 42";
        let target = "main";
        let includes = [include_dir.path()];

        let k1 = cache_key(source, target, &includes);

        // Wait a bit to ensure mtime changes if we overwrite (though some filesystems have low precision)
        // or just write different content/size.
        fs::write(&hs_file, "module Lib where\nfoo = 1").unwrap();
        let k2 = cache_key(source, target, &includes);

        assert_ne!(
            k1, k2,
            "Cache key should change when dependency file changes"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_cache_key_binary_fingerprint_mtime() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = TempDir::new().unwrap();
        let bin_path = temp_dir.path().join("fake-extract");
        fs::write(&bin_path, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&bin_path, fs::Permissions::from_mode(0o755)).unwrap();

        // Point directly to the binary to avoid PATH mutation
        let _guard = EnvGuard::new("TIDEPOOL_EXTRACT", &bin_path);

        let k1 = cache_key("source", "target", &[]);

        // Change mtime
        let past = filetime::FileTime::from_unix_time(100, 0);
        filetime::set_file_mtime(&bin_path, past).unwrap();

        let k2 = cache_key("source", "target", &[]);
        assert_ne!(k1, k2, "Cache key should change when binary mtime changes");
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_cache_key_wrapper_script_fingerprints_target() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = TempDir::new().unwrap();

        // Create the "real" binary.
        let real_bin = temp_dir.path().join("tidepool-extract-bin");
        fs::write(&real_bin, b"real-binary-v1").unwrap();
        fs::set_permissions(&real_bin, fs::Permissions::from_mode(0o755)).unwrap();

        // Create a wrapper script that execs the real binary.
        let wrapper = temp_dir.path().join("tidepool-extract");
        fs::write(
            &wrapper,
            format!("#!/bin/sh\nexec {} \"$@\"\n", real_bin.display()),
        )
        .unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();

        let _guard = EnvGuard::new("TIDEPOOL_EXTRACT", &wrapper);

        let k1 = cache_key("source", "target", &[]);

        // Change the real binary (wrapper unchanged) — key must change.
        fs::write(&real_bin, b"real-binary-v2-longer").unwrap();
        let k2 = cache_key("source", "target", &[]);

        assert_ne!(
            k1, k2,
            "Cache key should change when the target binary behind a wrapper changes"
        );
    }

    #[test]
    fn test_extract_exec_target() {
        assert_eq!(
            extract_exec_target("exec /usr/local/bin/foo \"$@\""),
            Some("/usr/local/bin/foo")
        );
        assert_eq!(
            extract_exec_target("/usr/local/bin/foo \"$@\""),
            Some("/usr/local/bin/foo")
        );
        assert_eq!(extract_exec_target("#!/bin/sh"), None);
        assert_eq!(extract_exec_target("FOO=bar"), None);
        assert_eq!(extract_exec_target(""), None);
        assert_eq!(extract_exec_target("relative-path arg"), None);
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_cache_key_binary_fingerprint_size() {
        use std::os::unix::fs::PermissionsExt;
        use std::io::Write;

        let temp_dir = TempDir::new().unwrap();
        let bin_path = temp_dir.path().join("fake-extract-size");
        fs::write(&bin_path, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&bin_path, fs::Permissions::from_mode(0o755)).unwrap();

        // Point directly to the binary to avoid PATH mutation
        let _guard = EnvGuard::new("TIDEPOOL_EXTRACT", &bin_path);

        let k1 = cache_key("source", "target", &[]);

        // Change size
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&bin_path)
            .unwrap();
        file.write_all(b"extra").unwrap();
        drop(file);

        let k2 = cache_key("source", "target", &[]);
        assert_ne!(k1, k2, "Cache key should change when binary size changes");
    }
}
