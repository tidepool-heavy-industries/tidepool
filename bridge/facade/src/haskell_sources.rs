//! Haskell source bundles embedded in installed Tidepool binaries.
//!
//! The build script walks each complete source tree, but only when
//! `TIDEPOOL_EMBED_HASKELL=1` (release/install builds); otherwise it emits
//! empty bundles and this module resolves the checkout on disk instead. This
//! module owns the one content-addressed materializer used by both the
//! public Tidepool library and Exomonad's public surface and private
//! interactive driver.

use std::path::{Path, PathBuf};

include!(concat!(env!("OUT_DIR"), "/embedded_stdlib.rs"));
include!(concat!(env!("OUT_DIR"), "/embedded_exomonad_haskell.rs"));

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
///
/// A dev build (`TIDEPOOL_EMBED_HASKELL` unset) embeds nothing, so there is no
/// fixed content to hash; instead this hashes the on-disk `bridge/haskell/lib`
/// and `bridge/haskell/actors` trees the build actually reads from, so
/// resuming against a workspace still fails closed when either tree changed.
pub(crate) fn source_identity() -> String {
    if EMBEDDED_STDLIB.is_empty() && EMBEDDED_EXOMONAD_HASKELL.is_empty() {
        return dev_source_identity();
    }
    format!(
        "{}:{}",
        content_hash(EMBEDDED_STDLIB),
        content_hash(EMBEDDED_EXOMONAD_HASKELL)
    )
}

/// [`source_identity`]'s dev-mode path: locate the checkout's stdlib and
/// actors trees and hash them with the same source-revision identity the
/// runtime uses elsewhere, rather than inventing a second content digest.
fn dev_source_identity() -> String {
    let stdlib = tidepool_toolchain::toolchain::locate_stdlib(
        &tidepool_toolchain::toolchain::StdlibFallbacks::default(),
    )
    .map(|location| location.dir)
    .ok();
    let actors = locate_exomonad_haskell();
    dev_source_identity_from(stdlib, actors)
}

/// Testable core of [`dev_source_identity`]: hash whichever of the stdlib and
/// actors directories were actually located. A missing tree contributes
/// nothing rather than a placeholder path, since [`source_roots_identity`]
/// hashes root count and content, not the root paths themselves — so a build
/// that cannot find one tree still gets a distinct identity from one that
/// found both.
fn dev_source_identity_from(stdlib: Option<PathBuf>, actors: Option<PathBuf>) -> String {
    let roots: Vec<PathBuf> = [stdlib, actors].into_iter().flatten().collect();
    tidepool_toolchain::cache::source_roots_identity(b"tidepool-facade-dev-source-identity", &roots)
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
    let stdlib = tidepool_toolchain::toolchain::locate_stdlib(
        &tidepool_toolchain::toolchain::StdlibFallbacks::default(),
    )
    .ok()?;
    let candidate = stdlib.dir.parent()?.join("actors");
    is_actors_root(&candidate).then_some(candidate)
}

fn find_actors_from(start: &Path) -> Option<PathBuf> {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        let candidate = dir.join("haskell").join("actors");
        if is_actors_root(&candidate) {
            return Some(candidate);
        }
        cur = dir.parent();
    }
    None
}

/// `Tidepool/Check.hs` is an ordinary production module directly under the
/// actors root, present in every checkout and not excluded by
/// `build.rs`'s `collect` filters — a stable sentinel for "this directory is
/// `bridge/haskell/actors`".
fn is_actors_root(dir: &Path) -> bool {
    dir.join("Tidepool").join("Check.hs").is_file()
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
        // Always create the destination itself: an empty bundle (dev builds
        // emit one for both EMBEDDED_STDLIB and EMBEDDED_EXOMONAD_HASKELL —
        // see build.rs) has no entries to derive a parent directory from, but
        // the sentinel write below still targets a file inside it.
        std::fs::create_dir_all(&destination)?;
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
    let fallbacks = tidepool_toolchain::toolchain::StdlibFallbacks {
        bundle: Some(ensure_embedded_stdlib()?),
        build_tree: None,
    };
    Ok(tidepool_toolchain::toolchain::locate_stdlib(&fallbacks)?.dir)
}

