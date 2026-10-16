//! Build-owned Exomonad sources paired with the Tidepool stdlib.

use std::path::{Path, PathBuf};

use tidepool::haskell_sources::EMBEDDED_STDLIB;
use tidepool_toolchain::source_bundle::{content_hash, materialize};
include!(concat!(env!("OUT_DIR"), "/embedded_exomonad_haskell.rs"));
include!(concat!(env!("OUT_DIR"), "/dev_source_identity.rs"));

pub(crate) const DEV_SOURCE_DOMAIN: &[u8] = b"tidepool-facade-dev-source-identity";

/// Identity of the library interfaces embedded in this build. Workspace
/// selections must not silently resume against a different imported surface.
///
/// A dev build embeds no source bytes, but its build script binds this binary
/// to the complete source identity it saw at build time.
pub(crate) fn source_identity() -> Result<String, tidepool_toolchain::cache::SourceManifestError> {
    if EMBEDDED_STDLIB.is_empty() && EMBEDDED_EXOMONAD_HASKELL.is_empty() {
        return Ok(DEV_SOURCE_IDENTITY
            .expect("dev Haskell source identity is emitted by build.rs")
            .to_owned());
    }
    Ok(format!(
        "{}:{}",
        content_hash(EMBEDDED_STDLIB),
        content_hash(EMBEDDED_EXOMONAD_HASKELL)
    ))
}

/// Resolve a run's library roots, retaining a configured deployment's final
/// source paths. A dev build refuses source bytes changed after its build.
pub(crate) fn runtime_source_roots(
    deployment: Option<&tidepool_toolchain::toolchain::NativeCatalogSourceSelection>,
) -> Result<[PathBuf; 2], Box<dyn std::error::Error>> {
    use tidepool_toolchain::toolchain::NativeSourceRole;
    let roots = match deployment {
        Some(selection) => [
            selection.root(NativeSourceRole::Stdlib),
            selection.root(NativeSourceRole::Actors),
        ],
        None => [
            tidepool::haskell_sources::ensure_embedded_stdlib()?,
            ensure_builtin_exomonad_haskell()?,
        ],
    };
    let expected = runtime_capture_identity(&roots)?;
    verify_runtime_capture(&roots, &expected)?;
    Ok(roots)
}

fn configured_runtime_sources() -> Result<Option<[PathBuf; 2]>, Box<dyn std::error::Error>> {
    tidepool_toolchain::toolchain::configured_module_source_selection()?
        .as_ref()
        .map(|selection| runtime_source_roots(Some(selection)))
        .transpose()
}

/// The capture must compare its copied bytes with the binary's fixed dev
/// identity, not another reading of a mutable checkout after admission.
pub(crate) fn runtime_capture_identity(
    roots: &[PathBuf; 2],
) -> Result<String, tidepool_toolchain::cache::SourceManifestError> {
    match DEV_SOURCE_IDENTITY {
        Some(identity) => Ok(identity.to_owned()),
        None => tidepool_toolchain::cache::source_roots_identity(DEV_SOURCE_DOMAIN, roots),
    }
}

pub(crate) fn verify_runtime_capture(
    roots: &[PathBuf; 2],
    expected_identity: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if DEV_SOURCE_IDENTITY.is_some() {
        return verify_dev_source_roots(roots, expected_identity);
    }
    for (root, entries) in roots
        .iter()
        .zip([EMBEDDED_STDLIB, EMBEDDED_EXOMONAD_HASKELL])
    {
        tidepool_toolchain::source_bundle::verify_embedded(root, entries)?;
    }
    Ok(())
}

fn verify_dev_source_roots(
    roots: &[PathBuf; 2],
    expected: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let selected = tidepool_toolchain::cache::source_roots_identity(DEV_SOURCE_DOMAIN, roots)?;
    if selected != expected {
        return Err(
            "dev Haskell library differs from this build; rebuild Exomonad before starting a run"
                .into(),
        );
    }
    Ok(())
}

/// Locate `bridge/haskell/actors` on disk, mirroring
/// `tidepool_toolchain::toolchain::locate_stdlib`'s in-repo walk-up: try every
/// ancestor of the current directory, then fall back to the `actors` sibling
/// of wherever the stdlib itself resolved (covering a launch cwd outside the
/// checkout that still has a stdlib override in play).
fn locate_exomonad_haskell() -> Option<PathBuf> {
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(found) = find_actors_from(&cwd) {
            return Some(found);
        }
    }
    let stdlib = tidepool::haskell_sources::ensure_embedded_stdlib().ok()?;
    let candidate = stdlib.parent()?.join("actors");
    is_actors_root(&candidate).then_some(candidate)
}

