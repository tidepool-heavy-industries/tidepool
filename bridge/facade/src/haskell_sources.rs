//! Haskell source bundles embedded in installed Tidepool binaries.
//!
//! The build script walks each complete source tree, but only when
//! `TIDEPOOL_EMBED_HASKELL=1` (release/install builds); otherwise it emits
//! empty bundles plus a build-bound source identity, and this module validates
//! the checkout before a run captures it. This
//! module owns the one content-addressed materializer used by both the
//! public Tidepool library and Exomonad's public surface and private
//! interactive driver.

use std::path::{Path, PathBuf};

include!(concat!(env!("OUT_DIR"), "/embedded_stdlib.rs"));
include!(concat!(env!("OUT_DIR"), "/embedded_exomonad_haskell.rs"));
include!(concat!(env!("OUT_DIR"), "/dev_source_identity.rs"));

pub(crate) const DEV_SOURCE_DOMAIN: &[u8] = b"tidepool-facade-dev-source-identity";

fn content_hash(entries: &[(&str, &str)]) -> String {
    // Length-prefix both fields so different path/content partitions cannot
    // produce the same byte stream.
    tidepool_toolchain::digest::of_parts(
        entries
            .iter()
            .flat_map(|(relative, content)| [relative.as_bytes(), content.as_bytes()]),
    )
}

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
/// stdlib path. A dev build refuses source bytes changed after its build.
pub(crate) fn runtime_source_roots(
    deployed_stdlib: Option<&Path>,
) -> Result<[PathBuf; 2], Box<dyn std::error::Error>> {
    let stdlib = match deployed_stdlib {
        Some(root) => root.to_path_buf(),
        None => ensure_builtin_stdlib()?,
    };
    let roots = [stdlib, ensure_exomonad_haskell()?];
    if let Some(expected) = DEV_SOURCE_IDENTITY {
        verify_dev_source_roots(&roots, expected)?;
    }
    Ok(roots)
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
        let actual = tidepool_toolchain::cache::source_root_manifest(root)?;
        let mut expected = entries
            .iter()
            .map(|(relative, content)| {
                (
                    PathBuf::from(relative),
                    blake3::hash(content.as_bytes()).to_hex().to_string(),
                )
            })
            .collect::<Vec<_>>();
        expected.sort_by(|a, b| a.0.cmp(&b.0));
        if actual != expected {
            let differing = actual
                .iter()
                .zip(&expected)
                .position(|(found, embedded)| found != embedded)
                .unwrap_or(actual.len().min(expected.len()));
            return Err(format!(
                "captured embedded Haskell library differs from this build at entry {differing}: found {:?}, embedded {:?}",
                actual.get(differing),
                expected.get(differing)
            )
            .into());
        }
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

/// The stdlib fallbacks a dev build carries: the source tree this binary was
/// built from (step 5 of the precedence table), so that every process of one
/// run, whatever its working directory, resolves the same trees. The CLI runs
/// from the checkout and the host runs in the project's directory; before
/// this, the two hashed different trees into `source_identity` and a fresh
/// run refused its own frozen workspace.
fn dev_fallbacks() -> tidepool_toolchain::toolchain::StdlibFallbacks {
    tidepool_toolchain::toolchain::StdlibFallbacks {
        bundle: None,
        build_tree: Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("../haskell/lib")),
    }
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
    let stdlib = tidepool_toolchain::toolchain::locate_stdlib(&dev_fallbacks()).ok()?;
    let candidate = stdlib.dir.parent()?.join("actors");
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

/// Resolve the configured deployment's immutable library, or use the ordinary
/// development overrides and embedded fallback when no catalog is configured.
pub fn ensure_stdlib() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(package) = tidepool_toolchain::toolchain::configured_module_package()? {
        return Ok(package.source_root().to_path_buf());
    }
    let fallbacks = tidepool_toolchain::toolchain::StdlibFallbacks {
        bundle: Some(ensure_builtin_stdlib()?),
        ..dev_fallbacks()
    };
    Ok(tidepool_toolchain::toolchain::locate_stdlib(&fallbacks)?.dir)
}

/// Resolve the immutable deployment root when configured; otherwise use this
/// binary's own library independently of launch cwd or development overrides.
///
/// A dev build embeds nothing, so there is no fixed bundle to materialize:
/// callers (the interactive driver's GHC include path, recipe checks) need a
/// real directory containing `Tidepool.Prelude` et al., not an empty
/// materialized stand-in. Run admission validates this selected checkout
/// against the identity embedded by the dev build before capturing it.
pub(crate) fn ensure_embedded_stdlib() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(package) = tidepool_toolchain::toolchain::configured_module_package()? {
        return Ok(package.source_root().to_path_buf());
    }
    ensure_builtin_stdlib()
}

fn ensure_builtin_stdlib() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if EMBEDDED_STDLIB.is_empty() {
        return tidepool_toolchain::toolchain::locate_stdlib(&dev_fallbacks())
            .map(|location| location.dir)
            .map_err(Into::into);
    }
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
    fn embedded_stdlib_retains_production_internal_modules() {
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
    fn dev_source_selection_rejects_an_edit_or_missing_tree() {
        let root = tempfile::tempdir().unwrap();
        let stdlib = root.path().join("lib");
        let actors = root.path().join("actors");
        std::fs::create_dir_all(&stdlib).unwrap();
        std::fs::create_dir_all(&actors).unwrap();
        std::fs::write(actors.join("Check.hs"), "module Tidepool.Check where\n").unwrap();

        let roots = [stdlib.clone(), actors.clone()];
        let before =
            tidepool_toolchain::cache::source_roots_identity(DEV_SOURCE_DOMAIN, &roots).unwrap();
        verify_dev_source_roots(&roots, &before).unwrap();
        std::fs::write(
            actors.join("Check.hs"),
            "module Tidepool.Check where\n-- changed\n",
        )
        .unwrap();
        assert!(verify_dev_source_roots(&roots, &before).is_err());
        std::fs::remove_dir_all(actors).unwrap();
        assert!(verify_dev_source_roots(&roots, &before).is_err());
    }
}
