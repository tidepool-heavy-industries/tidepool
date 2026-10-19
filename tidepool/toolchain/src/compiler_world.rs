//! Compiler-owned worlds and their exact cold input owners.

use std::any::Any;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use tidepool_extract_cmd::{InspectionNamespace, InspectionScope, StructuredInspection};

use crate::artifact_inventory::CanonicalProducerIdentity;
use crate::cache::SourceRootManifest;
use crate::checked_cell::CheckedValueArtifact;
use crate::declaration_context::ExactDeclarationContext;
use crate::exact_context::{ContextRoot, Materializer, ReachableBlocks};
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
}

impl CompilerWorldBindings {
    /// Charge borrowed metadata before cloning it. Callers can feed the actual
    /// BindingTable iterator directly; no intermediate all-binding Vec is needed.
    pub fn from_published_bindings<'a>(
        query_scope: u64,
        bindings: impl IntoIterator<Item = CompilerWorldBinding<'a>>,
    ) -> Result<Self, CompileError> {
        let allowance = capture_allowance()?;
        let mut remaining = allowance.checked_sub(64).ok_or_else(source_size_failure)?;
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
            remaining = remaining.checked_sub(bytes).ok_or_else(|| {
                source_failure(format!(
                    "compiler binding metadata exceeds capture allowance {allowance} bytes"
                ))
            })?;
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
        })
    }

    pub(crate) fn cold_parts(
        &self,
    ) -> Result<(Vec<Arc<CheckedValueArtifact>>, Arc<[u8]>), CompileError> {
        use ciborium::value::Value;
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
        let mut ordinals = BTreeMap::new();
        let mut issuers = Vec::with_capacity(rows.len());
        for (ordinal, (owner, row)) in rows.iter().enumerate() {
            let ordinal = u32::try_from(ordinal).map_err(|_| source_size_failure())?;
            ordinals.insert(owner.as_str(), ordinal);
            let (producer, receipt) = row.world_issuer();
            issuers.push(world_array(vec![
                world_text(owner.clone()),
                world_blob(row.certified_interface().artifact_id().0),
                world_blob(producer),
                world_blob(receipt),
            ]));
        }
        let entries = self
            .entries
            .iter()
            .map(|(occurrence, entry)| {
                let symbol = &entry.symbol;
                let owner = entry.row.owner().module_name();
                world_array(vec![
                    world_text(occurrence.clone()),
                    world_array(vec![
                        world_text(symbol.unit.clone()),
                        world_text(symbol.module.clone()),
                        world_text(symbol.namespace.clone()),
                        world_text(symbol.occurrence.clone()),
                        symbol
                            .record_parent
                            .as_ref()
                            .map_or(Value::Null, |parent| world_text(parent.clone())),
                    ]),
                    entry.session_var.raw().into(),
                    entry.defining_scope.into(),
                    u64::from(ordinals[owner.as_str()]).into(),
                ])
            })
            .collect();
        let encoded = world_bytes(&world_array(vec![
            world_text("TPCWBIND2"),
            self.query_scope.into(),
            world_array(entries),
            world_array(issuers),
        ]))?;
        Ok((rows.into_values().collect(), encoded.into()))
    }
}

pub struct CompilerWorldOpenInputs {
    primary_context: Arc<ExactDeclarationContext>,
    visible_rows: Vec<Arc<CheckedValueArtifact>>,
    _bindings: CompilerWorldBindings,
    sources: Arc<CapturedCompilerSources>,
    visibility: Arc<[u8]>,
    directory: Arc<tempfile::TempDir>,
}

