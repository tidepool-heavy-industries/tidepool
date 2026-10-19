//! Compiler-owned worlds and their exact cold input owners.

use std::any::Any;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use tidepool_extract_cmd::{InspectionNamespace, InspectionScope, StructuredInspection};

use crate::cache::SourceRootManifest;
use crate::checked_cell::CheckedValueArtifact;
use crate::declaration_context::ExactDeclarationContext;
use crate::exact_context::{ContextRoot, Materializer, StagedBlocks};
use crate::CompileError;

/// One complete captured root. Bodies include support files; the established
/// manifest authenticates the Haskell sources without inventing old header seals.
#[derive(Clone, Debug)]
pub struct CapturedCompilerSourceRoot {
    pub manifest: SourceRootManifest,
    pub bodies: BTreeMap<PathBuf, Arc<[u8]>>,
}

/// Immutable input data issued by the configured source capture. It does not
/// itself select a notebook world or grant an operation.
#[derive(Clone)]
pub struct CapturedCompilerSources {
    roots: Arc<[CapturedCompilerSourceRoot]>,
    origin: Arc<[u8]>,
    _owner: Arc<dyn Any + Send + Sync>,
    encoded: Arc<[u8]>,
    seal: [u8; 32],
}

impl fmt::Debug for CapturedCompilerSources {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CapturedCompilerSources")
            .field("roots", &self.roots.len())
            .field("encoded_bytes", &self.encoded.len())
            .field("seal", &self.seal)
            .finish_non_exhaustive()
    }
}

impl CapturedCompilerSources {
    /// Capture data only. The cold world builder couples it with the admitted
    /// primary selection and actual physical rows before the service issues a world.
    pub fn from_captured_roots(
        roots: Vec<CapturedCompilerSourceRoot>,
        origin: Arc<[u8]>,
        owner: Arc<dyn Any + Send + Sync>,
    ) -> Result<Self, CompileError> {
        u32::try_from(origin.len()).map_err(|_| source_size_failure())?;
        u32::try_from(roots.len()).map_err(|_| source_size_failure())?;
        let mut encoded_size = 8usize
            .checked_add(4)
            .and_then(|n| n.checked_add(origin.len()))
            .and_then(|n| n.checked_add(4))
            .ok_or_else(source_size_failure)?;
        for root in &roots {
            u32::try_from(root.bodies.len()).map_err(|_| source_size_failure())?;
            crate::host_work::checkpoint()?;
            encoded_size = encoded_size
                .checked_add(4)
                .ok_or_else(source_size_failure)?;
            for (path, body) in &root.bodies {
                crate::host_work::checkpoint()?;
                if path
                    .ancestors()
                    .skip(1)
                    .any(|parent| root.bodies.contains_key(parent))
                {
                    return Err(source_failure(format!(
                        "captured source file is also a directory: {}",
                        path.display()
                    )));
                }
                let path = relative_source_path(path)?;
                u32::try_from(path.len()).map_err(|_| source_size_failure())?;
                u32::try_from(body.len()).map_err(|_| source_size_failure())?;
                encoded_size = encoded_size
                    .checked_add(8)
                    .and_then(|n| n.checked_add(path.len()))
                    .and_then(|n| n.checked_add(body.len()))
                    .ok_or_else(source_size_failure)?;
            }
        }
        let allowance = capture_allowance()?;
        if encoded_size as u64 > allowance {
            return Err(source_failure(format!(
                "captured compiler sources require {encoded_size} bytes; request capture allowance is {allowance} bytes")));
        }
        for root in &roots {
            let actual = SourceRootManifest::from_file_digests(
                root.bodies
                    .iter()
                    .map(|(path, body)| (path.clone(), blake3::hash(body).to_hex().to_string())),
            )
            .map_err(|error| source_failure(error.to_string()))?;
            if !actual.files().eq(root.manifest.files()) {
                return Err(source_failure("captured compiler source namespace or bytes differ from the retained generation"));
            }
        }
        let mut encoded = Vec::with_capacity(encoded_size);
        encoded.extend_from_slice(b"TPCSRC01");
        source_frame(&mut encoded, &origin)?;
        source_count(&mut encoded, roots.len())?;
        for root in &roots {
            source_count(&mut encoded, root.bodies.len())?;
            for (path, body) in &root.bodies {
                crate::host_work::checkpoint()?;
                source_frame(&mut encoded, relative_source_path(path)?.as_bytes())?;
                source_frame(&mut encoded, body)?;
            }
        }
        let seal = Sha256::digest(&encoded).into();
        Ok(Self {
            roots: roots.into(),
            origin,
            _owner: owner,
            encoded: encoded.into(),
            seal,
        })
    }

    pub fn roots(&self) -> &[CapturedCompilerSourceRoot] {
        &self.roots
    }
    pub fn origin(&self) -> &[u8] {
        &self.origin
    }
    pub fn seal(&self) -> &[u8; 32] {
        &self.seal
    }
    pub(crate) fn encoded(&self) -> &[u8] {
        &self.encoded
    }
}

