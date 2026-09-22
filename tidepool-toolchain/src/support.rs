//! Immutable, compiler-owned Haskell support packages.
//!
//! A support package is source that Tidepool generates once but every compile
//! may import.  It is deliberately narrower than a session: its identity binds
//! the exact compiler producer and every exported source file, while row shims,
//! injected bindings, and session generations remain caller-owned mutable
//! inputs.  The manifest is published last and is the validity receipt for a
//! materialized package.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tidepool_extract_cmd::CompilerIdentity;

const MANIFEST: &str = "support-package-v1.json";
const FORMAT: u8 = 1;

/// One source file in an immutable support package. `path` is relative to the
/// include root (for example `Tidepool/Effects/Core.hs`).
#[derive(Debug, Clone, Copy)]
pub struct ImmutableSupportFile<'a> {
    pub path: &'a str,
    pub source: &'a str,
}

/// Exact source and public module surface of a reusable support package.
#[derive(Debug, Clone, Copy)]
pub struct ImmutableSupportPackage<'a> {
    pub name: &'a str,
    pub exports: &'a [&'a str],
    pub files: &'a [ImmutableSupportFile<'a>],
}

/// A validated immutable include root. The root can be supplied directly to a
/// compiler invocation; its identity is producer-bound and content-addressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImmutableSupport {
    root: PathBuf,
    producer_identity: String,
    content_identity: String,
    exports: Vec<String>,
}

impl ImmutableSupport {
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Stable identity of the compiler producer that may consume this package.
    #[must_use]
    pub fn producer_identity(&self) -> &str {
        &self.producer_identity
    }

    /// Content identity of the exact files and exports in this package.
    #[must_use]
    pub fn content_identity(&self) -> &str {
        &self.content_identity
    }

    #[must_use]
    pub fn exports(&self) -> &[String] {
        &self.exports
    }
}

/// Rejection while preparing an immutable support package.
#[derive(Debug, Error)]
pub enum ImmutableSupportError {
    #[error("invalid immutable support package: {0}")]
    Invalid(String),
    #[error("immutable support package I/O at {}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Manifest {
    format: u8,
    producer_identity: String,
    package: String,
    content_identity: String,
    exports: Vec<String>,
    files: Vec<ManifestFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ManifestFile {
    path: String,
    content_identity: String,
}

/// Stage `package` under the stable producer portion of `compiler`'s identity.
///
/// A daemon boot epoch intentionally does not enter the package key: it cannot
/// alter immutable source semantics, while frontend, worker, or GHC-libdir
/// changes do alter `CompilerIdentity::producer_hex` and receive a fresh root.
/// The manifest and every source digest are checked on every lookup; a reaped
/// or corrupted cache entry is repaired before its include root is returned.
pub fn stage_immutable_support(
    compiler: &CompilerIdentity,
    package: ImmutableSupportPackage<'_>,
) -> Result<ImmutableSupport, ImmutableSupportError> {
    stage_at(
        &crate::paths::compile_cache_dir(),
        &compiler.producer_hex(),
        package,
    )
}

fn stage_at(
    cache_root: &Path,
    producer_identity: &str,
    package: ImmutableSupportPackage<'_>,
) -> Result<ImmutableSupport, ImmutableSupportError> {
    let expected = expected_manifest(producer_identity, package)?;
    let root = cache_root
        .join("immutable-support")
        .join(producer_identity)
        .join(&expected.content_identity);

    // A package is a multi-file artifact. Serialize same-process repair so a
    // caller never observes our own half-published root; cross-process
    // publication below stages a complete sibling tree and renames it once.
    let guard = support_write_lock()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if !valid_at(&root, &expected) {
        materialize_at(&root, &expected, package)?;
    }
    drop(guard);

    Ok(ImmutableSupport {
        root,
        producer_identity: expected.producer_identity,
        content_identity: expected.content_identity,
        exports: expected.exports,
    })
}

fn support_write_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn expected_manifest(
    producer_identity: &str,
    package: ImmutableSupportPackage<'_>,
) -> Result<Manifest, ImmutableSupportError> {
    if package.name.is_empty()
        || !package
            .name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(ImmutableSupportError::Invalid(format!(
            "package name `{}` must contain only ASCII letters, digits, or `-`",
            package.name
        )));
    }
    if package.files.is_empty() || package.exports.is_empty() {
        return Err(ImmutableSupportError::Invalid(
            "packages need at least one source file and one export".to_owned(),
        ));
    }

