//! Compiler-owned worlds and their exact cold input owners.

use std::any::Any;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use tidepool_extract_cmd::{InspectionNamespace, InspectionScope, StructuredInspection};

use crate::artifact_inventory::{ArtifactBinding, CanonicalProducerIdentity};
use crate::cache::SourceRootManifest;
use crate::checked_cell::CheckedValueArtifact;
use crate::declaration_context::ExactDeclarationContext;
use crate::exact_context::{ContextRoot, MaterializedMap, Materializer, ReachableBlocks};
use crate::CompileError;

/// One closed captured source namespace. The capture owner decides its supported
/// file cohort; this type does not imply that foreign headers were captured.
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
    selected_sources: Arc<BTreeMap<String, WorldModuleOrigin>>,
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
    /// Bound source retention during the existing owner traversal, before bodies
    /// accumulate. The factory also checks exact framed encoding size.
    pub fn capture_byte_allowance() -> Result<u64, CompileError> {
        capture_allowance()
    }

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
        let mut ordering_scratch = 0usize;
        for root in &roots {
            ordering_scratch = ordering_scratch.max(
                root.bodies
                    .len()
                    .checked_mul(std::mem::size_of::<(&PathBuf, &Arc<[u8]>)>())
                    .ok_or_else(source_size_failure)?,
            );
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
        let required = encoded_size
            .checked_add(ordering_scratch)
            .ok_or_else(source_size_failure)?;
        if required as u64 > allowance {
            return Err(source_failure(format!(
                "captured compiler sources require {required} bytes including canonical ordering scratch; request capture allowance is {allowance} bytes")));
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
            // PathBuf compares components, while the wire compares the complete
            // UTF-8 relative path. Preserve root precedence and sort only the
            // borrowed, already validated cold namespace for canonical transfer.
            let mut ordered = root.bodies.iter().collect::<Vec<_>>();
            ordered.sort_unstable_by(|(left, _), (right, _)| {
                left.as_os_str()
                    .as_encoded_bytes()
                    .cmp(right.as_os_str().as_encoded_bytes())
            });
            for (path, body) in ordered {
                crate::host_work::checkpoint()?;
                source_frame(&mut encoded, relative_source_path(path)?.as_bytes())?;
                source_frame(&mut encoded, body)?;
            }
        }
        let seal = world_digest(&encoded);
        // Restricted conventional filename precedence is a primary captured
        // namespace fact. It is not a copy of GHC's hydrated HPT/import world.
        let mut selected_sources = BTreeMap::new();
        for (root, captured) in roots.iter().enumerate() {
            for (path, body) in &captured.bodies {
                crate::host_work::checkpoint()?;
                if !matches!(
                    path.extension().and_then(|s| s.to_str()),
                    Some("hs" | "lhs")
                ) {
                    continue;
                }
                let module = path
                    .with_extension("")
                    .components()
                    .map(|part| {
                        part.as_os_str()
                            .to_str()
                            .expect("validated source path")
                            .to_owned()
                    })
                    .collect::<Vec<_>>()
                    .join(".");
                selected_sources
                    .entry(module)
                    .or_insert_with(|| WorldModuleOrigin::Home {
                        root: root as u32,
                        path: path.to_str().expect("validated source path").to_owned(),
                        source_sha256: world_digest(body),
                    });
            }
        }
        Ok(Self {
            roots: roots.into(),
            origin,
            _owner: owner,
            encoded: encoded.into(),
            seal,
            selected_sources: Arc::new(selected_sources),
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
pub struct CompilerWorldBinding<'a> {
    occurrence: &'a str,
    symbol: &'a tidepool_repr::execution_schema::SymbolIdentity,
    session_var: tidepool_repr::SessionVarId,
    defining_scope: u64,
    row: Arc<CheckedValueArtifact>,
}

impl<'a> CompilerWorldBinding<'a> {
    /// Borrow data from the runtime's one publication capture. This does not
    /// issue a runtime client, world, source lease or operation admission.
    pub fn from_checked_value(
        occurrence: &'a str,
        symbol: &'a tidepool_repr::execution_schema::SymbolIdentity,
        session_var: tidepool_repr::SessionVarId,
        defining_scope: u64,
        row: Arc<CheckedValueArtifact>,
    ) -> Self {
        Self {
            occurrence,
            symbol,
            session_var,
            defining_scope,
            row,
        }
    }
}

#[derive(Clone)]
struct WorldBindingFact {
    symbol: tidepool_repr::execution_schema::SymbolIdentity,
    session_var: tidepool_repr::SessionVarId,
    defining_scope: u64,
    row: Arc<CheckedValueArtifact>,
}

/// The current occurrence table, coupled to genuine checked rows by its
/// publication owner. Aliases keep independent names and share their row.
pub struct CompilerWorldBindings {
    query_scope: u64,
    entries: imbl::OrdMap<String, WorldBindingFact>,
    metadata_bytes: u64,
}

// The occurrence is the existing SnapshotKey<String>. The row link names the
// independently materialized exact semantic row plus its original issuer.
// Compiler generation comes from that retained row owner; SessionVarId remains
// an independent runtime identity and never supplies a compiler generation.
type WorldBindingSnapshotFact = (
    (String, String, String, String, Option<String>),
    u64,
    u64,
    u64,
    cid::Cid,
);

impl CompilerWorldBindings {
    /// Charge borrowed metadata before cloning it. Callers can feed the actual
    /// BindingTable iterator directly; no intermediate all-binding Vec is needed.
    pub fn from_published_bindings<'a>(
        query_scope: u64,
        bindings: impl IntoIterator<Item = CompilerWorldBinding<'a>>,
    ) -> Result<Self, CompileError> {
        let allowance = capture_allowance()?;
        let mut reserved = 128u64;
        if reserved > allowance {
            return Err(binding_capture_refusal(reserved, allowance));
        }
        let mut entries = imbl::OrdMap::new();
        for input in bindings {
            crate::host_work::checkpoint()?;
            let symbol = input.symbol;
            let fields = [
                input.occurrence,
                symbol.unit.as_str(),
                symbol.module.as_str(),
                symbol.namespace.as_str(),
                symbol.occurrence.as_str(),
            ];
            if fields
                .iter()
                .any(|field| field.is_empty() || field.contains('\0'))
                || symbol
                    .record_parent
                    .as_ref()
                    .is_some_and(|parent| parent.contains('\0'))
            {
                return Err(source_failure("invalid current compiler binding identity"));
            }
            let bytes = binding_metadata_bound(
                fields.map(str::len),
                symbol.record_parent.as_ref().map_or(0, |p| p.len()),
            )?;
            reserved = reserved
                .checked_add(bytes)
                .ok_or_else(source_size_failure)?;
            if reserved > allowance {
                return Err(binding_capture_refusal(reserved, allowance));
            }
            if entries.contains_key(input.occurrence) {
                return Err(source_failure("duplicate visible compiler occurrence"));
            }
            entries.insert(
                input.occurrence.to_owned(),
                WorldBindingFact {
                    symbol: symbol.clone(),
                    session_var: input.session_var,
                    defining_scope: input.defining_scope,
                    row: input.row,
                },
            );
        }
        Ok(Self {
            query_scope,
            entries,
            metadata_bytes: reserved,
        })
    }

    pub(crate) fn cold_rows(&self) -> Result<Vec<Arc<CheckedValueArtifact>>, CompileError> {
        let mut rows = BTreeMap::<String, Arc<CheckedValueArtifact>>::new();
        for entry in self.entries.values() {
            crate::host_work::checkpoint()?;
            crate::host_work::record_host_work(
                crate::host_work::HostWorkCounter::ColdWorldBuilderRowVisit,
                1,
            );
            let owner = entry.row.owner().module_name();
            if let Some(previous) = rows.get(&owner) {
                if !previous.same_world_type_row(&entry.row) {
                    return Err(source_failure(
                        "competing checked rows for one current compiler owner",
                    ));
                }
            } else {
                rows.insert(owner, Arc::clone(&entry.row));
            }
        }
        Ok(rows.into_values().collect())
    }

    fn cold_snapshot(
        &self,
        rows: &BTreeMap<String, cid::Cid>,
    ) -> Result<imbl::OrdMap<String, WorldBindingSnapshotFact>, CompileError> {
        let mut facts = imbl::OrdMap::new();
        for (occurrence, entry) in &self.entries {
            crate::host_work::checkpoint()?;
            let row = rows.get(&entry.row.owner().module_name()).ok_or_else(|| {
                source_failure("current compiler binding lacks its exact materialized row")
            })?;
            let symbol = &entry.symbol;
            facts.insert(
                occurrence.clone(),
                (
                    (
                        symbol.unit.clone(),
                        symbol.module.clone(),
                        symbol.namespace.clone(),
                        symbol.occurrence.clone(),
                        symbol.record_parent.clone(),
                    ),
                    entry.session_var.raw(),
                    entry.defining_scope,
                    entry.row.owner().gen.0,
                    *row,
                ),
            );
        }
        Ok(facts)
    }
}

pub struct CompilerWorldOpenInputs {
    primary_context: Arc<ExactDeclarationContext>,
    visible_rows: Vec<Arc<CheckedValueArtifact>>,
    bindings: CompilerWorldBindings,
    sources: Arc<CapturedCompilerSources>,
    directory: Arc<tempfile::TempDir>,
}

impl CompilerWorldOpenInputs {
    pub(crate) fn from_primary_selection(
        primary_context: Arc<ExactDeclarationContext>,
        visible_rows: Vec<Arc<CheckedValueArtifact>>,
        bindings: CompilerWorldBindings,
        sources: Arc<CapturedCompilerSources>,
        directory: Arc<tempfile::TempDir>,
    ) -> Result<Self, CompileError> {
        Ok(Self {
            primary_context,
            visible_rows,
            bindings,
            sources,
            directory,
        })
    }
}

pub struct CompilerWorldRestoration {
    logical_id: [u8; 32],
    inputs: CompilerWorldOpenInputs,
    product: Arc<[u8]>,
    primary_root: ContextRoot,
    primary_blocks: ReachableBlocks,
    primary_associations: MaterializedMap<ArtifactBinding, cid::Cid>,
    bindings: MaterializedMap<String, WorldBindingSnapshotFact>,
    producer: [u8; 32],
    profile: [u8; 32],
    descriptor: PathBuf,
    input_files: Vec<Arc<std::fs::File>>,
}

impl fmt::Debug for CompilerWorldRestoration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompilerWorldRestoration")
            .field("logical_id", &self.logical_id)
            .field("primary_root", &self.primary_root)
            .field("primary_frontier_work", &self.primary_blocks.work())
            .field("primary_association_root", self.primary_associations.root())
            .field("binding_root", self.bindings.root())
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
    Rejected(WorldQueryRejection),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorldQueryRejection {
    Unsupported,
    Unavailable,
    Corrupt,
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
    fn captured_source_wire_uses_complete_utf8_path_order() {
        let bodies: BTreeMap<PathBuf, Arc<[u8]>> = [
            (
                "Tidepool/Effects/Core.hs",
                &b"module Tidepool.Effects.Core where"[..],
            ),
            ("Tidepool/Effects.hs", &b"module Tidepool.Effects where"[..]),
            (
                "Tidepool/Actor/Source.hs",
                &b"module Tidepool.Actor.Source where"[..],
            ),
            ("Tidepool/Actor.hs", &b"module Tidepool.Actor where"[..]),
            ("Tidepool/É.hs", &b"module Tidepool.Unicode where"[..]),
        ]
        .into_iter()
        .map(|(path, body)| (PathBuf::from(path), Arc::from(body)))
        .collect();
        let root = CapturedCompilerSourceRoot {
            manifest: SourceRootManifest::from_file_digests(
                bodies
                    .iter()
                    .map(|(path, body)| (path.clone(), blake3::hash(body).to_hex().to_string())),
            )
            .unwrap(),
            bodies,
        };
        // This fixture distinguishes component order from the receiving codec's
        // full-string order, rather than relying on the production sort twice.
        let captured = CapturedCompilerSources::from_captured_roots(
            vec![root.clone(), root],
            Arc::from(&b"owner"[..]),
            Arc::new(()),
        )
        .unwrap();
        fn count(input: &mut &[u8]) -> usize {
            let (word, rest) = input.split_at(4);
            *input = rest;
            u32::from_le_bytes(word.try_into().unwrap()) as usize
        }
        fn frame<'a>(input: &mut &'a [u8]) -> &'a [u8] {
            let length = count(input);
            let (value, rest) = input.split_at(length);
            *input = rest;
            value
        }
        let mut input = &captured.encoded()[8..];
        assert_eq!(frame(&mut input), b"owner");
        assert_eq!(count(&mut input), 2);
        for _ in 0..2 {
            assert_eq!(count(&mut input), 5);
            let mut paths = Vec::new();
            for _ in 0..5 {
                paths.push(std::str::from_utf8(frame(&mut input)).unwrap());
                frame(&mut input);
            }
            assert_eq!(
                paths,
                [
                    "Tidepool/Actor.hs",
                    "Tidepool/Actor/Source.hs",
                    "Tidepool/Effects.hs",
                    "Tidepool/Effects/Core.hs",
                    "Tidepool/É.hs"
                ]
            );
            assert!(paths
                .windows(2)
                .all(|pair| pair[0].as_bytes() < pair[1].as_bytes()));
        }
        assert!(input.is_empty());
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

pub(crate) fn world_bytes(value: &ciborium::value::Value) -> Result<Vec<u8>, CompileError> {
    crate::host_work::record_host_work(crate::host_work::HostWorkCounter::SemanticEncoding, 1);
    struct Bounded {
        bytes: Vec<u8>,
        remaining: u64,
    }
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() as u64 > self.remaining {
                return Err(std::io::Error::other(
                    "compiler world encoding exceeds capture allowance",
                ));
            }
            self.bytes.extend_from_slice(bytes);
            self.remaining -= bytes.len() as u64;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded {
        bytes: Vec::new(),
        remaining: capture_allowance()?,
    };
    ciborium::ser::into_writer(value, &mut writer).map_err(|e| source_failure(e.to_string()))?;
    crate::host_work::record_host_work(
        crate::host_work::HostWorkCounter::ScopeEncodedByte,
        writer.bytes.len() as u64,
    );
    Ok(writer.bytes)
}

fn world_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, b| {
            write!(out, "{b:02x}").expect("String formatting");
            out
        })
}
fn world_digest(bytes: &[u8]) -> [u8; 32] {
    crate::host_work::record_host_work(
        crate::host_work::HostWorkCounter::SemanticDigestInputByte,
        bytes.len() as u64,
    );
    Sha256::digest(bytes).into()
}
fn world_text(value: impl Into<String>) -> ciborium::value::Value {
    ciborium::value::Value::Text(value.into())
}
fn world_blob(bytes: impl AsRef<[u8]>) -> ciborium::value::Value {
    ciborium::value::Value::Bytes(bytes.as_ref().to_vec())
}
fn world_array(values: Vec<ciborium::value::Value>) -> ciborium::value::Value {
    ciborium::value::Value::Array(values)
}
fn world_path(path: &Path) -> Result<ciborium::value::Value, CompileError> {
    path.to_str()
        .filter(|v| path.is_absolute() && !v.contains('\0'))
        .map(world_text)
        .ok_or_else(|| source_failure("world input path must be absolute UTF-8"))
}

