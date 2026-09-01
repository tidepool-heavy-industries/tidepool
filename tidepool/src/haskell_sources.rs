//! Haskell source bundles embedded in installed Tidepool binaries.
//!
//! The build script walks each complete source tree. This module owns the one
//! content-addressed materializer used by both the public Tidepool library and
//! Shoal's bootstrap policy.

use std::path::PathBuf;

include!(concat!(env!("OUT_DIR"), "/embedded_stdlib.rs"));
include!(concat!(env!("OUT_DIR"), "/embedded_actor_policy.rs"));

fn content_hash(entries: &[(&str, &str)]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (relative, content) in entries {
        relative.hash(&mut hasher);
        content.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
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
    if !sentinel.exists() {
        for (relative, content) in entries {
            let path = destination.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, content)?;
        }
        std::fs::write(&sentinel, hash.as_bytes())?;
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