/// Exomonad's frozen library must match `source_identity`, independent of launch
/// cwd or development overrides used by the general Tidepool tools.
pub(crate) fn ensure_embedded_stdlib() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let hash = content_hash(EMBEDDED_STDLIB);
    materialize(
        EMBEDDED_STDLIB,
        tidepool_toolchain::paths::stdlib_dir(&hash),
    )
}

/// Resolve the Haskell modules used by Exomonad's interactive workbench: the
/// embedded bundle when this build carries one, otherwise the checkout's
/// `bridge/haskell/actors` located on disk.
pub fn ensure_exomonad_haskell() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if EMBEDDED_EXOMONAD_HASKELL.is_empty() {
        return locate_exomonad_haskell().ok_or_else(|| {
            "this build has no embedded Exomonad Haskell and no bridge/haskell/actors was found \
             above the current directory; build with TIDEPOOL_EMBED_HASKELL=1 or run inside the checkout"
                .into()
        });
    }
    let hash = content_hash(EMBEDDED_EXOMONAD_HASKELL);
    materialize(
        EMBEDDED_EXOMONAD_HASKELL,
        tidepool_toolchain::paths::cache_dir()
            .join("exomonad-haskell")
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
        if EMBEDDED_STDLIB.is_empty() {
            // TIDEPOOL_EMBED_HASKELL is unset for this build: build.rs emits
            // an empty bundle by design (see its doc comment), so there is no
            // embedded content to assert on here. Rerun with
            // TIDEPOOL_EMBED_HASKELL=1 to exercise this check.
            return;
        }
        assert!(EMBEDDED_STDLIB
            .iter()
            .any(|(path, _)| *path == "Tidepool/Internal/ExitCell.hs"));
        assert!(!EMBEDDED_STDLIB
            .iter()
            .any(|(path, _)| *path == "Tidepool/Internal/DataTextProbe.hs"));
    }

    #[test]
    fn find_actors_from_locates_bridge_haskell_actors_from_any_descendant() {
        // Mirrors `locate_stdlib`'s walk: the sentinel is found from any
        // descendant of the `bridge` ancestor directory — e.g. a crate nested
        // under `bridge/facade`, not just `bridge` itself.
        let root = tempfile::tempdir().unwrap();
        let actors = root.path().join("bridge/haskell/actors");
        std::fs::create_dir_all(actors.join("Tidepool")).unwrap();
        std::fs::write(
            actors.join("Tidepool/Check.hs"),
            "module Tidepool.Check where\n",
        )
        .unwrap();

        let deep = root.path().join("bridge/facade/some/deeply/nested/cwd");
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
        let deep = root.path().join("bridge/facade/some/other/tree");
        std::fs::create_dir_all(&deep).unwrap();
        assert!(find_actors_from(&deep).is_none());
    }

    #[test]
    fn find_actors_from_returns_none_when_the_sentinel_module_is_missing() {
        let root = tempfile::tempdir().unwrap();
        // The directory exists but lacks the sentinel module, e.g. a
        // half-populated checkout.
        std::fs::create_dir_all(root.path().join("bridge/haskell/actors/Tidepool")).unwrap();
        let deep = root.path().join("bridge/facade/some/cwd");
        std::fs::create_dir_all(&deep).unwrap();
        assert!(find_actors_from(&deep).is_none());
    }

    #[test]
    fn dev_source_identity_from_changes_when_an_input_directory_changes() {
        let root = tempfile::tempdir().unwrap();
        let stdlib = root.path().join("lib");
        let actors = root.path().join("actors");
        std::fs::create_dir_all(&stdlib).unwrap();
        std::fs::create_dir_all(&actors).unwrap();
        std::fs::write(actors.join("Check.hs"), "module Tidepool.Check where\n").unwrap();

        let before = dev_source_identity_from(Some(stdlib.clone()), Some(actors.clone()));
        std::fs::write(
            actors.join("Check.hs"),
            "module Tidepool.Check where\n-- changed\n",
        )
        .unwrap();
        let after = dev_source_identity_from(Some(stdlib.clone()), Some(actors.clone()));
        assert_ne!(
            before, after,
            "changing an actors file must change the identity"
        );

        let missing_actors = dev_source_identity_from(Some(stdlib), None);
        assert_ne!(
            after, missing_actors,
            "a missing tree must not silently collide with one that was found"
        );
    }
}