fn binding_capture_refusal(estimated: u64, allowance: u64) -> CompileError {
    source_failure(format!("compiler binding capture/staging reservation requires {estimated} bytes; configured capture allowance is {allowance} bytes; actual encoded size UNKNOWN"))
}

// Bound the actual fixed HAMT representation before copying borrowed fields.
// SHA256 has at most 52 five-bit routing levels. Charge each occurrence for an
// entire private path: every node header plus a CID edge, even though real
// paths share nodes. This is 4759 bytes plus borrowed strings per occurrence,
// roughly 0.9 million small occurrences under the default 4 GiB capture allowance.
// It reserves staging work, not actual encoded bytes or optional cache retention.
// The separate row-fact check includes its exact requirements. If real workloads
// hit this pessimistic reservation, use actual-path preflight in Materializer.
fn binding_metadata_bound(fields: [usize; 5], parent_bytes: usize) -> Result<u64, CompileError> {
    let cid_link: u64 = 9 + 9 + 39;
    let hamt_node = 9 + (9 + 4) + 9;
    let routing = 52 * (hamt_node + cid_link);
    let key_bucket_heads = 5 * 9;
    let binding_heads = 2 * 9 + 5 * 9 + 3 * 9 + cid_link;
    fields.into_iter().chain([parent_bytes]).try_fold(
        routing + key_bucket_heads + binding_heads,
        |size, field| {
            size.checked_add(field as u64)
                .ok_or_else(source_size_failure)
        },
    )
}

fn binding_row_metadata_bound(
    unit: &str,
    module: &str,
    requires: &[crate::declaration_join::ExactModuleIdentity],
) -> Result<u64, CompileError> {
    // rowFact[semanticRow[unit,module,producer,image,role,requires],artifact,receipt]
    let fixed = 2 * 9 + 2 * 9 + 4 * (9 + 32) + (9 + 32) + 9;
    requires.iter().try_fold(
        fixed + unit.len() as u64 + module.len() as u64,
        |size, key| {
            size.checked_add(3 * 9)
                .and_then(|n| n.checked_add(key.unit.len() as u64))
                .and_then(|n| n.checked_add(key.module.len() as u64))
                .ok_or_else(source_size_failure)
        },
    )
}

// ArtifactBinding's derived codec is a two-field map of fixed 32-byte
// arrays. Reserve its key, both acquired bindings and the complete private
// HAMT path before staging any association or copying semantic row fields.
fn primary_association_metadata_bound(
    unit: &str,
    module: &str,
    requires: &[crate::declaration_join::ExactModuleIdentity],
) -> Result<u64, CompileError> {
    let binding = 9 + 2 * (9 + 9 + 32 * 9);
    let routing = 52 * ((9 + 9 + 4 + 9) + (9 + 9 + 39));
    binding_row_metadata_bound(unit, module, requires)?
        .checked_add(routing + 5 * 9 + 3 * binding + 9 + 20 + 1)
        .ok_or_else(source_size_failure)
}