    let mut files = BTreeMap::new();
    for file in package.files {
        validate_source_path(file.path)?;
        let module = module_name(file.path).ok_or_else(|| {
            ImmutableSupportError::Invalid(format!("`{}` is not a Haskell source path", file.path))
        })?;
        if !declares_module(file.source, &module) {
            return Err(ImmutableSupportError::Invalid(format!(
                "`{}` does not declare module `{module}`",
                file.path
            )));
        }
        if files.insert(file.path, file.source).is_some() {
            return Err(ImmutableSupportError::Invalid(format!(
                "duplicate source path `{}`",
                file.path
            )));
        }
    }

    let mut exports = BTreeSet::new();
    for export in package.exports {
        if !exports.insert((*export).to_owned()) {
            return Err(ImmutableSupportError::Invalid(format!(
                "duplicate export `{export}`"
            )));
        }
        let expected_path = format!("{}.hs", export.replace('.', "/"));
        if !files.contains_key(expected_path.as_str()) {
            return Err(ImmutableSupportError::Invalid(format!(
                "export `{export}` has no matching source file `{expected_path}`"
            )));
        }
    }

    let files: Vec<ManifestFile> = files
        .iter()
        .map(|(path, source)| ManifestFile {
            path: (*path).to_owned(),
            content_identity: digest(source.as_bytes()),
        })
        .collect();
    let exports: Vec<String> = exports.into_iter().collect();
    let content_identity = package_identity(package.name, &exports, &files);
    Ok(Manifest {
        format: FORMAT,
        producer_identity: producer_identity.to_owned(),
        package: package.name.to_owned(),
        content_identity,
        exports,
        files,
    })
}

