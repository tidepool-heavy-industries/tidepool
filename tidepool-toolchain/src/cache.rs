//! One recipe-keyed, atomically published artifact bundle per compilation.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

/// Hash a length-prefixed field: unambiguous framing regardless of content.
fn frame(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// Source snapshot identity, independent from compiler dependency selection.
struct DependencyManifest {
    files: Vec<(PathBuf, Vec<u8>)>,
}

impl DependencyManifest {
    fn fingerprint(&self, prefix: &Path, hasher: &mut blake3::Hasher) {
        frame(hasher, &(self.files.len() as u64).to_le_bytes());
        for (rel, digest) in &self.files {
            frame(hasher, prefix.join(rel).as_os_str().as_encoded_bytes());
            frame(hasher, digest);
        }
    }
}

/// Enumerate every source form GHC can resolve as a Haskell home module from
/// an import path. Entries are relative to `root`, globally sorted, and carry
/// a tagged content digest (`1 || blake3(content)`, or `0` when unreadable).
///
/// `visited` holds the CANONICALIZED path of every directory already walked:
/// `path.is_dir()` follows symlinks, so a directory symlink under an include
/// dir that (directly or transitively) points back at an ancestor would
/// otherwise recurse forever. Canonicalizing and checking membership before
/// descending breaks the cycle (and, as a side effect, a diamond of two
/// symlinks to the same real directory is only hashed once).
fn dependency_source_manifest(root: &Path) -> DependencyManifest {
    let mut manifest = DependencyManifest { files: Vec::new() };
    let mut visited = std::collections::HashSet::new();
    collect_dependency_sources(root, root, &mut manifest, &mut visited);
    manifest.files.sort_by(|(a, _), (b, _)| a.cmp(b));
    manifest
}

/// The per-file content manifest used for source revision identities, as
/// readable data: each Haskell home-module source under `root`, by its path
/// RELATIVE to `root`, paired with the hex digest of its bytes. Unreadable
/// files carry an empty digest, exactly as they contribute an absent-marker to
/// the key.
///
/// Callers that need a content identity for a set of source roots — a live
/// source revision, say — must build it from this rather than from a second
/// directory walk, so "what the compiler keys on" and "what a revision is
/// named by" cannot drift apart.
#[must_use]
pub fn source_root_manifest(root: &Path) -> Vec<(PathBuf, String)> {
    dependency_source_manifest(root)
        .files
        .into_iter()
        .map(|(rel, digest)| {
            let hex = match digest.split_first() {
                Some((1, bytes)) => hex_digest(bytes),
                _ => String::new(),
            };
            (rel, hex)
        })
        .collect()
}

/// One content identity for an ORDERED set of source roots, framed exactly as
/// root count, then each root's
/// relative-path manifest in argument order. `domain` separates one caller's
/// identities from another's.
///
/// Order matters for the same reason it matters to the cache key: GHC's search
/// path decides module shadowing, so `[A, B]` and `[B, A]` are different
/// compilations and must not share an identity.
#[must_use]
pub fn source_roots_identity(domain: &[u8], roots: &[PathBuf]) -> String {
    let mut hasher = blake3::Hasher::new();
    frame(&mut hasher, domain);
    frame(&mut hasher, &(roots.len() as u64).to_le_bytes());
    for root in roots {
        dependency_source_manifest(root).fingerprint(Path::new(""), &mut hasher);
    }
    hasher.finalize().to_hex().to_string()
}

fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn collect_dependency_sources(
    root: &Path,
    dir: &Path,
    out: &mut DependencyManifest,
    visited: &mut std::collections::HashSet<PathBuf>,
) {
    if let Ok(canon) = fs::canonicalize(dir) {
        if !visited.insert(canon) {
            return;
        }
    }

    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(std::result::Result::ok).collect();
    entries.sort_by_key(std::fs::DirEntry::path);
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_dependency_sources(root, &path, out, visited);
            continue;
        }
        if !is_haskell_dependency_source(&path) {
            continue;
        }
        let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        // `fs::read` follows source symlinks, matching what GHC compiles.
        let digest = match fs::read(&path) {
            Ok(bytes) => {
                let mut digest = vec![1];
                digest.extend_from_slice(blake3::hash(&bytes).as_bytes());
                digest
            }
            Err(_) => vec![0],
        };
        out.files.push((rel, digest));
    }
}