struct WorldColdRows {
    producer: [u8; 32],
    files: BTreeMap<PathBuf, Arc<std::fs::File>>,
    images: BTreeMap<(String, String, String), ciborium::value::Value>,
    rows: BTreeMap<(String, String, String), ciborium::value::Value>,
    arenas: crate::declaration_context::original_inputs::OriginalInputArenaTable,
    diagnostics: Option<crate::artifacts::InputArtifactDiagnostics>,
}
impl WorldColdRows {
    fn row(
        &mut self,
        entry: &crate::artifact_inventory::ArtifactEntry,
        parts: Vec<crate::declaration_context::original_inputs::OriginalInputPart>,
    ) -> Result<(ciborium::value::Value, [u8; 32]), CompileError> {
        use crate::artifact_inventory::{ArtifactPayload, JoinedInterfaceRole};
        crate::host_work::record_host_work(
            crate::host_work::HostWorkCounter::ColdWorldBuilderRowVisit,
            1,
        );
        crate::host_work::record_host_work(
            crate::host_work::HostWorkCounter::InputFdSelectedRowVisit,
            1,
        );
        crate::host_work::record_host_work(
            crate::host_work::HostWorkCounter::InputFdSliceVisit,
            parts.len() as u64,
        );
        use crate::declaration_context::original_inputs::{encode_image, OriginalInputKind as K};
        use ciborium::value::Value;
        if entry.descriptor.producer_sha256 != self.producer {
            return Err(source_failure(
                "world row has a different compiler producer",
            ));
        }
        if let Some(capture) = &mut self.diagnostics {
            if let Err(error) = capture.original_parts(
                &self.producer,
                &entry.descriptor.owner.unit,
                &entry.descriptor.owner.module,
                &parts,
            ) {
                tracing::warn!(%error,"compiler world input diagnostics are incomplete");
            }
        }
        let find = |kind| {
            parts
                .iter()
                .find(|p| p.kind == kind)
                .ok_or_else(|| source_failure("world row lacks required type bytes"))
        };
        let iface = find(K::Interface)?;
        let packages = find(K::Packages)?;
        let canonical = match &entry.payload {
            ArtifactPayload::Original(product) => product.module_interface(),
            ArtifactPayload::Canonical(interface) => Some(interface),
            ArtifactPayload::Interface(..) => None,
        };
        let evidence = if let Some(interface) = canonical {
            let certificate = find(K::Certificate)?;
            let core = parts.iter().find(|p| p.kind == K::Core);
            world_array(vec![
                world_text(match interface.origin() {
                    crate::certified_products::CanonicalOrigin::SourceOriginal { .. } => "module",
                    crate::certified_products::CanonicalOrigin::NativeAuthoredDeclaration {
                        ..
                    } => "native-declaration",
                }),
                world_path(&certificate.path)?,
                world_text(certificate.sha256.clone()),
                core.map(|p| world_path(&p.path))
                    .transpose()?
                    .unwrap_or(Value::Null),
                core.map(|p| world_text(p.sha256.clone()))
                    .unwrap_or(Value::Null),
            ])
        } else {
            world_array(vec![world_text(match &entry.payload {
                ArtifactPayload::Interface(_, JoinedInterfaceRole::LexicalJoin) => "join",
                ArtifactPayload::Interface(_, JoinedInterfaceRole::ValueInterface) => "value",
                _ => {
                    return Err(source_failure(
                        "world original lacks its canonical type proof",
                    ))
                }
            })])
        };
        let owner = &entry.descriptor.owner;
        let interface_row = world_array(vec![
            world_text(owner.unit.clone()),
            world_text(owner.module.clone()),
            world_path(&iface.path)?,
            world_text(iface.sha256.clone()),
            world_array(
                entry
                    .requirements
                    .iter()
                    .map(|key| {
                        world_array(vec![
                            world_text(key.unit.clone()),
                            world_text(key.module.clone()),
                        ])
                    })
                    .collect(),
            ),
            world_path(&packages.path)?,
            world_text(packages.sha256.clone()),
            evidence,
        ]);
        for part in &parts {
            self.files
                .entry(part.transport.endpoint().to_owned())
                .or_insert_with(|| part.transport.compiler_file_lease());
        }
        let image = encode_image(
            &world_hex(&self.producer),
            &owner.unit,
            &owner.module,
            parts,
            None,
            &mut self.arenas,
        )?;
        let Value::Array(fields) = &image else {
            unreachable!()
        };
        let Value::Text(image_id) = &fields[3] else {
            unreachable!()
        };
        let image_id = image_id.clone();
        let key = (owner.unit.clone(), owner.module.clone(), image_id.clone());
        let mut digest = [0; 32];
        for (slot, pair) in digest.iter_mut().zip(image_id.as_bytes().chunks_exact(2)) {
            *slot = u8::from_str_radix(
                std::str::from_utf8(pair).map_err(|e| source_failure(e.to_string()))?,
                16,
            )
            .map_err(|e| source_failure(e.to_string()))?;
        }
        // Equal immutable type content shares the original receiving aliases.
        // Its independently issued historical runtime receipt remains in visibility.
        let row = self.receiving_aliases(key.clone(), interface_row)?;
        self.images.entry(key).or_insert(image);
        Ok((world_array(vec![row, world_blob(digest)]), digest))
    }
    fn receiving_aliases(
        &mut self,
        key: (String, String, String),
        incoming: ciborium::value::Value,
    ) -> Result<ciborium::value::Value, CompileError> {
        use std::collections::btree_map::Entry;
        match self.rows.entry(key) {
            Entry::Vacant(slot) => Ok(slot.insert(incoming).clone()),
            Entry::Occupied(slot) => {
                if world_interface_semantics(slot.get())? != world_interface_semantics(&incoming)? {
                    return Err(source_failure(
                        "equal world input image has conflicting role or required owners",
                    ));
                }
                Ok(slot.get().clone())
            }
        }
    }
}

/// Logical transfer facts only: paths and descriptor aliases do not identify a
/// row. The image binds all immutable part hashes; role and required owners are
/// separate semantic facts and must also survive transport unchanged.
fn world_interface_semantics(
    interface: &ciborium::value::Value,
) -> Result<(&str, std::collections::BTreeSet<(&str, &str)>), CompileError> {
    use ciborium::value::Value;
    let Value::Array(fields) = interface else {
        return Err(source_failure("invalid world interface row"));
    };
    let [_, _, _, _, Value::Array(requires), _, _, Value::Array(evidence)] = fields.as_slice()
    else {
        return Err(source_failure("invalid world interface row"));
    };
    let Some(Value::Text(role)) = evidence.first() else {
        return Err(source_failure("invalid world row role"));
    };
    if !matches!(
        role.as_str(),
        "module" | "native-declaration" | "join" | "value"
    ) {
        return Err(source_failure("invalid world row role"));
    }
    let mut owners = std::collections::BTreeSet::new();
    for owner in requires {
        let Value::Array(key) = owner else {
            return Err(source_failure("invalid world row requirement"));
        };
        let [Value::Text(unit), Value::Text(module)] = key.as_slice() else {
            return Err(source_failure("invalid world required owner"));
        };
        if !owners.insert((unit.as_str(), module.as_str())) {
            return Err(source_failure("duplicate world required owner"));
        }
    }
    Ok((role, owners))
}

fn world_semantic_row(
    row: &ciborium::value::Value,
    producer: &[u8; 32],
) -> Result<ciborium::value::Value, CompileError> {
    use ciborium::value::Value;
    let Value::Array(row) = row else {
        return Err(source_failure("invalid world row"));
    };
    let [interface @ Value::Array(fields), Value::Bytes(image)] = row.as_slice() else {
        return Err(source_failure("invalid world row image"));
    };
    let [Value::Text(unit), Value::Text(module), ..] = fields.as_slice() else {
        return Err(source_failure("invalid world interface row"));
    };
    if image.len() != 32 {
        return Err(source_failure("invalid world row image identity"));
    }
    let (role, owners) = world_interface_semantics(interface)?;
    Ok(world_array(vec![
        world_text(unit.clone()),
        world_text(module.clone()),
        world_blob(producer),
        world_blob(image),
        world_text(role),
        world_array(
            owners
                .into_iter()
                .map(|(unit, module)| world_array(vec![world_text(unit), world_text(module)]))
                .collect(),
        ),
    ]))
}

fn materialize_primary_association<BS: fvm_ipld_blockstore::Blockstore>(
    materializer: &mut Materializer<'_, BS>,
    interface: &ArtifactBinding,
    original: Option<&ArtifactBinding>,
    semantic: &ciborium::value::Value,
) -> Result<cid::Cid, CompileError> {
    materializer
        .materialize_value(&("TPCWPRIMARY1", interface, original, semantic))
        .map_err(|error| source_failure(error.to_string()))
}

fn materialize_primary_associations<BS: fvm_ipld_blockstore::Blockstore>(
    context: &ExactDeclarationContext,
    producer: &[u8; 32],
    directory: &Path,
    rows: &mut WorldColdRows,
    materializer: &mut Materializer<'_, BS>,
    remaining_raw: &mut u64,
) -> Result<
    (
        Vec<ciborium::value::Value>,
        MaterializedMap<ArtifactBinding, cid::Cid>,
    ),
    CompileError,
> {
    use crate::declaration_context::original_inputs::{OriginalInputKind as K, OriginalInputPart};
    let mut primary_facts = imbl::OrdMap::new();
    let mut base = Vec::new();
    for (ordinal, selected) in context
        .compiler_source_support()
        .world_primary_type_rows()
        .enumerate()
    {
        let entry = selected.entry;
        if selected.owner != &entry.descriptor.owner {
            return Err(source_failure("primary acquired row has another owner"));
        }
        crate::host_work::checkpoint()?;
        let estimate = primary_association_metadata_bound(
            &entry.descriptor.owner.unit,
            &entry.descriptor.owner.module,
            &entry.requirements,
        )?;
        *remaining_raw = remaining_raw
            .checked_sub(estimate)
            .ok_or_else(|| binding_capture_refusal(estimate, *remaining_raw))?;
        let root = directory.join(format!("primary-{ordinal}"));
        let mut builder =
            crate::owned_input_arena::OwnedInputArenaBuilder::new(capture_allowance()?)
                .map_err(|e| source_failure(e.to_string()))?;
        let mut pending = Vec::new();
        for (kind, bytes) in crate::declaration_context::original_inputs::entry_payloads(entry) {
            if matches!(kind, K::Native | K::Census | K::Graph) {
                continue;
            }
            if bytes.is_empty() || bytes.len() as u64 > kind.byte_limit() {
                return Err(source_failure("world type input exceeds its format bound"));
            }
            *remaining_raw = remaining_raw
                .checked_sub(bytes.len() as u64)
                .ok_or_else(source_size_failure)?;
            pending.push((
                kind,
                builder
                    .append(bytes)
                    .map_err(|e| source_failure(e.to_string()))?,
            ));
        }
        let arena = builder
            .finish()
            .map_err(|e| source_failure(e.to_string()))?;
        let parts = pending
            .into_iter()
            .map(|(kind, pending)| {
                let transport = arena
                    .issue_slice(pending)
                    .map_err(|e| source_failure(e.to_string()))?;
                Ok(OriginalInputPart {
                    kind,
                    path: root.join(kind.wire_tag()),
                    sha256: world_hex(transport.sha256()),
                    bytes: transport.len(),
                    transport,
                })
            })
            .collect::<Result<Vec<_>, CompileError>>()?;
        let row = rows.row(entry, parts)?.0;
        let semantic = world_semantic_row(&row, producer)?;
        let fact = materialize_primary_association(
            materializer,
            selected.interface_binding,
            selected.original_binding,
            &semantic,
        )?;
        if primary_facts
            .insert(*selected.transport_binding, fact)
            .is_some()
        {
            return Err(source_failure("duplicate primary transport binding"));
        }
        base.push(row);
    }
    let primary_associations = materializer
        .materialize_map(None, &primary_facts)
        .map_err(|error| source_failure(error.to_string()))?;
    Ok((base, primary_associations))
}

