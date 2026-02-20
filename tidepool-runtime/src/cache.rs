use std::path::{Path, PathBuf};

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
    let mut sorted: Vec<&Path> = include.to_vec();
    sorted.sort();
    for p in &sorted {
        hasher.update(p.as_os_str().as_encoded_bytes());
        hasher.update(b"\0");
    }
    hasher.finalize().to_hex().to_string()
}

pub(crate) fn cache_load(key: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    let dir = cache_dir()?;
    let expr_bytes = std::fs::read(dir.join(format!("{}.cbor", key))).ok()?;
    let meta_bytes = std::fs::read(dir.join(format!("{}.meta.cbor", key))).ok()?;
    Some((expr_bytes, meta_bytes))
}

pub(crate) fn cache_store(key: &str, expr_bytes: &[u8], meta_bytes: &[u8]) {
    let Some(dir) = cache_dir() else { return };
    if std::fs::create_dir_all(&dir).is_err() { return; }
    let _ = std::fs::write(dir.join(format!("{}.cbor", key)), expr_bytes);
    let _ = std::fs::write(dir.join(format!("{}.meta.cbor", key)), meta_bytes);
}