fn source_failure(message: impl Into<String>) -> CompileError {
    CompileError::ExtractFailed(message.into())
}

fn source_size_failure() -> CompileError {
    source_failure("captured compiler source size overflow")
}

fn capture_allowance() -> Result<u64, CompileError> {
    match std::env::var("TIDEPOOL_REQUEST_CAPTURE_BYTES") {
        Ok(value) => value
            .parse::<u64>()
            .ok()
            .filter(|amount| *amount > 0)
            .ok_or_else(|| {
                source_failure("TIDEPOOL_REQUEST_CAPTURE_BYTES must be a positive integer")
            }),
        Err(std::env::VarError::NotPresent) => Ok(4 * 1024 * 1024 * 1024),
        Err(error) => Err(source_failure(error.to_string())),
    }
}

fn relative_source_path(path: &Path) -> Result<&str, CompileError> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(source_failure(format!(
            "invalid captured relative source path {}",
            path.display()
        )));
    }
    path.to_str()
        .filter(|value| !value.contains('\0'))
        .ok_or_else(|| source_failure("captured source path is not non-NUL UTF-8"))
}

fn source_count(out: &mut Vec<u8>, count: usize) -> Result<(), CompileError> {
    out.extend_from_slice(
        &u32::try_from(count)
            .map_err(|_| source_size_failure())?
            .to_le_bytes(),
    );
    Ok(())
}

fn source_frame(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), CompileError> {
    source_count(out, bytes.len())?;
    out.extend_from_slice(bytes);
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicModuleQuestionKind {
    StructuredInfo,
    StructuredType,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicModuleQuestion {
    kind: PublicModuleQuestionKind,
    query: StructuredInspection,
}

impl PublicModuleQuestion {
    pub fn structured_info(module: String, namespace: InspectionNamespace, name: String) -> Self {
        Self::new(
            PublicModuleQuestionKind::StructuredInfo,
            module,
            namespace,
            name,
        )
    }
    pub fn structured_type(module: String, namespace: InspectionNamespace, name: String) -> Self {
        Self::new(
            PublicModuleQuestionKind::StructuredType,
            module,
            namespace,
            name,
        )
    }
    fn new(
        kind: PublicModuleQuestionKind,
        module: String,
        namespace: InspectionNamespace,
        name: String,
    ) -> Self {
        Self {
            kind,
            query: StructuredInspection {
                scope: InspectionScope::PublicModule(module),
                namespace,
                name,
                generation: 0,
                fingerprint: String::new(),
            },
        }
    }
    pub fn with_provenance(mut self, generation: u64, fingerprint: String) -> Self {
        self.query.generation = generation;
        self.query.fingerprint = fingerprint;
        self
    }
    pub fn kind(&self) -> PublicModuleQuestionKind {
        self.kind
    }
    pub fn module(&self) -> &str {
        match &self.query.scope {
            InspectionScope::PublicModule(module) => module,
            InspectionScope::Current => unreachable!("private public-module constructor"),
        }
    }
    pub fn namespace(&self) -> InspectionNamespace {
        self.query.namespace
    }
    pub fn name(&self) -> &str {
        &self.query.name
    }
    pub fn generation(&self) -> u64 {
        self.query.generation
    }
    pub fn fingerprint(&self) -> &str {
        &self.query.fingerprint
    }
}

/// Primary declaration/source facts and visible value rows remain separate.
/// Historical compiler/native support in a row is custody, not selected roots.
pub struct CompilerWorldOpenInputs {
    primary_context: Arc<ExactDeclarationContext>,
    primary_root: ContextRoot,
    primary_blocks: StagedBlocks,
    visible_rows: Vec<Arc<CheckedValueArtifact>>,
    sources: Arc<CapturedCompilerSources>,
    visibility: Arc<[u8]>,
    directory: Arc<tempfile::TempDir>,
}

impl CompilerWorldOpenInputs {
    pub(crate) fn from_primary_selection(
        primary_context: Arc<ExactDeclarationContext>,
        visible_rows: Vec<Arc<CheckedValueArtifact>>,
        sources: Arc<CapturedCompilerSources>,
        visibility: Arc<[u8]>,
        directory: Arc<tempfile::TempDir>,
    ) -> Result<Self, CompileError> {
        let snapshot = primary_context.snapshot();
        let store = fvm_ipld_blockstore::MemoryBlockstore::new();
        let mut materializer = Materializer::new(&store);
        let context = materializer
            .materialize_context(None, &snapshot)
            .map_err(|error| source_failure(error.to_string()))?;
        let primary_root = context.root().clone();
        let primary_blocks = materializer
            .finish()
            .map_err(|error| source_failure(error.to_string()))?;
        Ok(Self {
            primary_context,
            primary_root,
            primary_blocks,
            visible_rows,
            sources,
            visibility,
            directory,
        })
    }
}

pub struct CompilerWorldRestoration {
    logical_id: [u8; 32],
    inputs: CompilerWorldOpenInputs,
    product: Arc<[u8]>,
}

impl fmt::Debug for CompilerWorldRestoration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompilerWorldRestoration")
            .field("logical_id", &self.logical_id)
            .field("primary_root", &self.inputs.primary_root)
            .finish_non_exhaustive()
    }
}

