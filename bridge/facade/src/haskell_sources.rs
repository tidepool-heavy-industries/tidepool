//! Haskell source bundles embedded in installed Tidepool binaries.
//!
//! The build script walks each complete source tree. This module owns the one
//! content-addressed materializer used by both the public Tidepool library and
//! Shoal's public surface and private interactive driver.

use std::path::PathBuf;

include!(concat!(env!("OUT_DIR"), "/embedded_stdlib.rs"));
include!(concat!(env!("OUT_DIR"), "/embedded_shoal_haskell.rs"));

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

/// Identity of the library interfaces embedded in this build. Workspace
/// selections must not silently resume against a different imported surface.
pub(crate) fn source_identity() -> String {
    format!(
        "{}:{}",
        content_hash(EMBEDDED_STDLIB),
        content_hash(EMBEDDED_SHOAL_HASKELL)
    )
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
    let complete = std::fs::read_to_string(&sentinel).is_ok_and(|recorded| recorded == hash)
        && entries.iter().all(|(relative, content)| {
            std::fs::read(destination.join(relative)).is_ok_and(|bytes| bytes == content.as_bytes())
        });
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

/// Resolve the general Tidepool library, honoring development overrides before
/// falling back to the complete embedded source bundle.
pub fn ensure_stdlib() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let fallbacks = tidepool_runtime::toolchain::StdlibFallbacks {
        bundle: Some(ensure_embedded_stdlib()?),
        build_tree: None,
    };
    Ok(tidepool_runtime::toolchain::locate_stdlib(&fallbacks)?.dir)
}

/// Shoal's frozen library must match `source_identity`, independent of launch
/// cwd or development overrides used by the general Tidepool tools.
pub(crate) fn ensure_embedded_stdlib() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let hash = content_hash(EMBEDDED_STDLIB);
    materialize(EMBEDDED_STDLIB, tidepool_runtime::paths::stdlib_dir(&hash))
}

/// Resolve the Haskell modules used by Shoal's interactive workbench.
pub fn ensure_shoal_haskell() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let hash = content_hash(EMBEDDED_SHOAL_HASKELL);
    materialize(
        EMBEDDED_SHOAL_HASKELL,
        tidepool_runtime::paths::cache_dir()
            .join("shoal-haskell")
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

    #[test]
    fn completion_marker_does_not_hide_changed_or_missing_library_sources() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("bundle");
        materialize(SOURCES, destination.clone()).unwrap();
        let module = destination.join("Tidepool/One.hs");
        std::fs::write(&module, "changed despite valid marker").unwrap();
        materialize(SOURCES, destination.clone()).unwrap();
        assert_eq!(std::fs::read_to_string(&module).unwrap(), SOURCES[0].1);
        std::fs::remove_file(&module).unwrap();
        materialize(SOURCES, destination).unwrap();
        assert_eq!(std::fs::read_to_string(module).unwrap(), SOURCES[0].1);
    }

    #[test]
    fn embedded_stdlib_contains_production_internal_modules_only() {
        assert!(EMBEDDED_STDLIB
            .iter()
            .any(|(path, _)| *path == "Tidepool/Internal/ExitCell.hs"));
        assert!(!EMBEDDED_STDLIB
            .iter()
            .any(|(path, _)| *path == "Tidepool/Internal/DataTextProbe.hs"));
    }
}