fn validate_source_path(path: &str) -> Result<(), ImmutableSupportError> {
    let candidate = Path::new(path);
    if candidate.is_absolute()
        || !candidate.starts_with("Tidepool")
        || candidate
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("hs")
        || candidate.components().any(|component| {
            matches!(
                component,
                Component::ParentDir
                    | Component::CurDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
    {
        return Err(ImmutableSupportError::Invalid(format!(
            "source path `{path}` must be a relative `Tidepool/*.hs` path"
        )));
    }
    Ok(())
}

fn module_name(path: &str) -> Option<String> {
    path.strip_suffix(".hs").map(|stem| stem.replace('/', "."))
}

fn declares_module(source: &str, module: &str) -> bool {
    source.lines().any(|line| {
        let line = line.trim_start();
        line == format!("module {module} where")
            || line.starts_with(&format!("module {module} ("))
            || line.starts_with(&format!("module {module}\n"))
    })
}

fn package_identity(package: &str, exports: &[String], files: &[ManifestFile]) -> String {
    let mut hasher = blake3::Hasher::new();
    frame(&mut hasher, b"tidepool-immutable-support-v1");
    frame(&mut hasher, package.as_bytes());
    for export in exports {
        frame(&mut hasher, export.as_bytes());
    }
    for file in files {
        frame(&mut hasher, file.path.as_bytes());
        frame(&mut hasher, file.content_identity.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

fn digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn frame(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn valid_at(root: &Path, expected: &Manifest) -> bool {
    let manifest_path = root.join(MANIFEST);
    let Ok(bytes) = std::fs::read(manifest_path) else {
        return false;
    };
    let Ok(found) = serde_json::from_slice::<Manifest>(&bytes) else {
        return false;
    };
    if found != *expected {
        return false;
    }
    let expected_paths: BTreeSet<&str> = expected
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    if !expected.files.iter().all(|file| {
        std::fs::read(root.join(&file.path))
            .map(|bytes| digest(&bytes) == file.content_identity)
            .unwrap_or(false)
    }) {
        return false;
    }
    only_expected_files(root, &expected_paths)
}

/// A content-addressed include root is valid only when it contains precisely
/// the exported package's manifest and source tree. In particular, a stale
/// `Tidepool/Foo.hs` cannot become an unrecorded shadow witness merely because
/// the files the current package imports still hash correctly.
fn only_expected_files(root: &Path, expected: &BTreeSet<&str>) -> bool {
    let expected_dirs: BTreeSet<String> = expected
        .iter()
        .flat_map(|path| Path::new(path).ancestors().skip(1))
        .filter(|path| !path.as_os_str().is_empty())
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    fn walk(
        root: &Path,
        current: &Path,
        expected: &BTreeSet<&str>,
        expected_dirs: &BTreeSet<String>,
    ) -> bool {
        let Ok(entries) = std::fs::read_dir(current) else {
            return false;
        };
        for entry in entries {
            let Ok(entry) = entry else { return false };
            let path = entry.path();
            let Ok(relative) = path.strip_prefix(root) else {
                return false;
            };
            let text = relative.to_string_lossy();
            let Ok(file_type) = entry.file_type() else {
                return false;
            };
            if file_type.is_dir() {
                if !expected_dirs.contains(text.as_ref())
                    || !walk(root, &path, expected, expected_dirs)
                {
                    return false;
                }
            } else if file_type.is_file() {
                if text != MANIFEST && !expected.contains(text.as_ref()) {
                    return false;
                }
            } else {
                return false;
            }
        }
        true
    }
    walk(root, root, expected, &expected_dirs)
}

fn materialize_at(
    root: &Path,
    manifest: &Manifest,
    package: ImmutableSupportPackage<'_>,
) -> Result<(), ImmutableSupportError> {
    let parent = root.parent().ok_or_else(|| {
        ImmutableSupportError::Invalid(format!("support root `{}` has no parent", root.display()))
    })?;
    std::fs::create_dir_all(parent).map_err(|source| ImmutableSupportError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let staging = tempfile::Builder::new()
        .prefix("immutable-support-")
        .tempdir_in(parent)
        .map_err(|source| ImmutableSupportError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    let staged_root = staging.path();
    for file in package.files {
        let path = staged_root.join(file.path);
        let bytes = file.source.as_bytes();
        let file_parent = path.parent().ok_or_else(|| {
            ImmutableSupportError::Invalid(format!("source path `{}` has no parent", file.path))
        })?;
        std::fs::create_dir_all(file_parent).map_err(|source| ImmutableSupportError::Io {
            path: file_parent.to_path_buf(),
            source,
        })?;
        tidepool_atomic_write::write_best_effort(&path, bytes).map_err(|error| {
            ImmutableSupportError::Io {
                path: path.clone(),
                source: io::Error::other(error),
            }
        })?;
    }

    // The manifest is the receipt and is deliberately the final staged file.
    let manifest_path = staged_root.join(MANIFEST);
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|error| ImmutableSupportError::Io {
        path: manifest_path.clone(),
        source: io::Error::other(error),
    })?;
    tidepool_atomic_write::write_best_effort(&manifest_path, &bytes).map_err(|error| {
        ImmutableSupportError::Io {
            path: manifest_path,
            source: io::Error::other(error),
        }
    })?;

    // A valid root was already returned above. An invalid root is entirely
    // regenerable cache state, so replace it only after the sibling tree is
    // complete. A concurrent process may publish the same expected package
    // first; accept that winner after validating it rather than exposing a
    // partial directory or treating a benign race as a compiler failure.
    if root.exists() {
        std::fs::remove_dir_all(root).map_err(|source| ImmutableSupportError::Io {
            path: root.to_path_buf(),
            source,
        })?;
    }
    match std::fs::rename(staging.path(), root) {
        Ok(()) => Ok(()),
        Err(_source) if valid_at(root, manifest) => Ok(()),
        Err(source) => Err(ImmutableSupportError::Io {
            path: root.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CORE: &str = "module Tidepool.Effects.Core where\ncore = ()\n";
    const AUTHORED: &str = "module Tidepool.Effects.Authored where\nauthored = ()\n";

    fn stage(
        cache: &Path,
        producer: &str,
        core: &str,
    ) -> Result<ImmutableSupport, ImmutableSupportError> {
        let files = [
            ImmutableSupportFile {
                path: "Tidepool/Effects/Core.hs",
                source: core,
            },
            ImmutableSupportFile {
                path: "Tidepool/Effects/Authored.hs",
                source: AUTHORED,
            },
        ];
        stage_at(
            cache,
            producer,
            ImmutableSupportPackage {
                name: "tidepool-effects-core",
                exports: &["Tidepool.Effects.Core", "Tidepool.Effects.Authored"],
                files: &files,
            },
        )
    }

    #[test]
    fn producer_content_exports_and_manifest_define_reusable_support() {
        let cache = tempfile::tempdir().unwrap();
        let first = stage(cache.path(), "producer-a", CORE).unwrap();
        let again = stage(cache.path(), "producer-a", CORE).unwrap();
        let other_producer = stage(cache.path(), "producer-b", CORE).unwrap();
        let changed = stage(
            cache.path(),
            "producer-a",
            "module Tidepool.Effects.Core where\ncore = 1\n",
        )
        .unwrap();

        assert_eq!(first, again);
        assert_ne!(first.root(), other_producer.root());
        assert_ne!(first.root(), changed.root());
        assert_eq!(
            first.exports(),
            ["Tidepool.Effects.Authored", "Tidepool.Effects.Core"]
        );
        assert!(first.root().join(MANIFEST).is_file());
    }

    #[test]
    fn corrupt_or_missing_support_is_repaired_before_returning_include_root() {
        let cache = tempfile::tempdir().unwrap();
        let support = stage(cache.path(), "producer-a", CORE).unwrap();
        let core = support.root().join("Tidepool/Effects/Core.hs");
        std::fs::write(&core, "broken").unwrap();
        std::fs::remove_file(support.root().join(MANIFEST)).unwrap();

        let repaired = stage(cache.path(), "producer-a", CORE).unwrap();
        assert_eq!(repaired.root(), support.root());
        assert_eq!(std::fs::read_to_string(core).unwrap(), CORE);
        let files = [
            ImmutableSupportFile {
                path: "Tidepool/Effects/Core.hs",
                source: CORE,
            },
            ImmutableSupportFile {
                path: "Tidepool/Effects/Authored.hs",
                source: AUTHORED,
            },
        ];
        let expected = expected_manifest(
            "producer-a",
            ImmutableSupportPackage {
                name: "tidepool-effects-core",
                exports: &["Tidepool.Effects.Core", "Tidepool.Effects.Authored"],
                files: &files,
            },
        )
        .unwrap();
        assert!(valid_at(repaired.root(), &expected));
    }

    #[test]
    fn an_unrecorded_source_file_invalidates_and_is_removed_with_the_package() {
        let cache = tempfile::tempdir().unwrap();
        let support = stage(cache.path(), "producer-a", CORE).unwrap();
        let shadow = support.root().join("Tidepool/Effects/Shadow.hs");
        std::fs::write(&shadow, "module Tidepool.Effects.Shadow where\n").unwrap();

        let repaired = stage(cache.path(), "producer-a", CORE).unwrap();
        assert_eq!(repaired.root(), support.root());
        assert!(!shadow.exists());
    }

    #[test]
    fn rejects_paths_and_exports_outside_the_declared_immutable_surface() {
        let bad_path = ImmutableSupportPackage {
            name: "support",
            exports: &["Tidepool.Effects.Core"],
            files: &[ImmutableSupportFile {
                path: "../Core.hs",
                source: CORE,
            }],
        };
        assert!(matches!(
            expected_manifest("producer", bad_path),
            Err(ImmutableSupportError::Invalid(_))
        ));

        let missing_export = ImmutableSupportPackage {
            name: "support",
            exports: &["Tidepool.Other"],
            files: &[ImmutableSupportFile {
                path: "Tidepool/Effects/Core.hs",
                source: CORE,
            }],
        };
        assert!(matches!(
            expected_manifest("producer", missing_export),
            Err(ImmutableSupportError::Invalid(_))
        ));
    }
}