/// DATA from an already-issued context, through the production association
/// writer. The fixture packet owns copied arena bytes until its actual reader
/// completes. Its tagged file ranges are DATA, not a production acquisition:
/// the existing Haskell fixture owner must reseal them before the real reader.
/// These bytes issue no new compiler or runtime permission.
#[cfg(test)]
pub(crate) fn world_context_fixture(
    context: &ExactDeclarationContext,
    directory: &Path,
) -> Result<Vec<u8>, CompileError> {
    use ciborium::value::Value;
    use std::os::unix::fs::FileExt;
    let snapshot = context.snapshot();
    let producer = snapshot.producer_sha256();
    let store = fvm_ipld_blockstore::MemoryBlockstore::new();
    let mut materializer = Materializer::new(&store);
    let primary = materializer
        .materialize_context(None, &snapshot)
        .map_err(|error| source_failure(error.to_string()))?;
    let mut rows = WorldColdRows {
        producer,
        files: BTreeMap::new(),
        images: BTreeMap::new(),
        rows: BTreeMap::new(),
        arenas: Default::default(),
    };
    let mut remaining = capture_allowance()?;
    let (base, associations) = materialize_primary_associations(
        context,
        &producer,
        directory,
        &mut rows,
        &mut materializer,
        &mut remaining,
    )?;
    let mut acquisition = rows
        .arenas
        .acquisition(rows.images.into_values().collect())?;
    let Value::Array(fields) = &mut acquisition else {
        unreachable!()
    };
    fields[0] = world_text("TPCWORLDFIXTUREBYTES1");
    let Value::Array(arenas) = &mut fields[1] else {
        unreachable!()
    };
    for (index, descriptor) in arenas.iter_mut().enumerate() {
        let Value::Array(fields) = descriptor else {
            unreachable!()
        };
        let Value::Text(endpoint) = &fields[0] else {
            unreachable!()
        };
        let extent: u64 = fields[1]
            .as_integer()
            .and_then(|n| n.try_into().ok())
            .ok_or_else(|| source_failure("fixture arena extent"))?;
        let file = rows
            .files
            .get(Path::new(endpoint))
            .ok_or_else(|| source_failure("fixture arena owner"))?;
        let destination = directory.join(format!("world-arena-{index}"));
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)?;
        let mut offset = 0;
        let mut buffer = [0u8; 65536];
        while offset < extent {
            crate::host_work::checkpoint()?;
            let count = (extent - offset).min(buffer.len() as u64) as usize;
            file.read_exact_at(&mut buffer[..count], offset)?;
            std::io::Write::write_all(&mut output, &buffer[..count])?;
            offset += count as u64;
        }
        fields[0] = world_path(&destination)?;
    }
    let staged = materializer
        .finish()
        .map_err(|error| source_failure(error.to_string()))?;
    world_bytes(&world_array(vec![
        world_blob(primary.root().cid().to_bytes()),
        world_blob(associations.root().cid().to_bytes()),
        world_array(base),
        acquisition,
        world_array(
            staged
                .blocks()
                .iter()
                .map(|(cid, body)| world_array(vec![world_blob(cid.to_bytes()), world_blob(body)]))
                .collect(),
        ),
    ]))
}

impl CompilerWorldOpenInputs {
    fn materialize_primary(
        &self,
        endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
    ) -> Result<(Arc<ExactDeclarationContext>, ContextRoot, ReachableBlocks), CompileError> {
        let producer = CanonicalProducerIdentity::from_compiler(endpoint.identity()).sha256();
        let primary_context = Arc::new((*self.primary_context).clone().admit_producer(producer)?);
        let snapshot = primary_context.snapshot();
        let store = fvm_ipld_blockstore::MemoryBlockstore::new();
        let mut materializer = Materializer::new(&store);
        let context = materializer
            .materialize_context(None, &snapshot)
            .map_err(|error| source_failure(error.to_string()))?;
        let primary_root = context.root().clone();
        let staged = materializer
            .finish()
            .map_err(|error| source_failure(error.to_string()))?;
        let primary_blocks = ReachableBlocks::default()
            .replace_roots(&staged, vec![*primary_root.cid()])
            .map_err(|error| source_failure(error.to_string()))?;
        let work = primary_blocks.work();
        crate::host_work::record_host_work(
            crate::host_work::HostWorkCounter::SemanticDigestInputByte,
            work.verified_bytes,
        );
        crate::host_work::record_host_work(
            crate::host_work::HostWorkCounter::MaterializationGraphVisit,
            work.link_visits.saturating_add(work.unlink_visits),
        );
        Ok((primary_context, primary_root, primary_blocks))
    }

    fn prepare(
        mut self,
        endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
    ) -> Result<Arc<CompilerWorldRestoration>, CompileError> {
        let identity = endpoint.identity();
        let producer = CanonicalProducerIdentity::from_compiler(identity).sha256();
        let profile = *identity
            .package_source_association()
            .ok_or_else(|| {
                source_failure("compiler world requires a retained service package source")
            })?
            .profile_id();
        let (primary_context, primary_root, mut primary_blocks) =
            self.materialize_primary(endpoint)?;
        self.primary_context = primary_context;
        crate::host_work::record_host_work(
            crate::host_work::HostWorkCounter::InputFdEnumeration,
            1,
        );
        let mut rows = WorldColdRows {
            producer,
            files: BTreeMap::new(),
            images: BTreeMap::new(),
            rows: BTreeMap::new(),
            arenas: Default::default(),
            diagnostics: crate::artifacts::CompilerDiagnosticCapture::input_artifacts(
                self.directory.path(),
            ),
        };
        let block_bytes = primary_blocks
            .blocks()
            .try_fold(0u64, |total, (cid, body)| {
                total
                    .checked_add(body.len() as u64)
                    .and_then(|n| n.checked_add(cid.to_bytes().len() as u64 + 32))
                    .ok_or_else(source_size_failure)
            })?;
        let envelope_bytes = block_bytes
            .checked_add(self.sources.encoded().len() as u64)
            .and_then(|n| n.checked_add(self.bindings.metadata_bytes))
            .ok_or_else(source_size_failure)?;
        let mut remaining_raw = capture_allowance()?
            .checked_sub(envelope_bytes)
            .ok_or_else(source_size_failure)?;
        let mut binding_materializer = Materializer::new(&primary_blocks);
        let (base, primary_associations) = materialize_primary_associations(
            &self.primary_context,
            &producer,
            self.directory.path(),
            &mut rows,
            &mut binding_materializer,
            &mut remaining_raw,
        )?;
        let mut binding_rows = BTreeMap::new();
        let mut visible = Vec::with_capacity(self.visible_rows.len());
        for artifact in &self.visible_rows {
            crate::host_work::checkpoint()?;
            let (entry, parts) = artifact.world_type_parts()?;
            remaining_raw = remaining_raw
                .checked_sub(binding_row_metadata_bound(
                    &entry.descriptor.owner.unit,
                    &entry.descriptor.owner.module,
                    &entry.requirements,
                )?)
                .ok_or_else(source_size_failure)?;
            for part in &parts {
                remaining_raw = remaining_raw
                    .checked_sub(part.bytes)
                    .ok_or_else(source_size_failure)?;
            }
            let (row, _) = rows.row(&entry, parts)?;
            let semantic = world_semantic_row(&row, &producer)?;
            let (_, receipt) = artifact.world_issuer();
            let row_fact = world_array(vec![
                semantic.clone(),
                world_blob(artifact.certified_interface().artifact_id().0),
                world_blob(receipt),
            ]);
            let row_root = binding_materializer
                .materialize_value(&row_fact)
                .map_err(|error| source_failure(error.to_string()))?;
            binding_rows.insert(artifact.owner().module_name(), row_root);
            visible.push(row);
        }
        if let Some(capture) = &rows.diagnostics {
            if let Err(error) = capture.finish_original_parts() {
                tracing::warn!(%error,"compiler world input diagnostic manifest is incomplete");
            }
        }
        let binding_facts = self.bindings.cold_snapshot(&binding_rows)?;
        let bindings = binding_materializer
            .materialize_map(None, &binding_facts)
            .map_err(|error| source_failure(error.to_string()))?;
        let staged = binding_materializer
            .finish()
            .map_err(|error| source_failure(error.to_string()))?;
        primary_blocks = primary_blocks
            .replace_roots(
                &staged,
                vec![
                    *primary_root.cid(),
                    *primary_associations.root().cid(),
                    *bindings.root().cid(),
                ],
            )
            .map_err(|error| source_failure(error.to_string()))?;
        let work = primary_blocks.work();
        crate::host_work::record_host_work(
            crate::host_work::HostWorkCounter::SemanticDigestInputByte,
            work.verified_bytes,
        );
        crate::host_work::record_host_work(
            crate::host_work::HostWorkCounter::MaterializationGraphVisit,
            work.link_visits.saturating_add(work.unlink_visits),
        );
        let visibility = world_bytes(&world_array(vec![
            world_text("TPCWBIND4"),
            self.bindings.query_scope.into(),
            world_blob(bindings.root().cid().to_bytes()),
        ]))?;
        let blocks = primary_blocks
            .blocks()
            .map(|(cid, body)| world_array(vec![world_blob(cid.to_bytes()), world_blob(body)]))
            .collect();
        // CID-validated roots bind logical facts. Cold transfer order and raw
        // image inventory are not part of a live successor's fixed identity.
        let product = world_bytes(&world_array(vec![
            world_text("TPCWREST"),
            3u64.into(),
            world_blob(primary_root.cid().to_bytes()),
            world_blob(self.sources.seal()),
            world_blob(producer),
            world_blob(profile),
            self.bindings.query_scope.into(),
            world_blob(primary_associations.root().cid().to_bytes()),
            world_blob(bindings.root().cid().to_bytes()),
        ]))?;
        let logical_id = world_digest(&product);
        let source_path = self.directory.path().join("sources.bin");
        std::fs::write(&source_path, self.sources.encoded())?;
        let files = rows.files.into_values().collect();
        let acquisition = rows
            .arenas
            .acquisition(rows.images.into_values().collect())?;
        let descriptor = self.directory.path().join("world.cbor");
        let encoded = world_bytes(&world_array(vec![
            world_text("TPCWORLD"),
            3u64.into(),
            world_blob(&product),
            world_path(&source_path)?,
            world_blob(producer),
            world_blob(profile),
            world_array(vec![world_array(base), world_array(visible)]),
            acquisition,
            world_blob(&visibility),
            world_blob(primary_root.cid().to_bytes()),
            world_array(blocks),
        ]))?;
        std::fs::write(&descriptor, encoded)?;
        Ok(Arc::new(CompilerWorldRestoration {
            logical_id,
            inputs: self,
            product: product.into(),
            primary_root,
            primary_blocks,
            primary_associations,
            bindings,
            producer,
            profile,
            descriptor,
            input_files: files,
        }))
    }
}

