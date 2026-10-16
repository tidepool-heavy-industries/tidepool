//! Tidepool's build-owned Haskell standard library.

use std::path::{Path, PathBuf};
use tidepool_toolchain::source_bundle::{content_hash, materialize};

include!(concat!(env!("OUT_DIR"), "/embedded_stdlib.rs"));
include!(concat!(env!("OUT_DIR"), "/dev_source_identity.rs"));

const DEV_SOURCE_DOMAIN: &[u8] = b"tidepool-stdlib-dev-source-identity";

fn dev_fallbacks() -> tidepool_toolchain::toolchain::StdlibFallbacks {
    tidepool_toolchain::toolchain::StdlibFallbacks {
        bundle: None,
        build_tree: Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("../haskell/lib")),
    }
}

/// Resolve the configured deployment's immutable stdlib, or the development
/// overrides and embedded fallback when no catalog is configured.
pub fn ensure_stdlib() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(selection) = tidepool_toolchain::toolchain::configured_module_source_selection()? {
        let root = selection.root(tidepool_toolchain::toolchain::NativeSourceRole::Stdlib);
        verify_runtime_capture(&root)?;
        return Ok(root);
    }
    let fallbacks = tidepool_toolchain::toolchain::StdlibFallbacks {
        bundle: Some(ensure_builtin_stdlib()?),
        ..dev_fallbacks()
    };
    Ok(tidepool_toolchain::toolchain::locate_stdlib(&fallbacks)?.dir)
}

/// Select this binary's immutable library independently of launch cwd and
/// overrides, or validate the build-bound source tree in development mode.
pub fn ensure_embedded_stdlib() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(selection) = tidepool_toolchain::toolchain::configured_module_source_selection()? {
        let root = selection.root(tidepool_toolchain::toolchain::NativeSourceRole::Stdlib);
        verify_runtime_capture(&root)?;
        return Ok(root);
    }
    let root = ensure_builtin_stdlib()?;
    verify_runtime_capture(&root)?;
    Ok(root)
}

fn ensure_builtin_stdlib() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if EMBEDDED_STDLIB.is_empty() {
        return Ok(tidepool_toolchain::toolchain::locate_stdlib(&dev_fallbacks())?.dir);
    }
    Ok(materialize(
        EMBEDDED_STDLIB,
        tidepool_toolchain::paths::stdlib_dir(&content_hash(EMBEDDED_STDLIB)),
    )?)
}

fn verify_runtime_capture(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    match DEV_SOURCE_IDENTITY {
        Some(expected) => {
            let actual = tidepool_toolchain::cache::source_roots_identity(
                DEV_SOURCE_DOMAIN,
                &[root.to_path_buf()],
            )?;
            if actual != expected {
                return Err(
                    "dev Haskell standard library differs from this build; rebuild Tidepool".into(),
                );
            }
            Ok(())
        }
        None => tidepool_toolchain::source_bundle::verify_embedded(root, EMBEDDED_STDLIB),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_stdlib_retains_production_internal_modules() {
        assert!(
            !EMBEDDED_STDLIB.is_empty(),
            "embedded library proof requires TIDEPOOL_EMBED_HASKELL=1"
        );
        assert!(EMBEDDED_STDLIB
            .iter()
            .any(|(path, _)| *path == "Tidepool/Internal/ExitCell.hs"));
    }
}
