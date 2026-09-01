//! Haskell source bundles embedded in installed Tidepool binaries.
//!
//! The build script walks each complete source tree. This module owns the one
//! content-addressed materializer used by both the public Tidepool library and
//! Shoal's bootstrap policy.

use std::path::PathBuf;

include!(concat!(env!("OUT_DIR"), "/embedded_stdlib.rs"));
include!(concat!(env!("OUT_DIR"), "/embedded_actor_policy.rs"));

fn content_hash(entries: &[(&str, &str)]) -> String {
    let mut hasher = blake3::Hasher::new();
    for (relative, content) in entries {
        // Length-prefix both fields so different path/content partitions
        // cannot produce the same byte stream.
        hasher.update(&(relative.len() as u64).to_le_bytes());
        hasher.update(relative.as_bytes());
        hasher.update(&(content.len() as u64).to_le_bytes());
        hasher.update(content.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

/// Materialize one complete embedded source tree. A content-addressed path and
/// completion sentinel make concurrent or repeated startup idempotent; a
/// partial tree is never selected as complete.
fn materialize(
    entries: &[(&str, &str)],
    destination: PathBuf,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let hash = content_hash(entries);
    let sentinel = destination.join(".complete");
    let complete = std::fs::read_to_string(&sentinel).is_ok_and(|recorded| recorded == hash);
    if !complete {
        for (relative, content) in entries {
            let path = destination.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            tidepool_atomic_write::write_best_effort(&path, content.as_bytes())?;
        }
        tidepool_atomic_write::write_best_effort(&sentinel, hash.as_bytes())?;
    }
    Ok(destination)
}

/// Resolve the complete embedded Tidepool Haskell library.
pub fn ensure_stdlib() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let hash = content_hash(EMBEDDED_STDLIB);
    let fallbacks = tidepool_runtime::toolchain::StdlibFallbacks {
        bundle: Some(materialize(
            EMBEDDED_STDLIB,
            tidepool_runtime::paths::stdlib_dir(&hash),
        )?),
        build_tree: None,
    };
    Ok(tidepool_runtime::toolchain::locate_stdlib(&fallbacks)?.dir)
}

/// Resolve the bundled actor policy used by Shoal bootstrap.
pub fn ensure_actor_policy() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let hash = content_hash(EMBEDDED_ACTOR_POLICY);
    materialize(
        EMBEDDED_ACTOR_POLICY,
        tidepool_runtime::paths::cache_dir()
            .join("actor-policy")
            .join(hash),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCES: &[(&str, &str)] = &[("Tidepool/One.hs", "module Tidepool.One where\n")];

    #[test]
    fn materialization_repairs_a_stale_or_partial_completion_marker() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("bundle");
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(destination.join(".complete"), "not-the-content-digest").unwrap();

        materialize(SOURCES, destination.clone()).unwrap();
        assert_eq!(
            std::fs::read_to_string(destination.join("Tidepool/One.hs")).unwrap(),
            SOURCES[0].1
        );
        assert_eq!(
            std::fs::read_to_string(destination.join(".complete")).unwrap(),
            content_hash(SOURCES)
        );
    }
}