fn scoped_world_action<T>(
    action: impl FnOnce() -> Result<T, CompileError>,
) -> Result<T, CompileError> {
    if tidepool_extract_cmd::CompilerTransactionOrigin::current().is_none() {
        return Err(source_failure(
            "compiler world requires the runtime source compiler transaction",
        ));
    }
    action()
}

fn world_command(
    identity: &tidepool_extract_cmd::CompilerIdentity,
) -> Result<
    (
        tidepool_extract_cmd::ExtractCmd,
        tidepool_extract_cmd::CompilerEndpoint,
    ),
    CompileError,
> {
    crate::host_work::checkpoint()?;
    let cmd = tidepool_extract_cmd::ExtractCmd::new().map_err(|e| source_failure(e.to_string()))?;
    let endpoint = cmd.bind().map_err(CompileError::CompilerEndpoint)?;
    if endpoint.identity() != identity {
        return Err(source_failure(
            "compiler world endpoint changed before submission",
        ));
    }
    Ok((cmd, endpoint))
}

fn world_execute(
    cmd: &tidepool_extract_cmd::ExtractCmd,
    endpoint: tidepool_extract_cmd::CompilerEndpoint,
    files: Vec<Arc<std::fs::File>>,
) -> Result<tidepool_extract_cmd::ExtractRun, CompileError> {
    CompileError::compiler_invocation_result(endpoint.execute_with_input_files(cmd, files, |_| {}))
}

fn ensure_world(
    world: &CompilerWorld,
    identity: &tidepool_extract_cmd::CompilerIdentity,
) -> Result<String, CompileError> {
    use tidepool_extract_cmd::{
        EnsureExactInputsOutcome as O, ExactInputsAcquisition as A, ExactInputsRequest,
    };
    let restoration = &world.restoration;
    if CanonicalProducerIdentity::from_compiler(identity).sha256() != restoration.producer
        || identity
            .package_source_association()
            .map(|s| s.profile_id())
            != Some(&restoration.profile)
    {
        return Err(source_failure(
            "compiler world producer/package profile differs",
        ));
    }
    let root = world_hex(world.logical_id());
    let hint = world
        .realization
        .lock()
        .map_err(|_| source_failure("world realization lock poisoned"))?
        .clone()
        .unwrap_or_else(|| "not-resident".into());
    // This source-free probe precedes baseline descriptor exports or FD visits.
    let (mut cmd, endpoint) = world_command(identity)?;
    cmd.ensure_exact_inputs(ExactInputsRequest {
        root: root.clone(),
        acquisition: A::Resident(hint),
    });
    crate::host_work::record_host_work(
        crate::host_work::HostWorkCounter::WorldEnsureResidentProbe,
        1,
    );
    let run = world_execute(&cmd, endpoint, Vec::new())?;
    let response = O::decode(&run.output.stdout).map_err(|_| world_response_error(false))?;
    let realized = match response {
        O::Ready { world, realization } if world == root => {
            crate::host_work::record_host_work(
                crate::host_work::HostWorkCounter::WorldEnsureResidentHit,
                1,
            );
            realization
        }
        O::NeedsAcquisition(world) if world == root => {
            crate::host_work::record_host_work(
                crate::host_work::HostWorkCounter::WorldEnsureNeedsAcquisition,
                1,
            );
            if !tidepool_extract_cmd::retain_compiler_input_directory(Arc::clone(
                &restoration.inputs.directory,
            )) {
                return Err(source_failure(
                    "world acquisition requires its compiler scope",
                ));
            }
            let (mut cmd, endpoint) = world_command(identity)?;
            cmd.ensure_exact_inputs(ExactInputsRequest {
                root: root.clone(),
                acquisition: A::Complete(restoration.descriptor.clone()),
            });
            crate::host_work::record_host_work(
                crate::host_work::HostWorkCounter::InputFdExport,
                restoration.input_files.len() as u64,
            );
            crate::host_work::record_host_work(
                crate::host_work::HostWorkCounter::WorldEnsureComplete,
                1,
            );
            let diagnostics = crate::artifacts::CompilerDiagnosticCapture::start(
                restoration.inputs.directory.path(),
                &cmd,
            );
            let run = world_execute(&cmd, endpoint, restoration.input_files.clone())?;
            diagnostics.completed(
                restoration.inputs.directory.path(),
                &cmd,
                run.success(),
                &run.output.stderr,
            );

            match O::decode(&run.output.stdout).map_err(|_| world_response_error(false))? {
                O::Ready { world, realization } if world == root => {
                    crate::host_work::record_host_work(
                        crate::host_work::HostWorkCounter::WorldEnsureCompleteReady,
                        1,
                    );
                    realization
                }
                O::Rejected(reason) => {
                    return Err(source_failure(format!(
                        "compiler refused world acquisition: {reason:?}"
                    )))
                }
                _ => return Err(world_response_error(true)),
            }
        }
        O::Rejected(reason) => {
            return Err(source_failure(format!(
                "compiler refused world realization: {reason:?}"
            )))
        }
        _ => return Err(world_response_error(true)),
    };
    *world
        .realization
        .lock()
        .map_err(|_| source_failure("world realization lock poisoned"))? = Some(realized.clone());
    Ok(realized)
}

/// Acquire the exact published logical world independently of its next demand.
/// Runtime source admission owns the current physical transaction and its END.
pub fn open_world(
    cold: CompilerWorldOpenInputs,
    endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
) -> Result<Arc<CompilerWorld>, CompileError> {
    scoped_world_action(|| {
        let restoration = cold.prepare(endpoint)?;
        let world = Arc::new(CompilerWorld {
            restoration,
            realization: Arc::new(Mutex::new(None)),
        });
        ensure_world(&world, endpoint.identity())?;
        Ok(world)
    })
}

/// Observe the held logical world with a fresh operation authority. Residency
/// loss restores its retained product; accepted/uncertain queries are not replayed.
pub fn query_public_module(
    endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
    world: &CompilerWorld,
    question: PublicModuleQuestion,
) -> Result<WorldQueryResult, CompileError> {
    scoped_world_action(|| {
        query_world_inner(endpoint.identity(), Arc::new(world.clone()), question)
    })
}

/// Parse current authored input in the logical world. Source authority and
/// physical END remain with the caller's runtime source compiler transaction.
pub fn plan_cell(
    endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
    world: &CompilerWorld,
    recipe: Arc<tidepool_extract_cmd::WorldCellRecipe>,
) -> Result<Arc<crate::cell_plan::CompilerWorldCellPlan>, CompileError> {
    scoped_world_action(|| {
        let directory = Arc::new(crate::artifacts::compiler_scratch_directory()?);
        let output = directory.path().join("cell-plan.cbor");
        if !tidepool_extract_cmd::retain_compiler_input_directory(Arc::clone(&directory)) {
            return Err(source_failure(
                "world cell planning requires its compiler scope",
            ));
        }
        let mut request = tidepool_extract_cmd::WorldCellPlanRequest {
            world: world_hex(world.logical_id()),
            realization: "not-resident".into(),
            recipe: Arc::clone(&recipe),
            output: output.clone(),
        };
        request
            .validate_transport()
            .map_err(|error| source_failure(error.to_string()))?;
        request.realization = ensure_world(world, endpoint.identity())?;
        let (mut command, bound) = world_command(endpoint.identity())?;
        command
            .world_cell_plan(request)
            .map_err(|error| source_failure(error.to_string()))?;
        let diagnostics =
            crate::artifacts::CompilerDiagnosticCapture::start(directory.path(), &command);
        let result = world_execute(&command, bound, Vec::new())?;
        diagnostics.completed(
            directory.path(),
            &command,
            result.success(),
            &result.output.stderr,
        );
        crate::diag::decode_extract_result(
            result.success(),
            &result.output.stdout,
            &result.output.stderr,
        )?;
        let receipt = crate::checked_cell::read(output, 8 << 20)?;
        crate::cell_plan::admit_world_plan(
            Arc::new(world.clone()),
            recipe,
            CanonicalProducerIdentity::from_compiler(endpoint.identity()).sha256(),
            receipt,
        )
    })
}