fn is_haskell_dependency_source(path: &Path) -> bool {
    let Some(name) = path.file_name() else {
        return false;
    };
    [b".hs".as_slice(), b".hs-boot", b".lhs", b".lhs-boot"]
        .iter()
        .any(|suffix| name.as_encoded_bytes().ends_with(suffix))
}

/// The worker's consumed source and import-resolution evidence. Completeness
/// for cache reuse is independent from completeness for test selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyEvidence {
    pub version: u32,
    pub cache_safe: bool,
    pub selection_complete: bool,
    pub sources: Vec<SourceEvidence>,
    pub resolutions: Vec<ResolutionEvidence>,
    pub packages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEvidence {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolutionEvidence {
    pub module: String,
    pub selected: Option<PathBuf>,
    pub candidates: Vec<PathBuf>,
}

const GENERATED_SOURCE: &str = "@generated-source";

impl DependencyEvidence {
    /// Replace the request-local path only after checking the bytes the worker
    /// says it consumed. Authored dependencies retain their path identity.
    pub(crate) fn from_worker(bytes: &[u8], input: &Path, source: &str) -> Option<Self> {
        let mut evidence: Self = serde_json::from_slice(bytes).ok()?;
        let input = fs::canonicalize(input).ok()?;
        for item in &mut evidence.sources {
            if fs::canonicalize(&item.path).ok().as_ref() == Some(&input) {
                item.path = GENERATED_SOURCE.into();
            }
        }
        evidence.valid(source).then_some(evidence)
    }

    /// Validate contents and negative witnesses. IO errors are misses, including
    /// inaccessible candidates: absence must be known, not guessed.
    pub fn valid(&self, source: &str) -> bool {
        if self.version != 1 || !self.cache_safe || self.sources.is_empty() {
            return false;
        }
        let mut paths = std::collections::HashSet::new();
        let mut target = false;
        for item in &self.sources {
            if !paths.insert(&item.path) || item.sha256.len() != 64 {
                return false;
            }
            let digest = if item.path == Path::new(GENERATED_SOURCE) {
                target = true;
                hex_digest(&Sha256::digest(source.as_bytes()))
            } else {
                if !item.path.is_absolute() {
                    return false;
                }
                let Ok(bytes) = fs::read(&item.path) else {
                    return false;
                };
                hex_digest(&Sha256::digest(bytes))
            };
            if digest != item.sha256 {
                return false;
            }
        }
        if !target {
            return false;
        }
        for resolution in &self.resolutions {
            if resolution.module.is_empty() || resolution.candidates.is_empty() {
                return false;
            }
            if let Some(selected) = &resolution.selected {
                if resolution.candidates.last() != Some(selected) || !paths.contains(selected) {
                    return false;
                }
            }
            for candidate in &resolution.candidates {
                if !candidate.is_absolute() {
                    return false;
                }
                if Some(candidate) == resolution.selected.as_ref() {
                    continue;
                }
                match fs::metadata(candidate) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    _ => return false,
                }
            }
        }
        true
    }
}

/// A recipe names the source and the complete ordered compiler invocation.
/// Dependency contents are validated from worker evidence, not directory scans.
/// Unknown options and mutable session inputs cannot form a cache recipe.
pub struct Invocation<'a> {
    pub source: &'a str,
    pub argv: &'a [OsString],
    pub input_path: &'a Path,
    pub include: &'a [PathBuf],
    pub endpoint_identity: &'a [u8],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvocationKey(String);