fn find_actors_from(start: &Path) -> Option<PathBuf> {
    // `bridge/haskell/actors` from the checkout root or above; `haskell/actors`
    // from inside `bridge/`.
    tidepool_toolchain::toolchain::walk_up_for(
        start,
        &[
            Path::new("bridge/haskell/actors"),
            Path::new("haskell/actors"),
        ],
        is_actors_root,
    )
}

/// `Tidepool/Check.hs` is an ordinary production module directly under the
/// actors root, present in every checkout and not excluded by
/// `build.rs`'s `collect` filters — a stable sentinel for "this directory is
/// `bridge/haskell/actors`".
fn is_actors_root(dir: &Path) -> bool {
    dir.join("Tidepool").join("Check.hs").is_file()
}

/// Resolve the Haskell modules used by Exomonad's interactive workbench: the
/// embedded bundle when this build carries one, otherwise the checkout's
/// `bridge/haskell/actors` located on disk.
pub fn ensure_exomonad_haskell() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some([_, actors]) = configured_runtime_sources()? {
        return Ok(actors);
    }
    ensure_builtin_exomonad_haskell()
}

fn ensure_builtin_exomonad_haskell() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if EMBEDDED_EXOMONAD_HASKELL.is_empty() {
        return locate_exomonad_haskell().ok_or_else(|| {
            "this build has no embedded Exomonad Haskell and no bridge/haskell/actors was found \
             above the current directory; build with TIDEPOOL_EMBED_HASKELL=1 or run inside the checkout"
                .into()
        });
    }
    let hash = content_hash(EMBEDDED_EXOMONAD_HASKELL);
    Ok(materialize(
        EMBEDDED_EXOMONAD_HASKELL,
        tidepool_toolchain::paths::cache_dir()
            .join("exomonad-haskell")
            .join(hash),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_actors_from_locates_bridge_haskell_actors_from_any_descendant() {
        // Mirrors `locate_stdlib`'s walk: the sentinel is found from any
        // descendant of the `bridge` ancestor directory — e.g. a crate nested
        // under `exomonad/host`, not just `bridge` itself.
        let root = tempfile::tempdir().unwrap();
        let actors = root.path().join("bridge/haskell/actors");
        std::fs::create_dir_all(actors.join("Tidepool")).unwrap();
        std::fs::write(
            actors.join("Tidepool/Check.hs"),
            "module Tidepool.Check where\n",
        )
        .unwrap();

        let deep = root.path().join("exomonad/host/some/deeply/nested/cwd");
        std::fs::create_dir_all(&deep).unwrap();

        assert_eq!(
            find_actors_from(&deep).unwrap().canonicalize().unwrap(),
            actors.canonicalize().unwrap()
        );
    }

    #[test]
    fn find_actors_from_returns_none_without_a_sentinel() {
        let root = tempfile::tempdir().unwrap();
        // A directory tree with no `bridge/haskell/actors` at all.
        let deep = root.path().join("exomonad/host/some/other/tree");
        std::fs::create_dir_all(&deep).unwrap();
        assert!(find_actors_from(&deep).is_none());
    }

    #[test]
    fn find_actors_from_returns_none_when_the_sentinel_module_is_missing() {
        let root = tempfile::tempdir().unwrap();
        // The directory exists but lacks the sentinel module, e.g. a
        // half-populated checkout.
        std::fs::create_dir_all(root.path().join("bridge/haskell/actors/Tidepool")).unwrap();
        let deep = root.path().join("exomonad/host/some/cwd");
        std::fs::create_dir_all(&deep).unwrap();
        assert!(find_actors_from(&deep).is_none());
    }

    #[test]
    fn dev_source_selection_rejects_an_edit_or_missing_tree() {
        for changed in 0..2 {
            let root = tempfile::tempdir().unwrap();
            let stdlib = root.path().join("lib");
            let actors = root.path().join("actors");
            std::fs::create_dir_all(&stdlib).unwrap();
            std::fs::create_dir_all(&actors).unwrap();
            std::fs::write(stdlib.join("Prelude.hs"), "module Tidepool.Prelude where\n").unwrap();
            std::fs::write(actors.join("Check.hs"), "module Tidepool.Check where\n").unwrap();
            let roots = [stdlib, actors];
            let before =
                tidepool_toolchain::cache::source_roots_identity(DEV_SOURCE_DOMAIN, &roots)
                    .unwrap();
            verify_dev_source_roots(&roots, &before).unwrap();
            let module = if changed == 0 {
                "Prelude.hs"
            } else {
                "Check.hs"
            };
            std::fs::write(roots[changed].join(module), "module Changed where\n").unwrap();
            assert!(verify_dev_source_roots(&roots, &before).is_err());
            std::fs::remove_dir_all(&roots[changed]).unwrap();
            assert!(verify_dev_source_roots(&roots, &before).is_err());
        }
    }
}