fn query_world_inner(
    identity: &tidepool_extract_cmd::CompilerIdentity,
    world: Arc<CompilerWorld>,
    question: PublicModuleQuestion,
) -> Result<WorldQueryResult, CompileError> {
    let realization = ensure_world(&world, identity)?;
    let output_owner = Arc::new(
        tempfile::Builder::new()
            .prefix("tidepool-world-answer-")
            .tempdir()?,
    );
    let output = output_owner.path().join("answer.cbor");
    if !tidepool_extract_cmd::retain_compiler_input_directory(Arc::clone(&output_owner)) {
        return Err(source_failure("world question requires its compiler scope"));
    }
    let (mut cmd, endpoint) = world_command(identity)?;
    cmd.world_public_module(tidepool_extract_cmd::WorldPublicModuleRequest {
        world: world_hex(world.logical_id()),
        realization,
        mode: match question.kind {
            PublicModuleQuestionKind::StructuredInfo => tidepool_extract_cmd::WorldQueryMode::Info,
            PublicModuleQuestionKind::StructuredType => tidepool_extract_cmd::WorldQueryMode::Type,
        },
        query: question.query.clone(),
        output: output.clone(),
    });
    let run = world_execute(&cmd, endpoint, Vec::new())?;
    crate::diag::decode_extract_result(run.success(), &run.output.stdout, &run.output.stderr)?;
    let file = std::fs::File::open(&output).map_err(|_| world_response_error(false))?;
    let allowance = capture_allowance()?;
    if file.metadata()?.len() > allowance {
        return Err(world_response_error(false));
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    crate::host_work::read_to_end(&mut file.take(allowance.saturating_add(1)), &mut bytes)?;
    if bytes.len() as u64 > allowance {
        return Err(world_response_error(false));
    }
    decode_world_answer(world, question, &bytes)
}

fn world_response_error(provenance: bool) -> CompileError {
    use tidepool_extract_cmd::CompilerResponseRejection as R;
    CompileError::CompilerEndpoint(tidepool_extract_cmd::reject_compiler_response(
        if provenance {
            R::ProvenanceMismatch
        } else {
            R::Malformed
        },
    ))
}

fn answer_array(
    value: &ciborium::value::Value,
    count: usize,
) -> Result<&[ciborium::value::Value], CompileError> {
    match value {
        ciborium::value::Value::Array(v) if v.len() == count => Ok(v),
        _ => Err(world_response_error(false)),
    }
}
fn answer_text(value: &ciborium::value::Value) -> Result<&str, CompileError> {
    match value {
        ciborium::value::Value::Text(v) => Ok(v),
        _ => Err(world_response_error(false)),
    }
}
fn answer_u64(value: &ciborium::value::Value) -> Result<u64, CompileError> {
    match value {
        ciborium::value::Value::Integer(v) => {
            u64::try_from(*v).map_err(|_| world_response_error(false))
        }
        _ => Err(world_response_error(false)),
    }
}
fn answer_blob(value: &ciborium::value::Value) -> Result<&[u8], CompileError> {
    match value {
        ciborium::value::Value::Bytes(v) => Ok(v),
        _ => Err(world_response_error(false)),
    }
}
fn answer_hash(value: &ciborium::value::Value) -> Result<[u8; 32], CompileError> {
    answer_blob(value)?
        .try_into()
        .map_err(|_| world_response_error(false))
}
fn answer_cbor(bytes: &[u8]) -> Result<ciborium::value::Value, CompileError> {
    let mut input = std::io::Cursor::new(bytes);
    let value = ciborium::de::from_reader(&mut input).map_err(|_| world_response_error(false))?;
    if input.position() != bytes.len() as u64 {
        return Err(world_response_error(false));
    }
    Ok(value)
}
fn namespace_tag(namespace: InspectionNamespace) -> u64 {
    match namespace {
        InspectionNamespace::Any => 0,
        InspectionNamespace::Value => 1,
        InspectionNamespace::Type => 2,
        InspectionNamespace::Constructor => 3,
        InspectionNamespace::Field => 4,
    }
}
fn namespace_text(namespace: InspectionNamespace) -> &'static str {
    match namespace {
        InspectionNamespace::Any => "Any",
        InspectionNamespace::Value => "Value",
        InspectionNamespace::Type => "Type",
        InspectionNamespace::Constructor => "Constructor",
        InspectionNamespace::Field => "Field",
    }
}
#[derive(Clone, Copy)]
enum ModuleReceiptUse {
    Requested,
    Defining,
}

fn module_receipt(
    value: &ciborium::value::Value,
    sources: &CapturedCompilerSources,
    expected_profile: &[u8; 32],
    receipt_use: ModuleReceiptUse,
) -> Result<WorldModuleReceipt, CompileError> {
    let v = answer_array(value, 4)?;
    if answer_text(&v[0])? != "resolved" {
        return Err(world_response_error(false));
    }
    let unit = answer_text(&v[1])?.to_owned();
    let module = answer_text(&v[2])?.to_owned();
    if unit.is_empty() || module.is_empty() || unit.contains('\0') || module.contains('\0') {
        return Err(world_response_error(true));
    }
    let origin = match &v[3] {
        ciborium::value::Value::Array(o) if o.len() == 2 && answer_text(&o[0])? == "package" => {
            let profile = answer_hash(&o[1])?;
            if &profile != expected_profile
                || (matches!(receipt_use, ModuleReceiptUse::Requested)
                    && sources.selected_sources.contains_key(&module))
            {
                return Err(world_response_error(true));
            }
            WorldModuleOrigin::Package { profile }
        }
        ciborium::value::Value::Array(o) if o.len() == 4 && answer_text(&o[0])? == "home" => {
            let root =
                u32::try_from(answer_u64(&o[1])?).map_err(|_| world_response_error(false))?;
            let path = answer_text(&o[2])?.to_owned();
            if relative_source_path(Path::new(&path)).is_err() {
                return Err(world_response_error(true));
            }
            let source_sha256 = answer_hash(&o[3])?;
            let selected = WorldModuleOrigin::Home {
                root,
                path,
                source_sha256,
            };
            if sources.selected_sources.get(&module) != Some(&selected) {
                return Err(world_response_error(true));
            }
            selected
        }
        _ => return Err(world_response_error(false)),
    };
    Ok(WorldModuleReceipt {
        unit,
        module,
        origin,
    })
}

fn validate_identifier_namespace(
    requested: InspectionNamespace,
    actual: &str,
) -> Result<(), CompileError> {
    let actual = match actual {
        "Value" => InspectionNamespace::Value,
        "Type" => InspectionNamespace::Type,
        "Constructor" => InspectionNamespace::Constructor,
        "Field" => InspectionNamespace::Field,
        _ => return Err(world_response_error(false)),
    };
    // GHC's value-name domain includes record selectors; the inspected
    // identifier reports their more precise Field namespace.
    if requested == InspectionNamespace::Any
        || requested == actual
        || (requested == InspectionNamespace::Value && actual == InspectionNamespace::Field)
    {
        Ok(())
    } else {
        Err(world_response_error(true))
    }
}

fn validate_inspection(
    payload: &[u8],
    question: &PublicModuleQuestion,
    resolution: &WorldModuleResolution,
    defining: Option<&WorldModuleReceipt>,
) -> Result<(), CompileError> {
    let value = answer_cbor(payload)?;
    let top = answer_array(&value, 2)?;
    if answer_text(&top[0])? != "TPINSP008" {
        return Err(world_response_error(false));
    }
    let complete = answer_array(&top[1], 2)?;
    if answer_text(&complete[0])? != "Complete" {
        return Err(world_response_error(false));
    }
    let only = answer_array(&complete[1], 1)?;
    let answer = answer_array(&only[0], 2)?;
    match answer_text(&answer[0])? {
        "StructuredInfoOk" | "StructuredTypeOk" => {
            let expected = match question.kind {
                PublicModuleQuestionKind::StructuredInfo => "StructuredInfoOk",
                PublicModuleQuestionKind::StructuredType => "StructuredTypeOk",
            };
            if answer_text(&answer[0])? != expected
                || !matches!(resolution, WorldModuleResolution::Resolved(_))
            {
                return Err(world_response_error(true));
            }
            let details = answer_array(
                &answer[1],
                if expected == "StructuredInfoOk" { 4 } else { 3 },
            )?;
            let identifier = answer_array(&details[0], 3)?;
            let definition = defining.ok_or_else(|| world_response_error(true))?;
            let name = answer_text(&identifier[1])?;
            validate_identifier_namespace(question.namespace(), answer_text(&identifier[2])?)?;
            if answer_text(&identifier[0])? != definition.module
                || !(question.name() == name
                    || question.name() == format!("{}.{}", definition.module, name))
            {
                return Err(world_response_error(true));
            }
            let provenance = answer_array(
                details.last().ok_or_else(|| world_response_error(false))?,
                3,
            )?;
            let scope = answer_array(&provenance[0], 2)?;
            if answer_text(&scope[0])? != "PublicModule"
                || answer_text(&scope[1])? != question.module()
                || answer_u64(&provenance[1])? != question.generation()
                || answer_text(&provenance[2])? != question.fingerprint()
            {
                return Err(world_response_error(true));
            }
        }
        "StructuredError" => {
            if defining.is_some() {
                return Err(world_response_error(true));
            }
            let ciborium::value::Value::Array(error) = &answer[1] else {
                return Err(world_response_error(false));
            };
            let tag = answer_text(error.first().ok_or_else(|| world_response_error(false))?)?;
            match tag {
                "UnknownModule" if error.len() == 2 => {
                    if !matches!(resolution, WorldModuleResolution::Absent(_))
                        || answer_text(&error[1])? != question.module()
                    {
                        return Err(world_response_error(true));
                    }
                }
                "Unknown" | "Ambiguous" if error.len() == if tag == "Unknown" { 2 } else { 3 } => {
                    if !matches!(resolution, WorldModuleResolution::Resolved(_)) {
                        return Err(world_response_error(true));
                    }
                    let query = answer_array(&error[1], 3)?;
                    let scope = answer_array(&query[0], 2)?;
                    if answer_text(&scope[0])? != "PublicModule"
                        || answer_text(&scope[1])? != question.module()
                        || answer_text(&query[1])? != namespace_text(question.namespace())
                        || answer_text(&query[2])? != question.name()
                    {
                        return Err(world_response_error(true));
                    }
                }
                "Unsupported" if error.len() == 2 => {
                    let _ = answer_text(&error[1])?;
                }
                _ => return Err(world_response_error(false)),
            }
        }
        _ => return Err(world_response_error(false)),
    }
    Ok(())
}