impl CompilerWorldOpenInputs {
    pub(crate) fn from_primary_selection(
        primary_context: Arc<ExactDeclarationContext>,
        visible_rows: Vec<Arc<CheckedValueArtifact>>,
        bindings: CompilerWorldBindings,
        sources: Arc<CapturedCompilerSources>,
        visibility: Arc<[u8]>,
        directory: Arc<tempfile::TempDir>,
    ) -> Result<Self, CompileError> {
        Ok(Self {
            primary_context,
            visible_rows,
            _bindings: bindings,
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
    primary_root: ContextRoot,
    primary_blocks: StagedBlocks,
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
            .field("primary_block_count", &self.primary_blocks.blocks().len())
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

// Nine bytes bounds every CBOR head and integer. Each occurrence is charged
// for its own issuer row even when aliases will share that row. SessionModule's
// generated name has a bounded u64 generation, independently of SymbolIdentity.
fn binding_metadata_bound(fields: [usize; 5], parent_bytes: usize) -> Result<u64, CompileError> {
    let max_owner = tidepool_repr::SessionModule::val(tidepool_repr::Generation(u64::MAX))
        .module_name()
        .len() as u64;
    let entry_heads = 11 * 9;
    let issuer_row = 9 + 9 + max_owner + 3 * (9 + 32);
    fields
        .into_iter()
        .chain([parent_bytes])
        .try_fold(entry_heads + issuer_row, |size, field| {
            size.checked_add(field as u64)
                .ok_or_else(source_size_failure)
        })
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
        let row = self
            .rows
            .entry(key.clone())
            .or_insert(interface_row)
            .clone();
        self.images.entry(key).or_insert(image);
        Ok((world_array(vec![row, world_blob(digest)]), digest))
    }
}

/// Logical transfer facts only: paths and descriptor aliases do not identify a
/// row. The image binds all immutable part hashes; role and required owners are
/// separate semantic facts and must also survive transport unchanged.
fn world_semantic_row(
    row: &ciborium::value::Value,
    producer: &[u8; 32],
) -> Result<ciborium::value::Value, CompileError> {
    use ciborium::value::Value;
    let Value::Array(row) = row else {
        return Err(source_failure("invalid world row"));
    };
    let [Value::Array(fields), Value::Bytes(image)] = row.as_slice() else {
        return Err(source_failure("invalid world row image"));
    };
    let [Value::Text(unit), Value::Text(module), _, _, Value::Array(requires), _, _, Value::Array(evidence)] =
        fields.as_slice()
    else {
        return Err(source_failure("invalid world interface row"));
    };
    let Some(Value::Text(role)) = evidence.first() else {
        return Err(source_failure("invalid world row role"));
    };
    if image.len() != 32
        || !matches!(
            role.as_str(),
            "module" | "native-declaration" | "join" | "value"
        )
    {
        return Err(source_failure("invalid world row semantic facts"));
    }
    let mut owners = std::collections::BTreeSet::new();
    for owner in requires {
        let Value::Array(key) = owner else {
            return Err(source_failure("invalid world row requirement"));
        };
        let [Value::Text(unit), Value::Text(module)] = key.as_slice() else {
            return Err(source_failure("invalid world required owner"));
        };
        if !owners.insert((unit, module)) {
            return Err(source_failure("duplicate world required owner"));
        }
    }
    Ok(world_array(vec![
        world_text(unit.clone()),
        world_text(module.clone()),
        world_blob(producer),
        world_blob(image),
        world_text(role.clone()),
        world_array(
            owners
                .into_iter()
                .map(|(unit, module)| {
                    world_array(vec![world_text(unit.clone()), world_text(module.clone())])
                })
                .collect(),
        ),
    ]))
}

impl CompilerWorldOpenInputs {
    fn materialize_primary(
        &self,
        endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
    ) -> Result<(Arc<ExactDeclarationContext>, ContextRoot, StagedBlocks), CompileError> {
        // Published empty contexts have no producer yet. Bind that identity at
        // the actual admitted endpoint before issuing any portable root; a
        // context already issued by another producer must refuse here.
        let producer = CanonicalProducerIdentity::from_compiler(endpoint.identity()).sha256();
        let primary_context = Arc::new((*self.primary_context).clone().admit_producer(producer)?);
        let snapshot = primary_context.snapshot();
        let store = fvm_ipld_blockstore::MemoryBlockstore::new();
        let mut materializer = Materializer::new(&store);
        let context = materializer
            .materialize_context(None, &snapshot)
            .map_err(|error| source_failure(error.to_string()))?;
        let root = context.root().clone();
        let blocks = materializer
            .finish()
            .map_err(|error| source_failure(error.to_string()))?;
        Ok((primary_context, root, blocks))
    }

    fn prepare(
        mut self,
        endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
    ) -> Result<Arc<CompilerWorldRestoration>, CompileError> {
        use crate::declaration_context::original_inputs::{
            OriginalInputKind as K, OriginalInputPart,
        };
        let identity = endpoint.identity();
        let producer = CanonicalProducerIdentity::from_compiler(identity).sha256();
        let profile = *identity
            .package_source_association()
            .ok_or_else(|| {
                source_failure("compiler world requires a retained service package source")
            })?
            .profile_id();
        let (primary_context, primary_root, primary_blocks) = self.materialize_primary(endpoint)?;
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
            .iter()
            .try_fold(0u64, |total, (cid, body)| {
                total
                    .checked_add(body.len() as u64)
                    .and_then(|n| n.checked_add(cid.to_bytes().len() as u64 + 32))
                    .ok_or_else(source_size_failure)
            })?;
        let envelope_bytes = block_bytes
            .checked_add(self.sources.encoded().len() as u64)
            .and_then(|n| n.checked_add(self.visibility.len() as u64))
            .ok_or_else(source_size_failure)?;
        let mut remaining_raw = capture_allowance()?
            .checked_sub(envelope_bytes)
            .ok_or_else(source_size_failure)?;
        let metadata = self.primary_context.compiler_metadata_snapshot()?;
        let mut base = Vec::with_capacity(metadata.entries.len());
        let mut base_references = Vec::with_capacity(metadata.entries.len());
        for (ordinal, entry) in metadata.entries.values().enumerate() {
            crate::host_work::checkpoint()?;
            let root = self.directory.path().join(format!("primary-{ordinal}"));
            let mut builder =
                crate::owned_input_arena::OwnedInputArenaBuilder::new(capture_allowance()?)
                    .map_err(|e| source_failure(e.to_string()))?;
            let mut pending = Vec::new();
            for (kind, bytes) in crate::declaration_context::original_inputs::entry_payloads(entry)
            {
                if matches!(kind, K::Native | K::Census | K::Graph) {
                    continue;
                }
                if bytes.is_empty() || bytes.len() as u64 > kind.byte_limit() {
                    return Err(source_failure("world type input exceeds its format bound"));
                }
                remaining_raw = remaining_raw
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
            base_references.push(world_semantic_row(&row, &producer)?);
            base.push(row);
        }
        let mut visible = Vec::with_capacity(self.visible_rows.len());
        let mut references = Vec::with_capacity(self.visible_rows.len());
        for artifact in &self.visible_rows {
            crate::host_work::checkpoint()?;
            let (entry, parts) = artifact.world_type_parts()?;
            for part in &parts {
                remaining_raw = remaining_raw
                    .checked_sub(part.bytes)
                    .ok_or_else(source_size_failure)?;
            }
            let (row, _) = rows.row(&entry, parts)?;
            references.push(world_semantic_row(&row, &producer)?);
            visible.push(row);
        }
        if let Some(capture) = &rows.diagnostics {
            if let Err(error) = capture.finish_original_parts() {
                tracing::warn!(%error,"compiler world input diagnostic manifest is incomplete");
            }
        }
        let blocks = primary_blocks
            .blocks()
            .map(|(cid, body)| world_array(vec![world_blob(cid.to_bytes()), world_blob(body)]))
            .collect();
        let seals = primary_blocks
            .blocks()
            .map(|(cid, body)| {
                world_array(vec![
                    world_blob(cid.to_bytes()),
                    world_blob(world_digest(body)),
                ])
            })
            .collect();
        // The closed Materializer issued the CID basis. This SHA envelope binds
        // transfer and restoration; it does not independently verify CID content.
        let product = world_bytes(&world_array(vec![
            world_text("TPCWREST"),
            2u64.into(),
            world_blob(primary_root.cid().to_bytes()),
            world_blob(self.sources.seal()),
            world_blob(producer),
            world_blob(profile),
            world_blob(&self.visibility),
            world_blob(world_digest(&world_bytes(&world_array(base_references))?)),
            world_blob(world_digest(&world_bytes(&world_array(references))?)),
            world_blob(world_digest(&world_bytes(&world_array(seals))?)),
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
            2u64.into(),
            world_blob(&product),
            world_path(&source_path)?,
            world_blob(producer),
            world_blob(profile),
            world_array(vec![world_array(base), world_array(visible)]),
            acquisition,
            world_blob(&self.visibility),
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

/// Open exact published primary/source facts, then ask the actual service. The
/// public handle is returned only after the service has accepted those facts.
pub fn open_world(
    cold: CompilerWorldOpenInputs,
    endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
    question: PublicModuleQuestion,
) -> Result<WorldQueryResult, CompileError> {
    scoped_world_action(|| {
        let restoration = cold.prepare(endpoint)?;
        let world = Arc::new(CompilerWorld {
            restoration,
            realization: Arc::new(Mutex::new(None)),
        });
        query_world_inner(endpoint.identity(), world, question)
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
            Vec::new(),
            Arc::from(&b"published empty namespace"[..]),
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
            Vec::new(),
            Arc::from(&b"published empty namespace"[..]),
            Arc::clone(&sources),
        )
        .unwrap();
        assert_eq!(issued.materialize_primary(&endpoint).unwrap().1, root);
        let wrong = crate::artifacts::ModuleCandidateOffer::compiler_world_inputs(
            Some(Arc::new(
                (*cold.primary_context).clone().admit_producer(raw).unwrap(),
            )),
            Vec::new(),
            Arc::from(&b"published empty namespace"[..]),
            sources,
        )
        .unwrap();
        assert!(wrong.materialize_primary(&endpoint).is_err());
    }

    #[test]
    fn cold_visibility_rejects_oversize_before_clone_or_allocation() {
        assert!(binding_metadata_bound([usize::MAX; 5], 0).is_err());
        assert!(binding_metadata_bound([0; 5], usize::MAX).is_err());
        let symbol = ["alias", "base", "Actual.Defining", "Value", "definition"];
        let encoded_entry = world_array(vec![
            world_text(symbol[0]),
            world_array(
                symbol[1..]
                    .iter()
                    .map(|s| world_text(*s))
                    .chain([ciborium::value::Value::Null])
                    .collect(),
            ),
            u64::MAX.into(),
            u64::MAX.into(),
            u64::MAX.into(),
        ]);
        let owner =
            tidepool_repr::SessionModule::val(tidepool_repr::Generation(u64::MAX)).module_name();
        let issuer = world_array(vec![
            world_text(owner),
            world_blob([0; 32]),
            world_blob([1; 32]),
            world_blob([2; 32]),
        ]);
        let encoded = world_bytes(&world_array(vec![encoded_entry, issuer])).unwrap();
        assert!(binding_metadata_bound(symbol.map(str::len), 0).unwrap() >= encoded.len() as u64);
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