impl CompilerWorldRestoration {
    pub fn logical_id(&self) -> &[u8; 32] {
        &self.logical_id
    }
}

#[derive(Clone, Debug)]
pub struct CompilerWorld {
    restoration: Arc<CompilerWorldRestoration>,
    realization: Arc<Mutex<Option<String>>>,
}

impl CompilerWorld {
    pub fn logical_id(&self) -> &[u8; 32] {
        &self.restoration.logical_id
    }
    pub fn restoration(&self) -> &Arc<CompilerWorldRestoration> {
        &self.restoration
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorldModuleOrigin {
    Home {
        root: u32,
        path: String,
        source_sha256: [u8; 32],
    },
    Package {
        profile: [u8; 32],
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldModuleReceipt {
    pub unit: String,
    pub module: String,
    pub origin: WorldModuleOrigin,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosedModuleAbsence {
    pub module: String,
    pub source_domain: [u8; 32],
    pub package_profile: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorldModuleResolution {
    Resolved(WorldModuleReceipt),
    Absent(ClosedModuleAbsence),
}

#[derive(Clone, Debug)]
pub struct WorldQueryResult {
    world: Arc<CompilerWorld>,
    question: PublicModuleQuestion,
    resolution: WorldModuleResolution,
    defining: Option<WorldModuleReceipt>,
    inspection_payload: Vec<u8>,
}

impl WorldQueryResult {
    pub fn world(&self) -> &Arc<CompilerWorld> {
        &self.world
    }
    pub fn question(&self) -> &PublicModuleQuestion {
        &self.question
    }
    pub fn resolution(&self) -> &WorldModuleResolution {
        &self.resolution
    }
    pub fn defining_module(&self) -> Option<&WorldModuleReceipt> {
        self.defining.as_ref()
    }
    pub fn inspection_payload(&self) -> &[u8] {
        &self.inspection_payload
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured(path: &str, body: &[u8]) -> CapturedCompilerSourceRoot {
        CapturedCompilerSourceRoot {
            manifest: SourceRootManifest::from_file_digests([(
                PathBuf::from(path),
                blake3::hash(body).to_hex().to_string(),
            )])
            .unwrap(),
            bodies: BTreeMap::from([(PathBuf::from(path), Arc::from(body))]),
        }
    }

    #[test]
    fn captured_sources_reject_changed_body_and_namespace_escape() {
        let mut changed = captured("Module.hs", b"module Module where\nx = 1\n");
        changed.bodies.insert(
            PathBuf::from("Module.hs"),
            Arc::from(&b"module Module where\nx = 2\n"[..]),
        );
        assert!(CapturedCompilerSources::from_captured_roots(
            vec![changed],
            Arc::from(&b"owner"[..]),
            Arc::new(())
        )
        .is_err());
        let escaped = captured("../Module.hs", b"module Module where\n");
        assert!(CapturedCompilerSources::from_captured_roots(
            vec![escaped],
            Arc::from(&b"owner"[..]),
            Arc::new(())
        )
        .is_err());
        let mut widened = captured("Module.hs", b"module Module where\n");
        widened.bodies.insert(
            PathBuf::from("Later.hs"),
            Arc::from(&b"module Later where\n"[..]),
        );
        assert!(CapturedCompilerSources::from_captured_roots(
            vec![widened],
            Arc::from(&b"owner"[..]),
            Arc::new(())
        )
        .is_err());
    }

    #[test]
    fn ordered_roots_and_unimported_bodies_bind_source_capture() {
        let a = captured("Module.hs", b"module Module where\nx = 1\n");
        let b = captured("Module.hs", b"module Module where\nx = 2\n");
        let make = |roots| {
            CapturedCompilerSources::from_captured_roots(
                roots,
                Arc::from(&b"owner"[..]),
                Arc::new(()),
            )
            .unwrap()
        };
        let ab = make(vec![a.clone(), b.clone()]);
        let ba = make(vec![b, a.clone()]);
        assert_ne!(ab.seal(), ba.seal());
        let mut extra = a;
        extra.bodies.insert(
            PathBuf::from("Unimported.hs"),
            Arc::from(&b"module Unimported where\ny = True\n"[..]),
        );
        extra.manifest = SourceRootManifest::from_file_digests(
            extra
                .bodies
                .iter()
                .map(|(path, body)| (path.clone(), blake3::hash(body).to_hex().to_string())),
        )
        .unwrap();
        assert_ne!(
            make(vec![extra]).seal(),
            make(vec![captured("Module.hs", b"module Module where\nx = 1\n")]).seal()
        );
    }
}