fn decode_world_answer(
    world: Arc<CompilerWorld>,
    question: PublicModuleQuestion,
    bytes: &[u8],
) -> Result<WorldQueryResult, CompileError> {
    if bytes.get(..8) != Some(b"TPWQ0002") || bytes.len() < 12 {
        return Err(world_response_error(false));
    }
    let size = u32::from_le_bytes(
        bytes[8..12]
            .try_into()
            .map_err(|_| world_response_error(false))?,
    ) as usize;
    if size != bytes.len() - 12 {
        return Err(world_response_error(false));
    }
    let value = answer_cbor(&bytes[12..])?;
    let v = answer_array(&value, 8)?;
    let expected_mode = if question.kind == PublicModuleQuestionKind::StructuredInfo {
        0
    } else {
        1
    };
    let echoed = answer_array(&v[4], 4)?;
    if answer_u64(&v[0])? != 1
        || answer_hash(&v[1])? != *world.logical_id()
        || answer_u64(&v[2])? != expected_mode
        || answer_text(&v[3])? != question.module()
        || answer_u64(&echoed[0])? != namespace_tag(question.namespace())
        || answer_text(&echoed[1])? != question.name()
        || answer_u64(&echoed[2])? != question.generation()
        || answer_text(&echoed[3])? != question.fingerprint()
    {
        return Err(world_response_error(true));
    }
    let ciborium::value::Value::Array(resolution) = &v[5] else {
        return Err(world_response_error(false));
    };
    let tag = answer_text(
        resolution
            .first()
            .ok_or_else(|| world_response_error(false))?,
    )?;
    let resolution = match tag {
        "resolved" => {
            let receipt = module_receipt(
                &v[5],
                &world.restoration.inputs.sources,
                &world.restoration.profile,
                ModuleReceiptUse::Requested,
            )?;
            if receipt.module != question.module() {
                return Err(world_response_error(true));
            }
            WorldModuleResolution::Resolved(receipt)
        }
        "absent" if resolution.len() == 4 => {
            let module = answer_text(&resolution[1])?.to_owned();
            let source_domain = answer_hash(&resolution[2])?;
            let package_profile = answer_hash(&resolution[3])?;
            if module != question.module()
                || world
                    .restoration
                    .inputs
                    .sources
                    .selected_sources
                    .contains_key(&module)
                || source_domain != *world.restoration.inputs.sources.seal()
                || package_profile != world.restoration.profile
            {
                return Err(world_response_error(true));
            }
            WorldModuleResolution::Absent(ClosedModuleAbsence {
                module,
                source_domain,
                package_profile,
            })
        }
        "rejected" if resolution.len() == 2 => {
            WorldModuleResolution::Rejected(match answer_u64(&resolution[1])? {
                0 => WorldQueryRejection::Unsupported,
                1 => WorldQueryRejection::Unavailable,
                2 => WorldQueryRejection::Corrupt,
                _ => return Err(world_response_error(false)),
            })
        }
        _ => return Err(world_response_error(false)),
    };
    let defining = if matches!(v[6], ciborium::value::Value::Null) {
        None
    } else {
        Some(module_receipt(
            &v[6],
            &world.restoration.inputs.sources,
            &world.restoration.profile,
            ModuleReceiptUse::Defining,
        )?)
    };
    let inspection_payload = answer_blob(&v[7])?.to_vec();
    if matches!(resolution, WorldModuleResolution::Rejected(_)) {
        if defining.is_some() || !inspection_payload.is_empty() {
            return Err(world_response_error(false));
        }
    } else {
        validate_inspection(
            &inspection_payload,
            &question,
            &resolution,
            defining.as_ref(),
        )?;
    }
    Ok(WorldQueryResult {
        world,
        question,
        resolution,
        defining,
        inspection_payload,
    })
}

/// Re-establish an issued logical world from its retained restoration owner.
/// This rehydrates compiler facts; it never runs notebook cells or effects.
pub fn restore_world(
    restoration: &Arc<CompilerWorldRestoration>,
    endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
    question: PublicModuleQuestion,
) -> Result<WorldQueryResult, CompileError> {
    let world = CompilerWorld {
        restoration: Arc::clone(restoration),
        realization: Arc::new(Mutex::new(None)),
    };
    query_public_module(endpoint, &world, question)
}

#[cfg(test)]
mod answer_tests {
    use super::*;
    fn sources(first: &[u8], second: &[u8]) -> CapturedCompilerSources {
        let root = |body: &[u8]| CapturedCompilerSourceRoot {
            manifest: SourceRootManifest::from_file_digests([(
                PathBuf::from("Module.hs"),
                blake3::hash(body).to_hex().to_string(),
            )])
            .unwrap(),
            bodies: BTreeMap::from([(PathBuf::from("Module.hs"), Arc::<[u8]>::from(body))]),
        };
        CapturedCompilerSources::from_captured_roots(
            vec![root(first), root(second)],
            Arc::from(&b"retained-generation"[..]),
            Arc::new(()),
        )
        .unwrap()
    }
    fn home(root: u64, body: &[u8]) -> ciborium::value::Value {
        world_array(vec![
            world_text("resolved"),
            world_text("main"),
            world_text("Module"),
            world_array(vec![
                world_text("home"),
                root.into(),
                world_text("Module.hs"),
                world_blob(world_digest(body)),
            ]),
        ])
    }
    #[test]
    fn world_receipt_refuses_later_source_even_with_its_correct_hash() {
        let a = b"module Module where\nvalue = (1 :: Int)\n";
        let b = b"module Module where\nvalue = True\n";
        let ab = sources(a, b);
        assert!(module_receipt(&home(0, a), &ab, &[7; 32], ModuleReceiptUse::Requested).is_ok());
        assert!(module_receipt(&home(1, b), &ab, &[7; 32], ModuleReceiptUse::Requested).is_err());
        let ba = sources(b, a);
        assert!(module_receipt(&home(0, b), &ba, &[7; 32], ModuleReceiptUse::Requested).is_ok());
        assert!(module_receipt(&home(1, a), &ba, &[7; 32], ModuleReceiptUse::Requested).is_err());
        assert_ne!(ab.seal(), ba.seal());
        let package = world_array(vec![
            world_text("resolved"),
            world_text("base"),
            world_text("Module"),
            world_array(vec![world_text("package"), world_blob([7; 32])]),
        ]);
        assert!(module_receipt(&package, &ab, &[7; 32], ModuleReceiptUse::Requested).is_err());
        // A package-qualified reexport may define a name in a different unit
        // despite a same-named home module in the captured search namespace.
        assert!(module_receipt(&package, &ab, &[7; 32], ModuleReceiptUse::Defining).is_ok());
        assert!(module_receipt(&package, &ab, &[8; 32], ModuleReceiptUse::Defining).is_err());
    }
    #[test]
    fn cold_world_row_seal_binds_roles_requirements_order_and_ignores_aliases() {
        let row = |name: &str, alias: &str, role: &str, required: &str| {
            world_array(vec![
                world_array(vec![
                    world_text("main"),
                    world_text(name),
                    world_text(alias),
                    world_text("iface-hash"),
                    world_array(vec![world_array(vec![
                        world_text("main"),
                        world_text(required),
                    ])]),
                    world_text("/package-alias"),
                    world_text("packages-hash"),
                    world_array(vec![world_text(role)]),
                ]),
                world_blob([9; 32]),
            ])
        };
        let seal = |rows: Vec<ciborium::value::Value>| {
            world_digest(
                &world_bytes(&world_array(
                    rows.iter()
                        .map(|r| world_semantic_row(r, &[3; 32]).unwrap())
                        .collect(),
                ))
                .unwrap(),
            )
        };
        let original = row("A", "/original", "module", "Dependency");
        assert_eq!(
            seal(vec![original.clone()]),
            seal(vec![row("A", "/relocated", "module", "Dependency")])
        );
        assert_ne!(
            seal(vec![original.clone()]),
            seal(vec![row("A", "/original", "join", "Dependency")])
        );
        assert_ne!(
            seal(vec![original.clone()]),
            seal(vec![row("A", "/original", "module", "Other")])
        );
        assert_ne!(seal(vec![original.clone()]), seal(vec![]));
        let second = row("B", "/second", "module", "Dependency");
        assert_ne!(
            seal(vec![original.clone(), second.clone()]),
            seal(vec![second, original])
        );
    }
    #[test]
    fn equal_world_image_reuses_aliases_only_with_equal_semantics() {
        let interface = |role: &str, required: &str, path: &str| {
            world_array(vec![
                world_text("main"),
                world_text("Module"),
                world_text(path),
                world_text("ifacehash"),
                world_array(vec![world_array(vec![
                    world_text("main"),
                    world_text(required),
                ])]),
                world_text("packages"),
                world_text("packagehash"),
                world_array(vec![world_text(role)]),
            ])
        };
        let mut rows = WorldColdRows {
            producer: [0; 32],
            files: BTreeMap::new(),
            images: BTreeMap::new(),
            rows: BTreeMap::new(),
            arenas: Default::default(),
            diagnostics: None,
        };
        let key = ("main".into(), "Module".into(), "image".into());
        let first = rows
            .receiving_aliases(key.clone(), interface("value", "Dependency", "/first"))
            .unwrap();
        assert_eq!(
            rows.receiving_aliases(key.clone(), interface("value", "Dependency", "/alias"))
                .unwrap(),
            first
        );
        assert!(rows
            .receiving_aliases(key.clone(), interface("join", "Dependency", "/first"))
            .is_err());
        assert!(rows
            .receiving_aliases(key, interface("value", "Other", "/first"))
            .is_err());
        assert_eq!(rows.rows.len(), 1);
    }