impl std::fmt::Display for InvocationKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub fn invocation_key(inv: &Invocation<'_>) -> Option<InvocationKey> {
    if inv.endpoint_identity.is_empty() {
        return None;
    }
    let mut hasher = blake3::Hasher::new();
    // This version deliberately invalidates both former cache layouts.
    frame(&mut hasher, b"tidepool-compile-recipe-v2");
    frame(&mut hasher, inv.source.as_bytes());
    frame(&mut hasher, inv.input_path.file_name()?.as_encoded_bytes());
    let mut args = inv.argv.iter();
    let mut includes = Vec::new();
    let mut input_seen = false;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--output-dir") => {
                args.next()?;
            }
            Some("--build-products-dir") => {
                frame(&mut hasher, b"--build-products-dir");
                let path = std::path::absolute(PathBuf::from(args.next()?)).ok()?;
                frame(&mut hasher, path.as_os_str().as_encoded_bytes());
            }
            Some("--include") => {
                includes.push(PathBuf::from(args.next()?));
            }
            Some(flag @ ("--target" | "--targets")) => {
                frame(&mut hasher, flag.as_bytes());
                frame(&mut hasher, args.next()?.as_encoded_bytes());
            }
            _ if arg.as_os_str() == inv.input_path.as_os_str() && !input_seen => {
                input_seen = true;
            }
            _ => return None,
        }
    }
    if !input_seen || includes != inv.include {
        return None;
    }
    frame(&mut hasher, &(inv.include.len() as u64).to_le_bytes());
    for root in inv.include {
        let absolute = std::path::absolute(root).ok()?;
        frame(&mut hasher, absolute.as_os_str().as_encoded_bytes());
    }
    frame(&mut hasher, inv.endpoint_identity);
    Some(InvocationKey(hasher.finalize().to_hex().to_string()))
}

fn take_frame<'a>(remaining: &mut &'a [u8]) -> Option<&'a [u8]> {
    let (length, rest) = remaining.split_at_checked(8)?;
    let length = usize::try_from(u64::from_le_bytes(length.try_into().ok()?)).ok()?;
    let (value, rest) = rest.split_at_checked(length)?;
    *remaining = rest;
    Some(value)
}

