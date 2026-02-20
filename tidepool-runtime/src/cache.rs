use std::path::{Path, PathBuf};
use std::fs;

fn cache_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .map(|d| d.join("tidepool"))
}

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

    hasher.finalize().to_hex().to_string()
}

fn fingerprint_dir(dir: &Path, hasher: &mut blake3::Hasher) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut paths: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    paths.sort_by_key(|e| e.path());

    for entry in paths {
        let path = entry.path();
        if path.is_dir() {
            fingerprint_dir(&path, hasher);
        } else if let Some(ext) = path.extension() {
            if ext == "hs" || ext == "hs-boot" {
                if let Ok(meta) = entry.metadata() {
                    hasher.update(path.as_os_str().as_encoded_bytes());
                    hasher.update(&meta.len().to_le_bytes());
                    if let Ok(mtime) = meta.modified() {
                        if let Ok(dur) = mtime.duration_since(std::time::UNIX_EPOCH) {
                            hasher.update(&dur.as_nanos().to_le_bytes());
                        }
                    }
                }
            }
        }
    }
}

pub(crate) fn cache_load(key: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    let dir = cache_dir()?;
    let expr_bytes = fs::read(dir.join(format!("{}.cbor", key))).ok()?;
    let meta_bytes = fs::read(dir.join(format!("{}.meta.cbor", key))).ok()?;
    Some((expr_bytes, meta_bytes))
}

pub(crate) fn cache_store(key: &str, expr_bytes: &[u8], meta_bytes: &[u8]) {
    let Some(dir) = cache_dir() else { return };
    if fs::create_dir_all(&dir).is_err() { return; }

    let expr_path = dir.join(format!("{}.cbor", key));
    let meta_path = dir.join(format!("{}.meta.cbor", key));

    let _ = atomic_write(&expr_path, expr_bytes);
    let _ = atomic_write(&meta_path, meta_bytes);
}

fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "no parent dir"))?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    use std::io::Write;
    temp.write_all(data)?;
    temp.persist(path).map(|_| ()).map_err(|e| e.error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
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
    fn test_cache_roundtrip() {
        let temp_dir = TempDir::new().unwrap();
        std::env::set_var("XDG_CACHE_HOME", temp_dir.path());

        let key = "test-key";
        let expr = b"expr-data";
        let meta = b"meta-data";

        cache_store(key, expr, meta);
        let loaded = cache_load(key).expect("cache should load after store");
        assert_eq!(loaded.0, expr);
        assert_eq!(loaded.1, meta);
    }

    #[test]
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
        
        assert_ne!(k1, k2, "Cache key should change when dependency file changes");
    }
}