    #[test]
    fn cold_primary_binds_actual_admitted_producer_before_root_sealing() {
        use fvm_ipld_blockstore::Blockstore;
        let configured = crate::toolchain::CompilerDeploymentConfiguration::from_env()
            .expect("declared compiler deployment");
        let (endpoint, _, _) = crate::toolchain::bind_admitted_extract_endpoint(&configured)
            .expect("actual configured compiler endpoint");
        let endpoint = crate::toolchain::AdmittedCompilerEndpoint::from_bound(endpoint)
            .expect("actual admitted endpoint");
        let canonical = CanonicalProducerIdentity::from_compiler(endpoint.identity()).sha256();
        let raw = *endpoint.identity().producer_bytes();
        assert_ne!(canonical, raw);
        let sources = Arc::new(sources(b"module Module where\n", b"module Module where\n"));
        let cold = crate::artifacts::ModuleCandidateOffer::compiler_world_inputs(
            None,
            CompilerWorldBindings::from_published_bindings(0, std::iter::empty()).unwrap(),
            Arc::clone(&sources),
        )
        .unwrap();
        assert_eq!(cold.primary_context.snapshot().producer_sha256(), [0; 32]);
        let (bound, root, blocks) = cold.materialize_primary(&endpoint).unwrap();
        assert_eq!(bound.snapshot().producer_sha256(), canonical);
        // The original published owner remains unchanged. Inspect the actual
        // production materializer's root through its independent reader.
        assert_eq!(cold.primary_context.snapshot().producer_sha256(), [0; 32]);
        let store = fvm_ipld_blockstore::MemoryBlockstore::new();
        for (cid, body) in blocks.blocks() {
            store.put_keyed(cid, body).unwrap();
        }
        let restored = crate::exact_context::hydrate_context(&store, &root).unwrap();
        assert_eq!(restored.snapshot().producer_sha256(), canonical);
        let issued = crate::artifacts::ModuleCandidateOffer::compiler_world_inputs(
            Some(bound),
            CompilerWorldBindings::from_published_bindings(0, std::iter::empty()).unwrap(),
            Arc::clone(&sources),
        )
        .unwrap();
        assert_eq!(issued.materialize_primary(&endpoint).unwrap().1, root);
        let wrong = crate::artifacts::ModuleCandidateOffer::compiler_world_inputs(
            Some(Arc::new(
                (*cold.primary_context).clone().admit_producer(raw).unwrap(),
            )),
            CompilerWorldBindings::from_published_bindings(0, std::iter::empty()).unwrap(),
            sources,
        )
        .unwrap();
        assert!(wrong.materialize_primary(&endpoint).is_err());
    }

    #[test]
    fn cold_visibility_rejects_oversize_before_clone_or_allocation() {
        assert!(binding_metadata_bound([usize::MAX; 5], 0).is_err());
        assert!(binding_metadata_bound([0; 5], usize::MAX).is_err());
        // Data-only values through the actual Materializer, including a large
        // requirement list omitted by the old flat-visibility estimate.
        let requires = (0..256)
            .map(|n| crate::declaration_join::ExactModuleIdentity {
                unit: "main".into(),
                module: format!("Dependency.LongModule{n}"),
            })
            .collect::<Vec<_>>();
        let row_bound = binding_row_metadata_bound("main", "ValueOwner", &requires).unwrap();
        assert!(row_bound > binding_row_metadata_bound("main", "ValueOwner", &[]).unwrap());
        let semantic = world_array(vec![
            world_text("main"),
            world_text("ValueOwner"),
            world_blob([0; 32]),
            world_blob([1; 32]),
            world_text("value"),
            world_array(
                requires
                    .iter()
                    .map(|key| world_array(vec![world_text(&key.unit), world_text(&key.module)]))
                    .collect(),
            ),
        ]);
        let row_fact = world_array(vec![semantic, world_blob([2; 32]), world_blob([3; 32])]);
        assert!(world_bytes(&row_fact).unwrap().len() as u64 <= row_bound);
        let store = fvm_ipld_blockstore::MemoryBlockstore::new();
        let mut materializer = Materializer::new(&store);
        let row = materializer.materialize_value(&row_fact).unwrap();
        let mut facts = imbl::OrdMap::<String, WorldBindingSnapshotFact>::new();
        let mut bound = 128 + row_bound;
        for index in 0..128 {
            let occurrence = format!("alias{index}");
            let symbol = (
                "main".to_owned(),
                "Original.Defining".to_owned(),
                "value".to_owned(),
                "original".to_owned(),
                Some("Parent".to_owned()),
            );
            bound += binding_metadata_bound(
                [
                    occurrence.len(),
                    symbol.0.len(),
                    symbol.1.len(),
                    symbol.2.len(),
                    symbol.3.len(),
                ],
                symbol.4.as_ref().unwrap().len(),
            )
            .unwrap();
            facts.insert(occurrence, (symbol, index, index, index, row));
        }
        materializer.materialize_map(None, &facts).unwrap();
        let staged = materializer.finish().unwrap();
        assert!(staged.metrics().staged_bytes <= bound);
    }
    #[test]
    fn primary_associations_preserve_scoped_bindings_and_bound_actual_staging() {
        use crate::artifact_inventory::{ArtifactId, ArtifactSelectionId};
        use fvm_ipld_blockstore::Blockstore;
        let requires = vec![crate::declaration_join::ExactModuleIdentity {
            unit: "dependency-unit".repeat(8),
            module: "Dependency.Module".repeat(8),
        }];
        let semantic = world_array(vec![
            world_text("main"),
            world_text("Owner"),
            world_blob([1; 32]),
            world_blob([2; 32]),
            world_text("module"),
            world_array(vec![world_array(vec![
                world_text(&requires[0].unit),
                world_text(&requires[0].module),
            ])]),
        ]);
        let store = fvm_ipld_blockstore::MemoryBlockstore::new();
        let mut materializer = Materializer::new(&store);
        let mut facts = imbl::OrdMap::new();
        let mut scoped_roots = Vec::new();
        let estimate = primary_association_metadata_bound("main", "Owner", &requires).unwrap();
        for index in 0..128u8 {
            let interface = ArtifactBinding {
                artifact: ArtifactId([3; 32]),
                selection: ArtifactSelectionId([index; 32]),
            };
            let original = ArtifactBinding {
                artifact: ArtifactId([4; 32]),
                ..interface
            };
            let fact = materialize_primary_association(
                &mut materializer,
                &interface,
                Some(&original),
                &semantic,
            )
            .unwrap();
            facts.insert(original, fact);
            if index < 2 {
                let mut one = imbl::OrdMap::new();
                one.insert(original, fact);
                scoped_roots.push(
                    *materializer
                        .materialize_map(None, &one)
                        .unwrap()
                        .root()
                        .cid(),
                );
            }
        }
        assert_ne!(
            scoped_roots[0], scoped_roots[1],
            "equal images cannot erase acquisition scopes"
        );
        let root = materializer.materialize_map(None, &facts).unwrap();
        let staged = materializer.finish().unwrap();
        assert!(staged.metrics().staged_bytes <= 128 * estimate + 128);
        for (cid, body) in staged.blocks() {
            store.put_keyed(cid, body).unwrap();
        }
        let bytes = store.get(root.root().cid()).unwrap().unwrap();
        assert!(!bytes.is_empty());
        let first = *facts.iter().next().unwrap().1;
        let value: ciborium::value::Value =
            ciborium::de::from_reader(store.get(&first).unwrap().unwrap().as_slice()).unwrap();
        let fields = value.as_array().unwrap();
        assert_eq!(fields[0].as_text(), Some("TPCWPRIMARY1"));
        assert_eq!(&fields[3], &semantic);
        assert!(primary_association_metadata_bound("main", "Owner", &[]).unwrap() < estimate);
    }

    #[test]
    fn empty_context_parcel_uses_production_primary_association_writer() {
        let context = ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .admit_producer([7; 32])
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let encoded = world_context_fixture(&context, directory.path()).unwrap();
        let parcel: ciborium::value::Value = ciborium::de::from_reader(encoded.as_slice()).unwrap();
        let fields = parcel.as_array().unwrap();
        assert_eq!(fields.len(), 5);
        assert!(fields[2].as_array().unwrap().is_empty());
        let blocks = fields[4].as_array().unwrap();
        for root in &fields[..2] {
            assert!(blocks
                .iter()
                .any(|block| &block.as_array().unwrap()[0] == root));
        }
    }

    #[test]
    fn world_identifier_namespace_is_checked_beyond_question_echo() {
        use InspectionNamespace as N;
        for actual in ["Value", "Type", "Constructor", "Field"] {
            assert!(validate_identifier_namespace(N::Any, actual).is_ok());
        }
        assert!(validate_identifier_namespace(N::Type, "Type").is_ok());
        assert!(validate_identifier_namespace(N::Type, "Value").is_err());
        assert!(validate_identifier_namespace(N::Field, "Value").is_err());
        assert!(validate_identifier_namespace(N::Value, "Field").is_ok());
        assert!(validate_identifier_namespace(N::Constructor, "Type").is_err());
        assert!(validate_identifier_namespace(N::Any, "Any").is_err());
    }
    #[test]
    fn compiler_world_refuses_an_unscoped_action_before_preparation() {
        let reached = std::cell::Cell::new(false);
        assert!(scoped_world_action(|| {
            reached.set(true);
            Ok(())
        })
        .is_err());
        assert!(!reached.get());
    }
}