fn append_frame(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// Read one immutable bundle; the evidence participates in the same integrity
/// manifest as every named artifact. Old layouts and malformed bundles miss.
pub(crate) fn artifacts_load(
    key: &InvocationKey,
    names: &[&str],
    source: &str,
) -> Option<Vec<Option<Vec<u8>>>> {
    let bytes = fs::read(crate::paths::compile_cache_dir().join(format!("{key}.bundle"))).ok()?;
    decode_bundle(&bytes, names, source)
}

fn decode_bundle(bytes: &[u8], names: &[&str], source: &str) -> Option<Vec<Option<Vec<u8>>>> {
    let mut remaining = bytes;
    let manifest = tidepool_extract_report::artifact_manifest::ArtifactManifest::decode(
        take_frame(&mut remaining)?,
    )
    .ok()?;
    if manifest.entries().len() != names.len() + 1 {
        return None;
    }
    let mut out = Vec::with_capacity(names.len());
    for (name, entry) in names
        .iter()
        .copied()
        .chain(std::iter::once("dependencies.json"))
        .zip(manifest.entries())
    {
        if name != entry.name() || entry.digest().is_none() {
            return None;
        }
        let value = take_frame(&mut remaining)?;
        if !entry.matches(value) {
            return None;
        }
        if name == "dependencies.json" {
            let evidence: DependencyEvidence = serde_json::from_slice(value).ok()?;
            if !evidence.valid(source) {
                return None;
            }
        } else {
            out.push(Some(value.to_vec()));
        }
    }
    remaining.is_empty().then_some(out)
}

pub(crate) fn artifacts_store(
    key: &InvocationKey,
    artifacts: &[(&str, Option<&[u8]>)],
    evidence: &DependencyEvidence,
    source: &str,
) {
    if !evidence.valid(source)
        || artifacts
            .iter()
            .any(|(name, bytes)| *name == "dependencies.json" || bytes.is_none())
    {
        return;
    }
    let Ok(dependencies) = serde_json::to_vec(evidence) else {
        return;
    };
    let mut artifacts = artifacts.to_vec();
    artifacts.push(("dependencies.json", Some(&dependencies)));
    let manifest = tidepool_extract_report::artifact_manifest::ArtifactManifest::from_artifacts(
        artifacts.iter().copied(),
    );
    let mut bundle = Vec::new();
    append_frame(&mut bundle, &manifest.encode());
    for (_, bytes) in artifacts {
        append_frame(&mut bundle, bytes.unwrap());
    }
    let dir = crate::paths::compile_cache_dir();
    if fs::create_dir_all(&dir).is_err() || !evidence.valid(source) {
        return;
    }
    let _ = tidepool_atomic_write::write_best_effort(&dir.join(format!("{key}.bundle")), &bundle);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(bytes: &[u8]) -> String {
        hex_digest(&Sha256::digest(bytes))
    }

    fn evidence(root: &Path) -> DependencyEvidence {
        let selected = root.join("later/Library.hs");
        fs::create_dir_all(selected.parent().unwrap()).unwrap();
        fs::create_dir_all(root.join("first")).unwrap();
        fs::write(&selected, "library = 1").unwrap();
        DependencyEvidence {
            version: 1,
            cache_safe: true,
            selection_complete: true,
            sources: vec![
                SourceEvidence {
                    path: GENERATED_SOURCE.into(),
                    sha256: digest(b"target"),
                },
                SourceEvidence {
                    path: selected.clone(),
                    sha256: digest(b"library = 1"),
                },
            ],
            resolutions: vec![ResolutionEvidence {
                module: "Library".into(),
                selected: Some(selected.clone()),
                candidates: vec![root.join("first/Library.hs"), selected],
            }],
            packages: vec!["base".into()],
        }
    }

    #[test]
    fn evidence_tracks_consumed_bytes_and_shadowing_not_unrelated_edits() {
        let root = tempfile::tempdir().unwrap();
        let evidence = evidence(root.path());
        assert!(evidence.valid("target"));
        fs::write(root.path().join("later/Unrelated.hs"), "anything").unwrap();
        assert!(evidence.valid("target"));
        let shadow = root.path().join("first/Library.hs");
        fs::write(&shadow, "library = 1").unwrap();
        assert!(!evidence.valid("target"));
        fs::remove_file(shadow).unwrap();
        assert!(evidence.valid("target"));
        fs::write(root.path().join("later/Library.hs"), "library = 2").unwrap();
        assert!(!evidence.valid("target"));
    }

    #[test]
    fn incomplete_incompatible_or_malformed_evidence_cannot_hit() {
        let root = tempfile::tempdir().unwrap();
        let good = evidence(root.path());
        assert!(!good.valid("changed target"));
        let mut bad = good.clone();
        bad.cache_safe = false;
        assert!(!bad.valid("target"));
        bad = good.clone();
        bad.version += 1;
        assert!(!bad.valid("target"));
        bad = good.clone();
        bad.sources.remove(0);
        assert!(!bad.valid("target"));
        bad = good.clone();
        bad.sources.push(bad.sources[0].clone());
        assert!(!bad.valid("target"));
        bad = good.clone();
        bad.resolutions[0].selected = Some(root.path().join("untracked.hs"));
        assert!(!bad.valid("target"));
        bad = good;
        bad.selection_complete = false;
        assert!(bad.valid("target"), "selection completeness is independent");
    }

    #[test]
    fn package_selection_tracks_absent_home_candidates() {
        let root = tempfile::tempdir().unwrap();
        let mut evidence = evidence(root.path());
        let candidate = root.path().join("first/Package.hs");
        evidence.resolutions.push(ResolutionEvidence {
            module: "Package".into(),
            selected: None,
            candidates: vec![candidate.clone()],
        });
        assert!(evidence.valid("target"));
        fs::write(candidate, "module Package where").unwrap();
        assert!(!evidence.valid("target"));
    }

    #[test]
    fn generated_source_identity_is_stable_and_publication_detects_races() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("Generated.hs");
        fs::write(&input, "target").unwrap();
        let mut evidence = evidence(root.path());
        evidence.sources[0].path = input.clone();
        let bytes = serde_json::to_vec(&evidence).unwrap();
        let normalized = DependencyEvidence::from_worker(&bytes, &input, "target").unwrap();
        assert_eq!(normalized.sources[0].path, Path::new(GENERATED_SOURCE));
        fs::write(
            root.path().join("later/Library.hs"),
            "changed during compile",
        )
        .unwrap();
        assert!(DependencyEvidence::from_worker(&bytes, &input, "target").is_none());
    }

    fn key(source: &str, input: &Path, roots: &[PathBuf], endpoint: &[u8]) -> InvocationKey {
        let mut argv = vec![
            input.as_os_str().to_owned(),
            "--target".into(),
            "result".into(),
        ];
        for root in roots {
            argv.extend(["--include".into(), root.as_os_str().to_owned()]);
        }
        invocation_key(&Invocation {
            source,
            argv: &argv,
            input_path: input,
            include: roots,
            endpoint_identity: endpoint,
        })
        .unwrap()
    }

    #[test]
    fn recipe_binds_source_logical_location_order_and_endpoint() {
        let a = key(
            "target",
            Path::new("/scratch/a/Generated.hs"),
            &[],
            b"endpoint",
        );
        assert_eq!(
            a,
            key(
                "target",
                Path::new("/scratch/b/Generated.hs"),
                &[],
                b"endpoint"
            )
        );
        assert_ne!(
            a,
            key("target", Path::new("/scratch/a/Other.hs"), &[], b"endpoint")
        );
        assert_ne!(
            a,
            key(
                "changed",
                Path::new("/scratch/a/Generated.hs"),
                &[],
                b"endpoint"
            )
        );
        assert_ne!(
            a,
            key(
                "target",
                Path::new("/scratch/a/Generated.hs"),
                &[],
                b"rebound"
            )
        );
        let roots = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        assert_ne!(
            key("target", Path::new("Generated.hs"), &roots, b"endpoint"),
            key(
                "target",
                Path::new("Generated.hs"),
                &roots.into_iter().rev().collect::<Vec<_>>(),
                b"endpoint"
            )
        );
    }

    #[test]
    fn unknown_options_and_session_requests_are_uncacheable() {
        for option in ["--session-root", "--inject-val", "--future-transform"] {
            let argv = vec!["Generated.hs".into(), option.into(), "value".into()];
            assert!(invocation_key(&Invocation {
                source: "target",
                argv: &argv,
                input_path: Path::new("Generated.hs"),
                include: &[],
                endpoint_identity: b"endpoint",
            })
            .is_none());
        }
    }

    #[test]
    fn bundle_integrity_binds_evidence_and_exact_named_artifacts() {
        let root = tempfile::tempdir().unwrap();
        let evidence = serde_json::to_vec(&evidence(root.path())).unwrap();
        let artifacts = [
            ("meta.cbor", Some(b"meta".as_slice())),
            ("dependencies.json", Some(evidence.as_slice())),
        ];
        let manifest =
            tidepool_extract_report::artifact_manifest::ArtifactManifest::from_artifacts(artifacts);
        let mut bytes = Vec::new();
        append_frame(&mut bytes, &manifest.encode());
        for (_, value) in artifacts {
            append_frame(&mut bytes, value.unwrap());
        }
        assert!(decode_bundle(&bytes, &["meta.cbor"], "target").is_some());
        assert!(decode_bundle(&bytes, &["other.cbor"], "target").is_none());
        assert!(decode_bundle(&bytes, &["meta.cbor"], "other source").is_none());
        let end = bytes.len() - 1;
        bytes[end] ^= 1;
        assert!(decode_bundle(&bytes, &["meta.cbor"], "target").is_none());
        assert!(decode_bundle(&bytes[..end], &["meta.cbor"], "target").is_none());
    }
}
