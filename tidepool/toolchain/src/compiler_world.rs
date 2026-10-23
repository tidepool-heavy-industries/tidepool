//! Compiler-owned worlds and their exact cold input owners.

use std::any::Any;

pub(crate) mod native_demand;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use tidepool_extract_cmd::{
    CompilerResponseRejection, InspectionNamespace, InspectionScope, StructuredInspection,
};

use crate::artifact_inventory::CanonicalProducerIdentity;
use crate::cache::SourceRootManifest;
use crate::checked_cell::CheckedValueArtifact;
use crate::declaration_context::ExactDeclarationContext;
use crate::exact_context::{
    MaterializedContext, MaterializedMap, Materializer, ReachableBlocks, SemanticRoots,
};
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

/// A visible host binding carried with the actual compiler proof that issued
/// its type row. Runtime identities remain coordinates; the proof selects the
/// exact row and distinguishes checked execution from a thin host interface.
pub struct CompilerWorldHostBinding<'a> {
    occurrence: &'a str,
    symbol: &'a tidepool_repr::execution_schema::SymbolIdentity,
    session_var: tidepool_repr::SessionVarId,
    defining_scope: u64,
    issuer: PrivateHostBindingIssuer,
}

enum PrivateHostBindingIssuer {
    CheckedExecution(Arc<crate::checked_cell::ExactCompiledItem>),
    HostInterface(Arc<crate::checked_cell::ExactHostBindingInterface>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrivateHostBindingKind {
    CheckedExecution,
    HostInterface,
}

impl<'a> CompilerWorldHostBinding<'a> {
    pub fn from_checked_execution(
        occurrence: &'a str,
        symbol: &'a tidepool_repr::execution_schema::SymbolIdentity,
        session_var: tidepool_repr::SessionVarId,
        defining_scope: u64,
        execution: Arc<crate::checked_cell::ExactCompiledItem>,
    ) -> Self {
        Self {
            occurrence,
            symbol,
            session_var,
            defining_scope,
            issuer: PrivateHostBindingIssuer::CheckedExecution(execution),
        }
    }

    pub fn from_host_interface(
        occurrence: &'a str,
        symbol: &'a tidepool_repr::execution_schema::SymbolIdentity,
        session_var: tidepool_repr::SessionVarId,
        defining_scope: u64,
        interface: Arc<crate::checked_cell::ExactHostBindingInterface>,
    ) -> Self {
        Self {
            occurrence,
            symbol,
            session_var,
            defining_scope,
            issuer: PrivateHostBindingIssuer::HostInterface(interface),
        }
    }

    fn row(&self) -> Result<Arc<CheckedValueArtifact>, CompileError> {
        match &self.issuer {
            PrivateHostBindingIssuer::CheckedExecution(execution) => execution
                .value_interface_certificate()
                .ok_or_else(|| source_failure("checked host execution has no value interface")),
            PrivateHostBindingIssuer::HostInterface(interface) => {
                Ok(interface.value_interface_certificate())
            }
        }
    }

    fn kind(&self) -> PrivateHostBindingKind {
        match &self.issuer {
            PrivateHostBindingIssuer::CheckedExecution(_) => {
                PrivateHostBindingKind::CheckedExecution
            }
            PrivateHostBindingIssuer::HostInterface(_) => PrivateHostBindingKind::HostInterface,
        }
    }
}

#[derive(Clone, Debug)]
struct PrivateHostBindingProgressBinding {
    occurrence: String,
    symbol: tidepool_repr::execution_schema::SymbolIdentity,
    session_var: tidepool_repr::SessionVarId,
    defining_scope: u64,
    row: Arc<CheckedValueArtifact>,
    kind: PrivateHostBindingKind,
}

/// Closed proof of the visible host-binding rows projected into a private
/// compiler World. Only the compiler World projection below issues it.
#[derive(Debug)]
pub struct PrivateHostBindingProgress {
    bindings: Vec<PrivateHostBindingProgressBinding>,
}

impl PrivateHostBindingProgress {
    pub fn len(&self) -> usize {
        self.bindings.len()
    }
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
    pub fn checked_execution_bindings(&self) -> usize {
        self.bindings
            .iter()
            .filter(|binding| binding.kind == PrivateHostBindingKind::CheckedExecution)
            .count()
    }
    pub fn host_interface_bindings(&self) -> usize {
        self.bindings
            .iter()
            .filter(|binding| binding.kind == PrivateHostBindingKind::HostInterface)
            .count()
    }

    fn resumed_from(prior: &Arc<Self>, extension: &Arc<Self>) -> Result<Arc<Self>, CompileError> {
        let mut bindings = prior.bindings.clone();
        for binding in &extension.bindings {
            if bindings
                .iter()
                .any(|held| held.session_var == binding.session_var)
            {
                return Err(source_failure(
                    "visible host binding progress repeats an occurrence",
                ));
            }
            bindings.push(binding.clone());
        }
        Ok(Arc::new(Self { bindings }))
    }
}

pub struct CompilerWorldHostSuccessor {
    predecessor: Arc<CompilerWorld>,
    world: Arc<CompilerWorld>,
    progress: Arc<PrivateHostBindingProgress>,
}

impl CompilerWorldHostSuccessor {
    pub fn world(&self) -> &Arc<CompilerWorld> {
        &self.world
    }
    pub fn progress(&self) -> &Arc<PrivateHostBindingProgress> {
        &self.progress
    }
}

#[derive(Clone, Debug)]
struct WorldBindingFact {
    symbol: tidepool_repr::execution_schema::SymbolIdentity,
    session_var: tidepool_repr::SessionVarId,
    defining_scope: u64,
    row: Arc<CheckedValueArtifact>,
}

/// The two host-owned bindings exposed by the resident cell source producer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorldCellOperandExposure {
    Input,
    SessionInput,
}

/// One checked host value supplied to a cell operation. This compiler evidence
/// does not publish a World binding or authorize access to a runtime value.
#[derive(Debug)]
pub struct CompilerWorldCellOperand {
    exposure: WorldCellOperandExposure,
    symbol: tidepool_repr::execution_schema::SymbolIdentity,
    session_var: tidepool_repr::SessionVarId,
    defining_scope: u64,
    row: Arc<CheckedValueArtifact>,
    row_fact_cid: cid::Cid,
    request: tidepool_extract_cmd::WorldCellOperandRequest,
    directory: Arc<tempfile::TempDir>,
    input_files: Vec<Arc<std::fs::File>>,
}

impl CompilerWorldCellOperand {
    pub fn from_host_interface(
        world: &CompilerWorld,
        exposure: WorldCellOperandExposure,
        defining_scope: u64,
        interface: &Arc<crate::checked_cell::ExactHostBindingInterface>,
    ) -> Result<Arc<Self>, CompileError> {
        Self::from_binder(
            world,
            exposure,
            defining_scope,
            interface.binder(),
            interface.value_interface_certificate(),
        )
    }

    pub fn from_checked_host_execution(
        world: &CompilerWorld,
        exposure: WorldCellOperandExposure,
        defining_scope: u64,
        execution: &Arc<crate::checked_cell::ExactCompiledItem>,
    ) -> Result<Arc<Self>, CompileError> {
        let row = execution
            .value_interface_certificate()
            .ok_or_else(|| source_failure("host execution has no checked value interface"))?;
        let mut binders = execution.checked_bound_binders();
        let binder = binders
            .next()
            .ok_or_else(|| source_failure("host execution has no bound operand"))?;
        if binders.next().is_some() {
            return Err(source_failure("host execution has more than one operand"));
        }
        Self::from_binder(world, exposure, defining_scope, binder, row)
    }

    fn from_binder(
        world: &CompilerWorld,
        exposure: WorldCellOperandExposure,
        defining_scope: u64,
        binder: &ciborium::value::Value,
        artifact: Arc<CheckedValueArtifact>,
    ) -> Result<Arc<Self>, CompileError> {
        let fields = crate::checked_cell::row(binder, 7)?;
        let name = crate::checked_cell::string(&fields[0])?;
        let module = crate::checked_cell::string(&fields[2])?;
        let var = match &fields[1] {
            ciborium::value::Value::Integer(value) => u64::try_from(*value)
                .map_err(|_| source_failure("host operand runtime identity"))?,
            _ => return Err(source_failure("host operand runtime identity")),
        };
        let producer = world.restoration.producer;
        if module != artifact.owner().module_name() || artifact.world_issuer().0 != producer {
            return Err(source_failure(
                "host operand belongs to another compiler owner",
            ));
        }
        let symbol = tidepool_repr::execution_schema::SymbolIdentity {
            unit: "main".into(),
            module: module.into(),
            namespace: "value".into(),
            occurrence: name.into(),
            record_parent: None,
        };
        let directory = Arc::new(crate::artifacts::compiler_scratch_directory()?);
        let mut rows = WorldColdRows {
            producer,
            files: BTreeMap::new(),
            images: BTreeMap::new(),
            rows: BTreeMap::new(),
            arenas: Default::default(),
            diagnostics: None,
        };
        let (entry, parts) = artifact.world_type_parts()?;
        let mut remaining = capture_allowance()?;
        remaining = remaining
            .checked_sub(binding_row_metadata_bound(
                &entry.descriptor.owner.unit,
                &entry.descriptor.owner.module,
                &entry.requirements,
            )?)
            .ok_or_else(source_size_failure)?;
        for part in &parts {
            remaining = remaining
                .checked_sub(part.bytes)
                .ok_or_else(source_size_failure)?;
        }
        let (offered, _) = rows.row(&entry, parts)?;
        let row_fact = world_array(vec![
            world_semantic_row(&offered, &producer)?,
            world_blob(artifact.certified_interface().artifact_id().0),
            world_blob(artifact.world_issuer().1),
        ]);
        let store = fvm_ipld_blockstore::MemoryBlockstore::new();
        let mut materializer = Materializer::new(&store);
        let row_fact_cid = materializer
            .materialize_value(&row_fact)
            .map_err(|error| source_failure(error.to_string()))?;
        let staged = materializer
            .finish()
            .map_err(|error| source_failure(error.to_string()))?;
        let blocks = staged
            .blocks()
            .iter()
            .map(|(cid, body)| world_array(vec![world_blob(cid.to_bytes()), world_blob(body)]))
            .collect();
        let acquisition = rows
            .arenas
            .acquisition(rows.images.into_values().collect())?;
        let descriptor = world_array(vec![
            world_text("TPCWCELLOPERAND1"),
            match exposure {
                WorldCellOperandExposure::Input => 0u64,
                WorldCellOperandExposure::SessionInput => 1,
            }
            .into(),
            world_array(vec![
                world_array(vec![
                    world_text(&symbol.unit),
                    world_text(&symbol.module),
                    world_text(&symbol.namespace),
                    world_text(&symbol.occurrence),
                    ciborium::value::Value::Null,
                ]),
                var.into(),
                defining_scope.into(),
                artifact.owner().gen.0.into(),
                world_blob(row_fact_cid.to_bytes()),
            ]),
            world_array(blocks),
            offered,
            acquisition,
        ]);
        let bytes = world_bytes(&descriptor)?;
        let sha256 = world_digest(&bytes);
        let path = directory.path().join("cell-operand.cbor");
        std::fs::write(&path, bytes).map_err(|error| source_failure(error.to_string()))?;
        Ok(Arc::new(Self {
            exposure,
            symbol,
            session_var: tidepool_repr::SessionVarId::from_extract(var),
            defining_scope,
            row: artifact,
            row_fact_cid,
            request: tidepool_extract_cmd::WorldCellOperandRequest {
                descriptor: path,
                sha256,
            },
            directory,
            input_files: rows.files.into_values().collect(),
        }))
    }

    pub fn exposure(&self) -> WorldCellOperandExposure {
        self.exposure
    }
    pub fn symbol(&self) -> &tidepool_repr::execution_schema::SymbolIdentity {
        &self.symbol
    }
    pub fn session_var(&self) -> tidepool_repr::SessionVarId {
        self.session_var
    }
    pub fn defining_scope(&self) -> u64 {
        self.defining_scope
    }
    pub fn generation(&self) -> u64 {
        self.row.owner().gen.0
    }
    pub fn row_fact_cid(&self) -> &cid::Cid {
        &self.row_fact_cid
    }
    pub fn checked_row(&self) -> &Arc<CheckedValueArtifact> {
        &self.row
    }
    pub(crate) fn request(&self) -> &tidepool_extract_cmd::WorldCellOperandRequest {
        &self.request
    }

    fn retain_for_request(&self) -> Result<Vec<Arc<std::fs::File>>, CompileError> {
        if !tidepool_extract_cmd::retain_compiler_input_directory(Arc::clone(&self.directory)) {
            return Err(source_failure(
                "cell operand requires its compiler transaction",
            ));
        }
        Ok(self.input_files.clone())
    }
}

/// The current occurrence table, coupled to genuine checked rows by its
/// publication owner. Aliases keep independent names and share their row.
#[derive(Clone)]
pub struct VisibleWorldBindings {
    query_scope: u64,
    entries: imbl::OrdMap<String, WorldBindingFact>,
    metadata_bytes: u64,
    row_users: imbl::OrdMap<String, usize>,
    rows: imbl::OrdMap<String, Arc<CheckedValueArtifact>>,
}

struct VisibleWorldBindingAdvance {
    bindings: VisibleWorldBindings,
    occurrences: std::collections::BTreeSet<String>,
    rows: std::collections::BTreeSet<String>,
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

impl VisibleWorldBindings {
    /// Charge borrowed metadata before cloning it. Callers can feed the actual
    /// BindingTable iterator directly; no intermediate all-binding Vec is needed.
    pub fn from_published_bindings<'a>(
        query_scope: u64,
        bindings: impl IntoIterator<Item = CompilerWorldBinding<'a>>,
    ) -> Result<Self, CompileError> {
        let mut selected = Self {
            query_scope,
            entries: Default::default(),
            metadata_bytes: 128,
            row_users: Default::default(),
            rows: Default::default(),
        };
        let allowance = capture_allowance()?;
        if selected.metadata_bytes > allowance {
            return Err(binding_capture_refusal(selected.metadata_bytes, allowance));
        }
        for input in bindings {
            crate::host_work::checkpoint()?;
            selected.insert(input)?;
        }
        Ok(selected)
    }

    fn insert(&mut self, input: CompilerWorldBinding<'_>) -> Result<(), CompileError> {
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
            || symbol.unit != "main"
            || symbol.namespace != "value"
            || symbol.module != input.row.owner().module_name()
        {
            return Err(source_failure("invalid current compiler binding identity"));
        }
        if self.entries.contains_key(input.occurrence) {
            return Err(source_failure("duplicate visible compiler occurrence"));
        }
        let metadata_bytes = self
            .metadata_bytes
            .checked_add(binding_metadata_bound(
                fields.map(str::len),
                symbol.record_parent.as_ref().map_or(0, String::len),
            )?)
            .ok_or_else(source_size_failure)?;
        let allowance = capture_allowance()?;
        if metadata_bytes > allowance {
            return Err(binding_capture_refusal(metadata_bytes, allowance));
        }
        let owner = input.row.owner().module_name();
        if self
            .rows
            .get(&owner)
            .is_some_and(|old| !old.same_world_type_row(&input.row))
        {
            return Err(source_failure(
                "competing checked rows for one current compiler owner",
            ));
        }
        let users = self
            .row_users
            .get(&owner)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(source_size_failure)?;
        self.metadata_bytes = metadata_bytes;
        self.row_users.insert(owner.clone(), users);
        if !self.rows.contains_key(&owner) {
            self.rows.insert(owner, input.row.clone());
        }
        self.entries.insert(
            input.occurrence.to_owned(),
            WorldBindingFact {
                symbol: symbol.clone(),
                session_var: input.session_var,
                defining_scope: input.defining_scope,
                row: input.row,
            },
        );
        Ok(())
    }

    fn remove(&mut self, occurrence: &str) -> Result<(), CompileError> {
        let Some(old) = self.entries.remove(occurrence) else {
            return Ok(());
        };
        self.metadata_bytes = self
            .metadata_bytes
            .checked_sub(binding_metadata_bound(
                [
                    occurrence.len(),
                    old.symbol.unit.len(),
                    old.symbol.module.len(),
                    old.symbol.namespace.len(),
                    old.symbol.occurrence.len(),
                ],
                old.symbol.record_parent.as_ref().map_or(0, String::len),
            )?)
            .ok_or_else(|| source_failure("predecessor binding metadata charge underflow"))?;
        let owner = old.row.owner().module_name();
        let users = self
            .row_users
            .get(&owner)
            .copied()
            .ok_or_else(|| source_failure("predecessor binding has no row custody count"))?;
        if users == 1 {
            self.row_users.remove(&owner);
            self.rows.remove(&owner);
        } else {
            self.row_users.insert(owner, users - 1);
        }
        Ok(())
    }

    fn advance(
        &self,
        query_scope: u64,
        changes: impl IntoIterator<Item = (String, Option<WorldBindingFact>)>,
    ) -> Result<VisibleWorldBindingAdvance, CompileError> {
        let changes: Vec<_> = changes.into_iter().collect();
        let mut occurrences = std::collections::BTreeSet::new();
        let mut rows = std::collections::BTreeSet::new();
        for (occurrence, fact) in &changes {
            if !occurrences.insert(occurrence.clone()) {
                return Err(source_failure("duplicate visible compiler binding change"));
            }
            if let Some(old) = self.entries.get(occurrence) {
                rows.insert(old.row.owner().module_name());
            }
            if let Some(fact) = fact {
                rows.insert(fact.row.owner().module_name());
            }
        }
        let mut bindings = self.clone();
        bindings.query_scope = query_scope;
        for (occurrence, _) in &changes {
            bindings.remove(occurrence)?;
        }
        for (occurrence, fact) in changes {
            if let Some(fact) = fact {
                bindings.insert(CompilerWorldBinding::from_checked_value(
                    &occurrence,
                    &fact.symbol,
                    fact.session_var,
                    fact.defining_scope,
                    fact.row,
                ))?;
            }
        }
        Ok(VisibleWorldBindingAdvance {
            bindings,
            occurrences,
            rows,
        })
    }

    #[cfg(test)]
    pub(crate) fn assert_shared_row_alias_removal_for_test(&self) {
        assert_eq!(self.entries.len(), 2);
        assert_eq!(self.rows.len(), 1);
        assert_eq!(
            self.row_users.values().copied().collect::<Vec<_>>(),
            vec![2]
        );
        let names = self.entries.keys().cloned().collect::<Vec<_>>();
        assert!(self
            .advance(
                self.query_scope,
                [(names[0].clone(), None), (names[0].clone(), None)]
            )
            .is_err());
        assert_eq!(
            self.entries.len(),
            2,
            "refused change preserves predecessor"
        );
        let next = self
            .advance(self.query_scope, [(names[0].clone(), None)])
            .unwrap()
            .bindings;
        let expected = Self::from_published_bindings(
            self.query_scope,
            next.entries.iter().map(|(name, fact)| {
                CompilerWorldBinding::from_checked_value(
                    name,
                    &fact.symbol,
                    fact.session_var,
                    fact.defining_scope,
                    fact.row.clone(),
                )
            }),
        )
        .unwrap();
        assert_eq!(next.entries.len(), 1);
        assert_eq!(
            next.row_users.values().copied().collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(next.rows.len(), expected.rows.len());
        assert_eq!(next.metadata_bytes, expected.metadata_bytes);
        for (owner, row) in &next.rows {
            assert!(expected.rows.get(owner).unwrap().same_world_type_row(row));
            assert!(self.rows.get(owner).unwrap().same_world_type_row(row));
        }
        let empty = next
            .advance(self.query_scope, [(names[1].clone(), None)])
            .unwrap()
            .bindings;
        let expected_empty =
            Self::from_published_bindings(self.query_scope, std::iter::empty()).unwrap();
        assert!(empty.entries.is_empty() && empty.rows.is_empty() && empty.row_users.is_empty());
        assert_eq!(empty.metadata_bytes, expected_empty.metadata_bytes);
        assert_eq!(
            self.entries.len(),
            2,
            "successful derivation preserves predecessor"
        );
    }

    /// The scope this occurrence table was published for.
    pub fn scope(&self) -> u64 {
        self.query_scope
    }

    /// The visible occurrences and their defining identities, in occurrence
    /// order. Enumeration answers need nothing else: presentation and row
    /// availability belong to `info`, which does inspect a target.
    pub fn names(&self) -> impl Iterator<Item = WorldBindingName> + '_ {
        self.entries
            .iter()
            .map(|(occurrence, fact)| WorldBindingName {
                occurrence: occurrence.clone(),
                unit: fact.symbol.unit.clone(),
                module: fact.symbol.module.clone(),
                namespace: fact.symbol.namespace.clone(),
            })
    }

    fn cold_snapshot(
        &self,
        rows: &imbl::OrdMap<String, cid::Cid>,
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

/// One published occurrence, reduced to the identity an enumeration answer
/// carries. The defining row stays with its artifact owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldBindingName {
    pub occurrence: String,
    pub unit: String,
    pub module: String,
    pub namespace: String,
}

/// The selected declaration facade remains with its issuing publication owner.
/// Availability of another interface does not select it for source imports.
#[derive(Clone, Debug)]
pub enum WorldDeclarationSelection {
    Joined(Arc<crate::declaration_join::AcceptedJoin>),
    Recovered(Arc<crate::declaration_join::RecoveredDeclarationTip>),
}

/// Descriptive profile of a protected compiler observation, never execution authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorldObservationProfile {
    Workbench,
    HostCarrier,
}

/// Source-independent declarations issued by the actor's common source producer.
/// Native request types retain their original checked-signature representation.
/// Static imports are specifications without the leading `import`; the compiler
/// renders that keyword separately from the World's complete lexical import lines.
#[derive(Clone, Debug)]
pub struct WorldObservationScaffold {
    profile: WorldObservationProfile,
    fragment: Arc<str>,
    static_imports: Arc<[String]>,
    request_types: Option<(
        crate::declaration_join::RequestHelperRecipe,
        Arc<crate::checked_cell::RequestTypeSignatures>,
    )>,
    input_type: Option<Arc<crate::checked_cell::ExactCheckedSignature>>,
    type_interfaces: Arc<[Arc<crate::declaration_context::SelectedTypeInterfaces>]>,
    metadata_bytes: u64,
}

impl PartialEq for WorldObservationScaffold {
    fn eq(&self, other: &Self) -> bool {
        self.profile == other.profile
            && self.fragment == other.fragment
            && self.static_imports == other.static_imports
            && self.request_types == other.request_types
            && self.input_type == other.input_type
    }
}
impl Eq for WorldObservationScaffold {}

impl WorldObservationScaffold {
    pub fn new(
        profile: WorldObservationProfile,
        fragment: Arc<str>,
        static_imports: Vec<String>,
        request_types: Option<(
            crate::declaration_join::RequestHelperRecipe,
            Arc<crate::checked_cell::RequestTypeSignatures>,
        )>,
        input_type: Option<Arc<crate::checked_cell::ExactCheckedSignature>>,
        type_interfaces: Vec<Arc<crate::declaration_context::SelectedTypeInterfaces>>,
    ) -> Result<Self, CompileError> {
        if fragment.is_empty() || fragment.contains('\0') {
            return Err(source_failure("invalid protected observation fragment"));
        }
        for import in &static_imports {
            if import.is_empty() || import.contains(['\0', '\n']) {
                return Err(source_failure("invalid protected observation import"));
            }
        }
        let allowance = capture_allowance()?;
        let input_bytes =
            static_imports
                .iter()
                .try_fold(fragment.len() as u64, |total, import| {
                    total
                        .checked_add(import.len() as u64)
                        .ok_or_else(source_size_failure)
                })?;
        if input_bytes > allowance {
            return Err(binding_capture_refusal(input_bytes, allowance));
        }
        let mut scaffold = Self {
            profile,
            fragment,
            static_imports: static_imports.into(),
            request_types,
            input_type,
            type_interfaces: type_interfaces.into(),
            metadata_bytes: 0,
        };
        let bytes = world_bytes(&scaffold.value())?.len() as u64;
        if bytes > allowance {
            return Err(binding_capture_refusal(bytes, allowance));
        }
        scaffold.metadata_bytes = bytes;
        Ok(scaffold)
    }

    /// Actual selected original TYPE owners, retained separately from wire data.
    /// The World primary issuer validates and incorporates their canonical roles.
    pub fn type_interfaces(&self) -> &[Arc<crate::declaration_context::SelectedTypeInterfaces>] {
        &self.type_interfaces
    }

    fn value(&self) -> ciborium::value::Value {
        world_array(vec![
            match self.profile {
                WorldObservationProfile::Workbench => 0u64,
                WorldObservationProfile::HostCarrier => 1u64,
            }
            .into(),
            world_text(self.fragment.as_ref()),
            world_array(self.static_imports.iter().map(world_text).collect()),
            if self.request_types.is_none() && self.input_type.is_none() {
                ciborium::value::Value::Null
            } else {
                world_array(vec![
                    self.request_types.as_ref().map_or(
                        ciborium::value::Value::Null,
                        |(recipe, signatures)| {
                            world_array(vec![
                                world_text(recipe.as_str()),
                                signatures.authorization_value(),
                            ])
                        },
                    ),
                    self.input_type
                        .as_ref()
                        .map_or(ciborium::value::Value::Null, |signature| {
                            crate::checked_cell::encode_signature(signature)
                        }),
                ])
            },
        ])
    }
}

/// Ordered import syntax with separately issued declaration and observation facts.
/// Source selection and runtime grants remain with their owning admissions.
#[derive(Clone, Debug)]
pub struct CompilerWorldLexicalSurface {
    import_lines: Arc<[String]>,
    declaration: Option<WorldDeclarationSelection>,
    observation: Option<Arc<WorldObservationScaffold>>,
    metadata_bytes: u64,
}

impl CompilerWorldLexicalSurface {
    /// Original TYPE selections attached by the protected source issuer.
    pub(crate) fn observation_type_interfaces(
        &self,
    ) -> &[Arc<crate::declaration_context::SelectedTypeInterfaces>] {
        self.observation
            .as_ref()
            .map_or(&[], |scaffold| scaffold.type_interfaces())
    }

    /// Complete Haskell import statements used to render compiler source.
    /// This checks statement framing and size; the compiler parses the syntax.
    /// Source publication requires its selected context's lexical proof.
    /// Raw template specifications must be rendered before this boundary.
    pub fn from_import_lines(
        import_lines: Vec<String>,
        declaration: Option<WorldDeclarationSelection>,
    ) -> Result<Self, CompileError> {
        let mut bytes = import_lines.iter().try_fold(128u64, |bytes, line| {
            if !line.strip_prefix("import").is_some_and(|rest| {
                rest.starts_with(char::is_whitespace) && !rest.trim().is_empty()
            }) || line.contains('\0')
                || line.contains('\n')
            {
                return Err(source_failure(
                    "published compiler imports require complete import lines",
                ));
            }
            bytes
                .checked_add(line.len() as u64 + 32)
                .ok_or_else(source_size_failure)
        })?;
        let exports = match &declaration {
            Some(WorldDeclarationSelection::Joined(join)) => join.exports(),
            Some(WorldDeclarationSelection::Recovered(tip)) => tip.exports(),
            None => &[],
        };
        for export in exports {
            crate::host_work::checkpoint()?;
            bytes = bytes.checked_add(64).ok_or_else(source_size_failure)?;
            for name in std::iter::once(&export.head).chain(&export.children) {
                let text = name
                    .unit
                    .len()
                    .saturating_add(name.module.len())
                    .saturating_add(name.occurrence.len())
                    .saturating_add(name.record_parent.as_ref().map_or(0, String::len));
                bytes = bytes
                    .checked_add(text as u64 + 128)
                    .ok_or_else(source_size_failure)?;
            }
        }
        let allowance = capture_allowance()?;
        if bytes > allowance {
            return Err(binding_capture_refusal(bytes, allowance));
        }
        Ok(Self {
            import_lines: import_lines.into(),
            declaration,
            observation: None,
            metadata_bytes: bytes,
        })
    }

    pub fn with_observation(
        mut self,
        observation: Option<Arc<WorldObservationScaffold>>,
    ) -> Result<Self, CompileError> {
        let old = self
            .observation
            .as_ref()
            .map_or(0, |fact| fact.metadata_bytes);
        let new = observation.as_ref().map_or(0, |fact| fact.metadata_bytes);
        let bytes = self
            .metadata_bytes
            .checked_sub(old)
            .and_then(|bytes| bytes.checked_add(new))
            .ok_or_else(source_size_failure)?;
        let allowance = capture_allowance()?;
        if bytes > allowance {
            return Err(binding_capture_refusal(bytes, allowance));
        }
        self.observation = observation;
        self.metadata_bytes = bytes;
        Ok(self)
    }

    fn materialize<BS: fvm_ipld_blockstore::Blockstore>(
        &self,
        primary: &ExactDeclarationContext,
        materializer: &mut Materializer<'_, BS>,
    ) -> Result<cid::Cid, CompileError> {
        let declaration = match &self.declaration {
            None => ciborium::value::Value::Null,
            Some(selected) => {
                let (owner, expected_artifact, expected_interface, exports) = match selected {
                    WorldDeclarationSelection::Joined(join) => (
                        crate::declaration_join::ExactModuleIdentity {
                            unit: join.reserved().unit.clone(),
                            module: join.reserved().module.clone(),
                        },
                        Some(join.artifact_id()),
                        crate::checked_cell::hash(join.interface_bytes()),
                        join.exports(),
                    ),
                    WorldDeclarationSelection::Recovered(tip) => {
                        let roles = tip
                            .context()
                            .compiler_source_support()
                            .projection()
                            .snapshot_roles();
                        let interface = roles
                            .get(tip.root())
                            .ok_or_else(|| {
                                source_failure(
                                    "recovered declaration facade lacks its issuing primary role",
                                )
                            })?
                            .interface();
                        (
                            tip.root().clone(),
                            Some(interface),
                            tip.interface_sha256().to_owned(),
                            tip.exports(),
                        )
                    }
                };
                let roles = primary
                    .compiler_source_support()
                    .projection()
                    .snapshot_roles();
                let interface = roles
                    .get(&owner)
                    .ok_or_else(|| {
                        source_failure("selected declaration facade has no primary compiler role")
                    })?
                    .interface();
                if expected_artifact.is_some_and(|expected| expected != interface) {
                    return Err(source_failure(
                        "selected declaration facade has another primary interface",
                    ));
                }
                let payloads = primary
                    .compiler_source_support()
                    .artifacts()
                    .retained_payloads();
                let artifact = payloads.get(&interface).ok_or_else(|| {
                    source_failure(
                        "selected declaration facade lacks its primary interface custody",
                    )
                })?;
                if artifact.descriptor.owner != owner
                    || world_hex(&artifact.descriptor.interface_sha256) != expected_interface
                {
                    return Err(source_failure(
                        "selected declaration facade differs from its published interface",
                    ));
                }
                let encoded_exports = crate::declaration_join::encode_declaration_exports(exports);
                world_array(vec![
                    world_text(owner.unit),
                    world_text(owner.module),
                    world_blob(interface.0),
                    world_blob(encoded_exports),
                ])
            }
        };
        let lexical = world_array(vec![
            world_text("TPCWLEX2"),
            world_array(self.import_lines.iter().map(world_text).collect()),
            declaration,
            self.observation
                .as_ref()
                .map_or(ciborium::value::Value::Null, |scaffold| scaffold.value()),
        ]);
        let root = materializer
            .materialize_value(&lexical)
            .map_err(|error| source_failure(error.to_string()))?;
        Ok(root)
    }
}

#[derive(Clone)]
pub struct CompilerWorldOpenInputs {
    primary_context: Arc<ExactDeclarationContext>,
    resources: WorldInputResources,
}

#[derive(Clone)]
struct WorldInputResources {
    bindings: VisibleWorldBindings,
    sources: Arc<CapturedCompilerSources>,
    lexical: CompilerWorldLexicalSurface,
    directory: Arc<tempfile::TempDir>,
}

impl CompilerWorldOpenInputs {
    pub(crate) fn from_primary_selection(
        primary_context: Arc<ExactDeclarationContext>,
        bindings: VisibleWorldBindings,
        sources: Arc<CapturedCompilerSources>,
        lexical: CompilerWorldLexicalSurface,
        directory: Arc<tempfile::TempDir>,
    ) -> Result<Self, CompileError> {
        Ok(Self {
            primary_context,
            resources: WorldInputResources {
                bindings,
                sources,
                lexical,
                directory,
            },
        })
    }
}

pub struct CompilerWorldRestoration {
    logical_id: [u8; 32],
    inputs: WorldInputResources,
    product: Arc<[u8]>,
    compiler_input_identity: [u8; 32],
    primary_root: MaterializedContext,
    runtime_context_blocks: ReachableBlocks,
    primary_blocks: ReachableBlocks,
    primary_associations: MaterializedMap<crate::declaration_join::ExactModuleIdentity, cid::Cid>,
    primary_inputs: crate::declaration_context::WorldPrimaryInputs,
    bindings: MaterializedMap<String, WorldBindingSnapshotFact>,
    binding_rows: imbl::OrdMap<String, cid::Cid>,
    lexical_root: cid::Cid,
    producer: [u8; 32],
    profile: [u8; 32],
    descriptor: PathBuf,
    input_files: imbl::Vector<Arc<std::fs::File>>,
    successor: Option<WorldSuccessorAcquisition>,
}

#[derive(Clone, Debug)]
struct WorldRealizationObservation {
    reference: tidepool_extract_cmd::WorldRealizationRef,
    location: Option<tidepool_extract_cmd::CompilerWorkerLocation>,
    residency: tidepool_extract_cmd::WorldResidency,
}

impl WorldRealizationObservation {
    fn received(
        reference: tidepool_extract_cmd::WorldRealizationRef,
        location: Option<tidepool_extract_cmd::CompilerWorkerLocation>,
        residency: tidepool_extract_cmd::WorldResidency,
    ) -> Result<Self, CompileError> {
        if location
            .as_ref()
            .is_some_and(|actual| !reference.matches_location(actual))
        {
            return Err(world_response_error(
                CompilerResponseRejection::ProvenanceMismatch,
            ));
        }
        Ok(Self {
            reference,
            location,
            residency,
        })
    }
}

struct WorldSuccessorAcquisition {
    predecessor: [u8; 32],
    realization: Arc<Mutex<Option<WorldRealizationObservation>>>,
    files: Vec<Arc<std::fs::File>>,
    cold: Mutex<Option<Arc<CompilerWorldRestoration>>>,
}

impl fmt::Debug for CompilerWorldRestoration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompilerWorldRestoration")
            .field("logical_id", &self.logical_id)
            .field("compiler_input_identity", &self.compiler_input_identity)
            .field("primary_root", &self.primary_root)
            .field("primary_frontier_work", &self.primary_blocks.work())
            .field("primary_association_root", self.primary_associations.root())
            .field("binding_root", self.bindings.root())
            .finish_non_exhaustive()
    }
}

impl CompilerWorldRestoration {
    /// Rebuild only from this product's current immutable facts. The receiving
    /// compiler still decides whether a complete acquisition is needed.
    fn prepare_cold(&self) -> Result<Arc<Self>, CompileError> {
        let cold = self.inputs.clone().prepare_for(
            self.primary_inputs.clone(),
            self.producer,
            self.profile,
        )?;
        if cold.logical_id != self.logical_id
            || cold.compiler_input_identity != self.compiler_input_identity
        {
            return Err(source_failure(
                "successor cold restoration changed logical product",
            ));
        }
        Ok(cold)
    }

    #[cfg(test)]
    /// Diagnostic DATA only; this does not issue a World or admission.
    pub(crate) fn export_cold_observation_for_test(
        &self,
        destination: &Path,
    ) -> Result<(), CompileError> {
        self.prepare_cold()?
            .export_transport_observation_for_test(destination)
    }

    #[cfg(test)]
    /// Preserve the actual writer envelope and sealed arena bytes before their
    /// physical aliases retire. Reader controls may replace transport aliases.
    pub(crate) fn export_transport_observation_for_test(
        &self,
        destination: &Path,
    ) -> Result<(), CompileError> {
        use ciborium::value::Value;
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::FileExt;
        std::fs::create_dir_all(destination)?;
        let bytes = std::fs::read(&self.descriptor)?;
        let value: Value = ciborium::de::from_reader(bytes.as_slice())
            .map_err(|error| source_failure(error.to_string()))?;
        let fields = value
            .as_array()
            .ok_or_else(|| source_failure("observation descriptor is not an array"))?;
        let (kind, product, acquisition) = match fields.first().and_then(Value::as_text) {
            Some("TPCWORLD") if fields.len() == 11 => ("cold", &fields[2], &fields[7]),
            Some("TPCWSUCC5") if fields.len() == 6 => ("successor", &fields[1], &fields[5]),
            _ => return Err(source_failure("observation descriptor format")),
        };
        let product = product
            .as_bytes()
            .ok_or_else(|| source_failure("observation product bytes"))?;
        let acquisition = acquisition
            .as_array()
            .ok_or_else(|| source_failure("observation acquisition"))?;
        let arenas = acquisition
            .get(1)
            .and_then(Value::as_array)
            .ok_or_else(|| source_failure("observation acquisition arenas"))?;
        let mut remaining = capture_allowance()?;
        let mut exported = Vec::new();
        for (index, arena) in arenas.iter().enumerate() {
            let fields = arena
                .as_array()
                .ok_or_else(|| source_failure("observation arena"))?;
            let endpoint = fields
                .first()
                .and_then(Value::as_text)
                .ok_or_else(|| source_failure("observation arena endpoint"))?;
            let extent = fields
                .get(1)
                .and_then(Value::as_integer)
                .and_then(|value| u64::try_from(value).ok())
                .ok_or_else(|| source_failure("observation arena extent"))?;
            remaining = remaining
                .checked_sub(extent)
                .ok_or_else(source_size_failure)?;
            let file = self
                .input_files
                .iter()
                .find(|file| {
                    endpoint == format!("/proc/{}/fd/{}", std::process::id(), file.as_raw_fd())
                })
                .ok_or_else(|| {
                    source_failure("observation arena has no actual retained file owner")
                })?;
            if file.metadata()?.len() != extent {
                return Err(source_failure("observation arena extent changed"));
            }
            let mut body = vec![0; usize::try_from(extent).map_err(|_| source_size_failure())?];
            file.read_exact_at(&mut body, 0)?;
            let name = format!("arena-{index}.bin");
            std::fs::write(destination.join(&name), &body)?;
            exported.push(
                serde_json::json!({"index":index,"original_endpoint":endpoint,
                "file":name,"extent":extent,"sha256":world_hex(&world_digest(&body))}),
            );
        }
        std::fs::write(destination.join("descriptor.cbor"), &bytes)?;
        if kind == "cold" {
            std::fs::write(
                destination.join("sources.bin"),
                self.inputs.sources.encoded(),
            )?;
        }
        std::fs::write(destination.join("observation.json"), serde_json::to_vec_pretty(&serde_json::json!({
            "schema":1,"kind":kind,"authority":false,"logical_id":world_hex(&self.logical_id),
            "product_sha256":world_hex(&world_digest(product)),
            "descriptor_file":"descriptor.cbor","descriptor_sha256":world_hex(&world_digest(&bytes)),
            "sources_file":(kind == "cold").then_some("sources.bin"),"arenas":exported,
        })).map_err(|error| source_failure(error.to_string()))?)?;
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn input_directory_for_test(&self) -> &Arc<tempfile::TempDir> {
        &self.inputs.directory
    }

    pub(crate) fn captured_source_domain(&self) -> &[u8; 32] {
        self.inputs.sources.seal()
    }

    pub fn logical_id(&self) -> &[u8; 32] {
        &self.logical_id
    }

    pub fn lexical_root(&self) -> &cid::Cid {
        &self.lexical_root
    }

    /// Occurrence identities of the restored binding table. A restored world
    /// keeps only snapshot facts, not the runtime's checked rows, so this
    /// yields exactly what enumeration may answer without a compiler call.
    pub fn binding_names(&self) -> impl Iterator<Item = WorldBindingName> + '_ {
        self.bindings
            .facts()
            .iter()
            .map(|(occurrence, fact)| WorldBindingName {
                occurrence: occurrence.clone(),
                unit: fact.0 .0.clone(),
                module: fact.0 .1.clone(),
                namespace: fact.0 .2.clone(),
            })
    }
}

/// Transport aliases for one immutable captured namespace. Filesystem paths
/// are receiving labels only: source bytes and negative facts belong to the
/// captured roots, even after the worker materialization has retired.
pub(crate) struct WorldSourcePathDomain<'a> {
    sources: &'a CapturedCompilerSources,
    aliases: &'a [PathBuf],
    operation: &'a tidepool_repr::execution_schema::InventoryOperation,
}

impl<'a> WorldSourcePathDomain<'a> {
    fn new(
        sources: &'a CapturedCompilerSources,
        aliases: &'a [PathBuf],
        operation: &'a tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<Self, CompileError> {
        if aliases.len() != sources.roots.len() {
            return Err(source_failure("world source root alias count differs"));
        }
        operation
            .reserve::<[&PathBuf; 2]>(aliases.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        for alias in aliases {
            crate::host_work::checkpoint()?;
            operation
                .charge(alias.as_os_str().len())
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            if !absolute_source_alias(alias) {
                return Err(source_failure("world source root alias is not absolute"));
            }
        }
        let mut ordered = aliases.iter().collect::<Vec<_>>();
        ordered.sort_unstable();
        if ordered.windows(2).any(|pair| pair[1].starts_with(pair[0])) {
            return Err(source_failure("world source root aliases are ambiguous"));
        }
        Ok(Self {
            sources,
            aliases,
            operation,
        })
    }

    pub(crate) fn aliases(&self) -> &[PathBuf] {
        self.aliases
    }

    fn relative<'p>(&self, path: &'p Path) -> Result<(usize, &'p Path), CompileError> {
        if !absolute_source_alias(path) {
            return Err(source_failure("world source path leaves its root alias"));
        }
        for (index, alias) in self.aliases.iter().enumerate() {
            crate::host_work::checkpoint()?;
            self.operation
                .charge(1)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            if let Ok(relative) = path.strip_prefix(alias) {
                if !relative.as_os_str().is_empty() {
                    return Ok((index, relative));
                }
            }
        }
        Err(source_failure(
            "world source path has no captured root alias",
        ))
    }

    pub(crate) fn selected_source(
        &self,
        module: &str,
        path: &Path,
    ) -> Result<([u8; 32], usize), CompileError> {
        let (index, relative) = self.relative(path)?;
        match self.sources.selected_sources.get(module) {
            Some(WorldModuleOrigin::Home {
                root,
                path,
                source_sha256,
            }) if *root as usize == index && Path::new(path) == relative => {
                let body = self.sources.roots[index]
                    .bodies
                    .get(relative)
                    .ok_or_else(|| source_failure("captured source selection has no body"))?;
                Ok((*source_sha256, body.len()))
            }
            _ => Err(source_failure(
                "world source selected another captured root or file",
            )),
        }
    }

    pub(crate) fn require_absent(&self, path: &Path) -> Result<(), CompileError> {
        let (index, relative) = self.relative(path)?;
        if self.sources.roots[index].bodies.contains_key(relative) {
            return Err(source_failure("world source negative candidate is present"));
        }
        Ok(())
    }
}

fn absolute_source_alias(path: &Path) -> bool {
    path.is_absolute()
        && !path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
}

/// One issued compiler World. Observations retain this shared owner; only a
/// checked successor or explicit restoration creates another World.
#[derive(Debug)]
pub struct CompilerWorld {
    restoration: Arc<CompilerWorldRestoration>,
    realization: Arc<Mutex<Option<WorldRealizationObservation>>>,
    position: Option<WorldCellPosition>,
}

/// The completed compiler progress a private execution can carry into public
/// promotion. Changed branches retain their issuing settled frontier; promotion
/// authenticates its exact kind and lineage. Runtime does not infer progress
/// from tool names, absent positions or pointer identity.
#[derive(Clone, Debug)]
pub enum CompletedPrivateCompilerProgress {
    /// The private World is the exact borrowed public product with no compiler
    /// additions. Publication keeps the latest public World instead of
    /// restoring the older borrowed base.
    Unchanged,
    /// A real completed checked-cell successor was issued by the cell owner.
    /// The existing checked promotion path validates its full lineage.
    Checked(Arc<PrivateSettledFrontier>),
    /// Accepted visible host bindings issued a real compiler row and their
    /// named facts were projected into the private World. No checked item
    /// completed for this invocation.
    HostChanged(Arc<PrivateSettledFrontier>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivatePublicationDiagnostic {
    pub same_product: bool,
    pub position_present: bool,
    pub fences_match: bool,
    pub original_base_matches: bool,
    pub position_complete: bool,
    pub host_matches: bool,
}

/// Runtime-observed lifecycle of the checked item belonging to one private
/// execution. The runtime moves this only at the actual prefix start and
/// settlement boundaries; the compiler still validates the resulting World.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivateCheckedItemProgress {
    NotStarted,
    Started,
    Completed,
}

#[derive(Clone, Debug)]
struct WorldCellPosition {
    original_base: [u8; 32],
    certificate: [u8; 32],
    next_item: usize,
    total_items: usize,
}

impl CompilerWorld {
    pub fn binding_scope(&self) -> u64 {
        self.restoration.inputs.bindings.query_scope
    }

    pub fn logical_id(&self) -> &[u8; 32] {
        &self.restoration.logical_id
    }
    /// Begin a private lineage at this exact borrowed World. This cannot pair
    /// a caller-selected successor with a different birth World.
    pub fn private_frontier(self: &Arc<Self>) -> Arc<PrivateSettledFrontier> {
        Arc::new(PrivateSettledFrontier {
            birth: *self.logical_id(),
            world: self.clone(),
            kind: PrivateSettledKind::Borrowed,
        })
    }

    pub fn diagnose_private_publication(
        private: &Arc<Self>,
        original_public: &Arc<Self>,
        host: Option<Arc<PrivateHostBindingProgress>>,
    ) -> PrivatePublicationDiagnostic {
        let fences_match = |candidate: &Self, base: &Self| {
            candidate.restoration.producer == base.restoration.producer
                && candidate.restoration.profile == base.restoration.profile
                && candidate.restoration.inputs.sources.seal()
                    == base.restoration.inputs.sources.seal()
        };
        let same_product = private.logical_id() == original_public.logical_id();
        let (position_present, original_base_matches, complete) = match private.position.as_ref() {
            Some(position) => (
                true,
                position.original_base == *original_public.logical_id(),
                position.next_item == position.total_items,
            ),
            None => (false, false, false),
        };
        let host_matches = host.as_ref().is_none_or(|host| {
            host.bindings.iter().all(|binding| {
                private
                    .restoration
                    .inputs
                    .bindings
                    .entries
                    .get(&binding.occurrence)
                    .is_some_and(|fact| {
                        fact.symbol == binding.symbol
                            && fact.session_var == binding.session_var
                            && fact.defining_scope == binding.defining_scope
                            && Arc::ptr_eq(&fact.row, &binding.row)
                    })
            })
        });
        PrivatePublicationDiagnostic {
            same_product,
            position_present,
            fences_match: fences_match(private, original_public),
            original_base_matches,
            position_complete: complete,
            host_matches,
        }
    }
    /// The last actual Ready admission's optional storage observation. This
    /// is neither current residency nor a lease and may be stale after END.
    pub fn last_admission_residency(
        &self,
    ) -> Result<Option<tidepool_extract_cmd::WorldResidency>, CompileError> {
        Ok(self
            .realization
            .lock()
            .map_err(|_| source_failure("world realization lock poisoned"))?
            .as_ref()
            .map(|observed| observed.residency))
    }

    pub fn restoration(&self) -> &Arc<CompilerWorldRestoration> {
        &self.restoration
    }

    /// Admit the original TYPE owners and protected lexical scaffold as one
    /// derived product of this held predecessor.
    pub fn with_observation_scaffold(
        self: &Arc<Self>,
        observation: Option<Arc<WorldObservationScaffold>>,
    ) -> Result<Arc<Self>, CompileError> {
        let lexical = self
            .restoration
            .inputs
            .lexical
            .clone()
            .with_observation(observation)?;
        self.with_primary_and_lexical(lexical.observation_type_interfaces(), Some(&lexical))
    }
}

/// Immutable certificate of the current settled private lineage. Runtime owns
/// its replacement and the active lease; this value retains no history graph.
pub struct PrivateSettledFrontier {
    birth: [u8; 32],
    world: Arc<CompilerWorld>,
    kind: PrivateSettledKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrivateSettledKind {
    Borrowed,
    Host,
    Checked,
}

impl std::fmt::Debug for PrivateSettledFrontier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PrivateSettledFrontier")
            .field("birth", &self.birth)
            .field("current", self.world.logical_id())
            .field("kind", &self.kind)
            .finish()
    }
}

impl PrivateSettledFrontier {
    pub fn matches_world(&self, world: &Arc<CompilerWorld>) -> bool {
        Arc::ptr_eq(&self.world, world)
    }

    pub fn publication(self: &Arc<Self>) -> CompletedPrivateCompilerProgress {
        match self.kind {
            PrivateSettledKind::Borrowed => CompletedPrivateCompilerProgress::Unchanged,
            PrivateSettledKind::Host => CompletedPrivateCompilerProgress::HostChanged(self.clone()),
            PrivateSettledKind::Checked => CompletedPrivateCompilerProgress::Checked(self.clone()),
        }
    }

    /// Validate this program's terminal independently of its runtime lifecycle.
    /// Runtime may commit this candidate only with the same active program.
    pub fn complete_program(
        &self,
        program: &crate::checked_cell::CellProgram,
        successor: &CheckedWorldSuccessor,
    ) -> Result<Arc<Self>, CompileError> {
        let crate::checked_cell::CellProgramBasis::World(output) = program.basis() else {
            return Err(source_failure("private frontier requires a World program"));
        };
        let position = successor.world.position.as_ref().ok_or_else(|| {
            source_failure("private frontier requires its completed program cursor")
        })?;
        if self.world.logical_id() != output.logical().logical_id()
            || position.original_base != *self.world.logical_id()
            || position.certificate != *output.certificate_sha256()
            || position.total_items != program.items().len()
            || position.next_item != position.total_items
            || successor.world.restoration.producer != self.world.restoration.producer
            || successor.world.restoration.profile != self.world.restoration.profile
            || successor.world.restoration.inputs.sources.seal()
                != self.world.restoration.inputs.sources.seal()
        {
            return Err(source_failure(
                "private terminal belongs to another frontier or program",
            ));
        }
        Ok(Arc::new(Self {
            birth: self.birth,
            world: successor.world.clone(),
            kind: PrivateSettledKind::Checked,
        }))
    }

    pub fn complete_empty_program(
        &self,
        program: &crate::checked_cell::CellProgram,
    ) -> Result<Arc<Self>, CompileError> {
        let crate::checked_cell::CellProgramBasis::World(output) = program.basis() else {
            return Err(source_failure("private frontier requires a World program"));
        };
        if !program.items().is_empty()
            || self.world.logical_id() != output.logical().logical_id()
            || self.world.restoration.producer != *output.producer_sha256()
            || self.world.restoration.profile != output.logical().profile_sha256
        {
            return Err(source_failure(
                "empty terminal belongs to another private frontier",
            ));
        }
        Ok(Arc::new(Self {
            birth: self.birth,
            world: self.world.clone(),
            kind: PrivateSettledKind::Checked,
        }))
    }

    pub fn apply_host_successor(
        &self,
        successor: &CompilerWorldHostSuccessor,
    ) -> Result<Arc<Self>, CompileError> {
        if !self.matches_world(&successor.predecessor) {
            return Err(source_failure(
                "host successor belongs to another private frontier",
            ));
        }
        Ok(Arc::new(Self {
            birth: self.birth,
            world: successor.world.clone(),
            kind: match self.kind {
                PrivateSettledKind::Checked => PrivateSettledKind::Checked,
                _ if !successor.progress.is_empty() => PrivateSettledKind::Host,
                _ => self.kind,
            },
        }))
    }

    fn validate_publication(
        &self,
        birth: &Arc<CompilerWorld>,
        current: &Arc<CompilerWorld>,
        expected: PrivateSettledKind,
    ) -> Result<(), CompileError> {
        if self.birth != *birth.logical_id()
            || !self.matches_world(current)
            || self.kind != expected
        {
            return Err(source_failure(
                "private publication has another settled frontier",
            ));
        }
        Ok(())
    }
}

/// A successful checked item's next compiler state. The context is borrowed
/// from the same World, so callers cannot pair a warm World with older cold
/// restoration facts. Only checked item advancement issues this owner.
pub struct CheckedWorldSuccessor {
    world: Arc<CompilerWorld>,
}

impl CheckedWorldSuccessor {
    pub fn world(&self) -> &Arc<CompilerWorld> {
        &self.world
    }

    pub fn compiler_context(&self) -> &Arc<ExactDeclarationContext> {
        self.world.restoration.primary_inputs.context()
    }
}

impl crate::checked_cell::CellProgram {
    /// Advance a declaration only from its actual checked item and the
    /// authenticated cumulative facade prepared for that item.
    pub fn declaration_item_successor<'a>(
        self: &Arc<Self>,
        predecessor: &Arc<CompilerWorld>,
        item: &crate::checked_cell::ExactCheckedItem,
        publication: &crate::declaration_join::PublicationComposition,
        import_lines: Vec<String>,
        visible_changes: impl IntoIterator<Item = (&'a str, Option<CompilerWorldBinding<'a>>)>,
    ) -> Result<CheckedWorldSuccessor, CompileError> {
        let joined = publication.join();
        let projection_context = publication.context();
        let crate::checked_cell::CellProgramBasis::World(program) = self.basis() else {
            return Err(source_failure("finite declaration has no World successor"));
        };
        let ordinal = item.index();
        let issued = self
            .items()
            .get(ordinal)
            .filter(|issued| issued.checked_item() == item)
            .ok_or_else(|| source_failure("declaration belongs to another checked output"))?;
        let observed = program
            .item(ordinal)
            .ok_or_else(|| source_failure("declaration lacks its captured World proof"))?;
        let ordered = if ordinal == 0 {
            predecessor.logical_id() == program.logical().logical_id()
        } else {
            predecessor.position.as_ref().is_some_and(|position| {
                position.original_base == *program.logical().logical_id()
                    && position.certificate == *program.certificate_sha256()
                    && position.next_item == ordinal
                    && position.total_items == program.items().len()
            })
        };
        let authored = item
            .planned_declaration()
            .ok_or_else(|| source_failure("World successor is not a declaration item"))?;
        if !ordered
            || predecessor.restoration.producer != *program.producer_sha256()
            || predecessor.restoration.profile != program.logical.profile_sha256
            || authored.product().owner().unit != "main"
            || authored.product().owner().module
                != tidepool_repr::SessionModule::lib(tidepool_repr::Generation(
                    observed.generation(),
                ))
                .module_name()
        {
            return Err(source_failure(
                "declaration successor has a foreign checked prefix",
            ));
        }
        let product_owner = authored.product().owner();
        let owner = crate::declaration_join::ExactModuleIdentity {
            unit: product_owner.unit.clone(),
            module: product_owner.module.clone(),
        };
        let expected = authored
            .compiler_source_support()
            .world_primary_type_row(&owner)
            .ok_or_else(|| source_failure("authored item lacks its issued primary binding"))?;
        // The accepted join retains the exact authored input selection. Its
        // public projection may select only that owner's canonical interface.
        let input = joined
            .context()
            .compiler_source_support()
            .world_primary_type_row(&owner)
            .ok_or_else(|| source_failure("declaration join omits the actual authored input"))?;
        if input.interface_binding != expected.interface_binding
            || input.original_binding != expected.original_binding
        {
            return Err(source_failure(
                "declaration projection substitutes its authored binding",
            ));
        }
        let actual = projection_context
            .compiler_source_support()
            .world_primary_type_row(&owner)
            .ok_or_else(|| {
                source_failure("declaration projection omits the actual authored owner")
            })?;
        if actual.interface_binding != expected.interface_binding {
            return Err(source_failure(
                "declaration projection substitutes its authored interface",
            ));
        }
        let declaration = predecessor
            .restoration
            .primary_inputs
            .with_declaration_projection(publication)?;
        let advance = declaration.with_native_completion_from(issued.compiler_context())?;
        tracing::debug!(target: "exomonad_harness::timing",
            promotion_qualifier_invocations = advance.promotion_qualifier_invocations,
            changed_primary_owners = advance.changed_owners.len(),
            "World declaration support admitted");
        let lexical = CompilerWorldLexicalSurface::from_import_lines(
            import_lines,
            Some(WorldDeclarationSelection::Joined(joined.clone())),
        )?
        .with_observation(predecessor.restoration.inputs.lexical.observation.clone())?;
        let mut changes = BTreeMap::new();
        let mut added = Vec::new();
        for (name, binding) in visible_changes {
            if name.is_empty() || name.contains('\0') || changes.contains_key(name) {
                return Err(source_failure("invalid declaration binding delta"));
            }
            if let Some(binding) = binding {
                if binding.occurrence != name {
                    return Err(source_failure("declaration winner has another occurrence"));
                }
                added.push(binding);
                changes.insert(name.to_owned(), true);
            } else {
                changes.insert(name.to_owned(), false);
            }
        }
        let mut additions = VisibleWorldBindings::from_published_bindings(
            predecessor.restoration.inputs.bindings.query_scope,
            added,
        )?;
        let changes = changes.into_iter().map(|(name, present)| {
            let fact = present.then(|| {
                additions
                    .entries
                    .remove(&name)
                    .expect("validated declaration winner")
            });
            (name, fact)
        });
        let world = predecessor.project_bindings_with_primary(
            predecessor.restoration.inputs.bindings.query_scope,
            changes,
            Some(WorldCellPosition {
                original_base: *program.logical().logical_id(),
                certificate: *program.certificate_sha256(),
                next_item: ordinal + 1,
                total_items: program.items().len(),
            }),
            Some(WorldPrimaryAdvance::Declaration { advance, lexical }),
        )?;
        Ok(CheckedWorldSuccessor { world })
    }
    /// Issue the compiler facts of one successful runtime prefix. The caller
    /// supplies descriptions of the actual installed delta; this owner checks
    /// them against the same captured item and retains its type/native custody.
    /// Runtime source admission and atomic installation remain runtime-owned.
    pub fn item_successor<'a>(
        self: &Arc<Self>,
        predecessor: &Arc<CompilerWorld>,
        item: &crate::checked_cell::ExactCheckedItem,
        bindings: impl IntoIterator<Item = CompilerWorldBinding<'a>>,
    ) -> Result<CheckedWorldSuccessor, CompileError> {
        let crate::checked_cell::CellProgramBasis::World(program) = self.basis() else {
            return Err(source_failure(
                "finite cell has no compiler-world successor",
            ));
        };
        let ordinal = item.index();
        let issued = self
            .items()
            .get(ordinal)
            .filter(|issued| issued.checked_item() == item)
            .ok_or_else(|| source_failure("successor item belongs to another checked output"))?;
        let observed = program
            .item(ordinal)
            .ok_or_else(|| source_failure("successor item has no captured World proof"))?;
        let ordered = if ordinal == 0 {
            predecessor.logical_id() == program.logical().logical_id()
        } else {
            predecessor.position.as_ref().is_some_and(|position| {
                position.original_base == *program.logical().logical_id()
                    && position.certificate == *program.certificate_sha256()
                    && position.next_item == ordinal
                    && position.total_items == program.items().len()
            })
        };
        if !ordered
            || predecessor.restoration.producer != *program.producer_sha256()
            || predecessor.restoration.profile != program.logical.profile_sha256
        {
            return Err(source_failure(
                "successor predecessor is not this item's checked prefix",
            ));
        }
        let native = issued.native().ok_or_else(|| {
            source_failure("declaration successor requires its checked lexical product")
        })?;
        let row = native.value_interface_certificate();
        let additions = VisibleWorldBindings::from_published_bindings(
            predecessor.restoration.inputs.bindings.query_scope,
            bindings,
        )?;
        if additions.entries.len() != observed.binders().len() {
            return Err(source_failure(
                "runtime delta does not cover this item's captured binders",
            ));
        }
        for binder in observed.binders() {
            let fact = additions
                .entries
                .get(binder.name())
                .ok_or_else(|| source_failure("runtime delta omits a captured binder"))?;
            if fact.session_var.raw() != binder.varid()
                || fact.symbol.module != binder.module()
                || fact.symbol.occurrence != binder.name()
                || fact.symbol.namespace != "value"
                || fact.symbol.record_parent.is_some()
                || fact.row.owner().gen.0 != observed.generation()
                || row
                    .as_ref()
                    .is_none_or(|row| !row.same_world_type_row(&fact.row))
            {
                return Err(source_failure(
                    "runtime delta has another captured binder or row",
                ));
            }
        }
        let advance = predecessor
            .restoration
            .primary_inputs
            .with_native_completion_from(issued.compiler_context())?;
        let world = predecessor.project_bindings_with_primary(
            predecessor.restoration.inputs.bindings.query_scope,
            additions
                .entries
                .into_iter()
                .map(|(name, fact)| (name, Some(fact))),
            Some(WorldCellPosition {
                original_base: *program.logical().logical_id(),
                certificate: *program.certificate_sha256(),
                next_item: ordinal + 1,
                total_items: program.items().len(),
            }),
            Some(WorldPrimaryAdvance::NativeCompletion(advance)),
        )?;
        Ok(CheckedWorldSuccessor { world })
    }
}

/// Runtime completion can advance without changing the compiler namespace.
/// Only a declaration also replaces the lexical surface.
enum WorldPrimaryAdvance {
    NativeCompletion(crate::declaration_context::WorldPrimaryProgramSupport),
    Declaration {
        advance: crate::declaration_context::WorldPrimaryProgramSupport,
        lexical: CompilerWorldLexicalSurface,
    },
}

impl WorldPrimaryAdvance {
    fn support(&self) -> &crate::declaration_context::WorldPrimaryProgramSupport {
        match self {
            Self::NativeCompletion(advance) | Self::Declaration { advance, .. } => advance,
        }
    }

    fn lexical(&self) -> Option<&CompilerWorldLexicalSurface> {
        match self {
            Self::NativeCompletion(_) => None,
            Self::Declaration { lexical, .. } => Some(lexical),
        }
    }
}

/// A declaration delta prepared against one exact public World owner.
/// Runtime still authenticates its publication ticket and owns the commit.
pub struct CompilerWorldDeclarationPublication {
    base: Arc<CompilerWorld>,
    advance: crate::declaration_context::WorldPrimaryProgramSupport,
    lexical: CompilerWorldLexicalSurface,
}

impl CompilerWorld {
    /// Prepare the common joined publication before the runtime commit claim.
    /// The primary owner validates the facade's exact issued binding and keeps
    /// the coherent context and inputs together in the returned product.
    pub fn prepare_declaration_publication(
        self: &Arc<Self>,
        publication: &crate::declaration_join::PublicationComposition,
        import_lines: Vec<String>,
    ) -> Result<CompilerWorldDeclarationPublication, CompileError> {
        let joined = publication.join();
        let advance = self
            .restoration
            .primary_inputs
            .with_declaration_projection(publication)?;
        let lexical = CompilerWorldLexicalSurface::from_import_lines(
            import_lines,
            Some(WorldDeclarationSelection::Joined(joined.clone())),
        )?
        .with_observation(self.restoration.inputs.lexical.observation.clone())?;
        Ok(CompilerWorldDeclarationPublication {
            base: self.clone(),
            advance,
            lexical,
        })
    }

    /// Project a completed checked execution onto the latest public compiler
    /// product. Runtime still owns source gates, binding publication and the
    /// atomic installation of this prospective immutable product.
    pub fn promote_private_bindings<'a>(
        self: &Arc<Self>,
        original_public: &Arc<Self>,
        private: &Arc<Self>,
        target_scope: u64,
        progress: &CompletedPrivateCompilerProgress,
        declaration: Option<CompilerWorldDeclarationPublication>,
        visible_changes: impl IntoIterator<Item = (&'a str, Option<(tidepool_repr::SessionVarId, u64)>)>,
    ) -> Result<Arc<Self>, CompileError> {
        let checked_prefix = match progress {
            CompletedPrivateCompilerProgress::Checked(frontier) => {
                frontier.validate_publication(
                    original_public,
                    private,
                    PrivateSettledKind::Checked,
                )?;
                true
            }
            CompletedPrivateCompilerProgress::HostChanged(frontier) => {
                frontier.validate_publication(
                    original_public,
                    private,
                    PrivateSettledKind::Host,
                )?;
                false
            }
            CompletedPrivateCompilerProgress::Unchanged => {
                return Err(source_failure(
                    "private publication progress is not promotable",
                ));
            }
        };
        if private.restoration.producer != original_public.restoration.producer
            || private.restoration.profile != original_public.restoration.profile
            || self.restoration.producer != private.restoration.producer
            || self.restoration.profile != private.restoration.profile
            || original_public.restoration.inputs.sources.seal()
                != private.restoration.inputs.sources.seal()
            || self.restoration.inputs.sources.seal() != private.restoration.inputs.sources.seal()
        {
            return Err(source_failure(
                "private publication has an incomplete or foreign checked lineage",
            ));
        }
        self.validate_private_promotion(original_public, private, target_scope)?;
        let declaration = declaration
            .map(|declaration| {
                if !checked_prefix {
                    return Err(source_failure(
                        "host-only publication cannot carry a declaration successor",
                    ));
                }
                if !Arc::ptr_eq(&declaration.base, self) {
                    return Err(source_failure(
                        "declaration publication belongs to another World",
                    ));
                }
                Ok(WorldPrimaryAdvance::Declaration {
                    advance: declaration.advance.with_native_completion_from(
                        private.restoration.primary_inputs.context(),
                    )?,
                    lexical: declaration.lexical,
                })
            })
            .transpose()?
            .map_or_else(
                || {
                    self.restoration
                        .primary_inputs
                        .with_native_completion_from(private.restoration.primary_inputs.context())
                        .map(WorldPrimaryAdvance::NativeCompletion)
                },
                Ok,
            )?;
        let changes = Self::private_binding_changes(private, self, visible_changes)?;
        self.project_bindings_with_primary(target_scope, changes, None, Some(declaration))
    }

    fn validate_private_promotion(
        self: &Arc<Self>,
        original_public: &Arc<Self>,
        private: &Arc<Self>,
        target_scope: u64,
    ) -> Result<(), CompileError> {
        if private.restoration.producer != original_public.restoration.producer
            || private.restoration.profile != original_public.restoration.profile
            || self.restoration.producer != private.restoration.producer
            || self.restoration.profile != private.restoration.profile
            || original_public.restoration.inputs.sources.seal()
                != private.restoration.inputs.sources.seal()
            || self.restoration.inputs.sources.seal() != private.restoration.inputs.sources.seal()
        {
            return Err(source_failure(
                "private publication has an incomplete or foreign checked lineage",
            ));
        }
        if self.restoration.inputs.bindings.query_scope != target_scope {
            return Err(source_failure(
                "public compiler projection belongs to another scope",
            ));
        }
        Ok(())
    }

    fn private_binding_changes<'a>(
        private: &Arc<Self>,
        latest: &Arc<Self>,
        visible_changes: impl IntoIterator<Item = (&'a str, Option<(tidepool_repr::SessionVarId, u64)>)>,
    ) -> Result<BTreeMap<String, Option<WorldBindingFact>>, CompileError> {
        let mut changes = BTreeMap::new();
        for (name, binding) in visible_changes {
            crate::host_work::checkpoint()?;
            if name.is_empty() || name.contains('\0') || changes.contains_key(name) {
                return Err(source_failure("invalid or duplicate promotion occurrence"));
            }
            let fact = match binding {
                None => None,
                Some((id, defining_scope)) => {
                    let matches = |fact: &&WorldBindingFact| {
                        fact.session_var == id && fact.defining_scope == defining_scope
                    };
                    let fact = private
                        .restoration
                        .inputs
                        .bindings
                        .entries
                        .get(name)
                        .filter(matches)
                        .or_else(|| {
                            latest
                                .restoration
                                .inputs
                                .bindings
                                .entries
                                .get(name)
                                .filter(matches)
                        })
                        .ok_or_else(|| {
                            source_failure("runtime promotion has no same issued compiler binding")
                        })?;
                    Some(fact.clone())
                }
            };
            changes.insert(name.to_owned(), fact);
        }
        Ok(changes)
    }

    /// Project accepted visible host rows into a private World. Each row is
    /// issued by the host interface that installed the runtime binding; this
    /// operation preserves any completed checked prefix and its position.
    pub fn with_visible_host_bindings<'a>(
        self: &Arc<Self>,
        target_scope: u64,
        bindings: impl IntoIterator<Item = CompilerWorldHostBinding<'a>>,
    ) -> Result<CompilerWorldHostSuccessor, CompileError> {
        self.with_visible_host_bindings_resuming(target_scope, bindings, None)
    }

    /// Extend one private World with accepted host rows while carrying the
    /// exact host-binding progress already applied to that same lineage.
    pub fn with_visible_host_bindings_resuming<'a>(
        self: &Arc<Self>,
        target_scope: u64,
        bindings: impl IntoIterator<Item = CompilerWorldHostBinding<'a>>,
        prior_progress: Option<Arc<PrivateHostBindingProgress>>,
    ) -> Result<CompilerWorldHostSuccessor, CompileError> {
        let mut progress = Vec::new();
        let mut compiler_bindings = Vec::new();
        for binding in bindings {
            let row = binding.row()?;
            progress.push(PrivateHostBindingProgressBinding {
                occurrence: binding.occurrence.to_owned(),
                symbol: binding.symbol.clone(),
                session_var: binding.session_var,
                defining_scope: binding.defining_scope,
                row: row.clone(),
                kind: binding.kind(),
            });
            compiler_bindings.push(CompilerWorldBinding::from_checked_value(
                binding.occurrence,
                binding.symbol,
                binding.session_var,
                binding.defining_scope,
                row,
            ));
        }
        let additions = VisibleWorldBindings::from_published_bindings(
            self.restoration.inputs.bindings.query_scope,
            compiler_bindings,
        )?;
        if additions.entries.is_empty() {
            let extension = Arc::new(PrivateHostBindingProgress { bindings: progress });
            let progress = match prior_progress {
                Some(prior) => PrivateHostBindingProgress::resumed_from(&prior, &extension)?,
                None => extension,
            };
            return Ok(CompilerWorldHostSuccessor {
                predecessor: self.clone(),
                world: self.clone(),
                progress,
            });
        }
        for (occurrence, fact) in &additions.entries {
            if fact.symbol.occurrence != occurrence.as_str()
                || fact.row.world_issuer().0 != self.restoration.producer
            {
                return Err(source_failure(
                    "visible host binding has a foreign compiler issuer",
                ));
            }
        }
        let world = self.project_bindings_with_primary(
            target_scope,
            additions
                .entries
                .into_iter()
                .map(|(occurrence, fact)| (occurrence, Some(fact))),
            self.position.clone(),
            None,
        )?;
        let extension = Arc::new(PrivateHostBindingProgress { bindings: progress });
        let progress = prior_progress
            .map(|prior| PrivateHostBindingProgress::resumed_from(&prior, &extension))
            .transpose()?
            .unwrap_or(extension);
        Ok(CompilerWorldHostSuccessor {
            predecessor: self.clone(),
            world,
            progress,
        })
    }

    /// Extend the held primary with only canonical TYPE roles selected by
    /// the original signature owners. Native choices and lexical source
    /// authority remain with the existing primary owner.
    pub fn with_type_interfaces(
        self: &Arc<Self>,
        contexts: &[Arc<crate::declaration_context::SelectedTypeInterfaces>],
    ) -> Result<Arc<Self>, CompileError> {
        self.with_primary_and_lexical(contexts, None)
    }

    fn with_primary_and_lexical(
        self: &Arc<Self>,
        contexts: &[Arc<crate::declaration_context::SelectedTypeInterfaces>],
        lexical: Option<&CompilerWorldLexicalSurface>,
    ) -> Result<Arc<Self>, CompileError> {
        let prior = &self.restoration;
        let advance = prior.primary_inputs.with_type_interfaces(contexts)?;
        let lexical_changed =
            lexical.is_some_and(|lexical| prior.inputs.lexical.observation != lexical.observation);
        if advance.changed_owners.is_empty() && !lexical_changed {
            return Ok(self.clone());
        }
        let mut inputs = prior.inputs.clone();
        let primary_inputs = advance.primary.clone();
        if let Some(lexical) = lexical {
            inputs.lexical = lexical.clone();
        }
        let directory = Arc::new(crate::artifacts::compiler_scratch_directory()?);
        inputs.directory = directory.clone();
        let mut rows = WorldColdRows {
            producer: prior.producer,
            files: BTreeMap::new(),
            images: BTreeMap::new(),
            rows: BTreeMap::new(),
            arenas: Default::default(),
            diagnostics: None,
        };
        let mut runtime_materializer = Materializer::new(&prior.runtime_context_blocks);
        let primary_root = runtime_materializer
            .materialize_context(
                Some(&prior.primary_root),
                &primary_inputs.context().snapshot(),
            )
            .map_err(|error| source_failure(error.to_string()))?;
        let runtime_staged = runtime_materializer
            .finish()
            .map_err(|error| source_failure(error.to_string()))?;
        let runtime_context_blocks = prior
            .runtime_context_blocks
            .replace_roots(&runtime_staged, vec![*primary_root.root().cid()])
            .map_err(|error| source_failure(error.to_string()))?;
        let mut materializer = Materializer::new(&prior.primary_blocks);
        let mut association_facts = prior.primary_associations.facts().clone();
        let mut primary_changes = Vec::new();
        let mut remaining_raw = capture_allowance()?;
        let support = advance.primary.inputs().compiler_source_support();
        for (ordinal, owner) in advance.changed_owners.iter().enumerate() {
            let selected = support
                .compiler_primary_input(owner)?
                .ok_or_else(|| source_failure("TYPE delta omits its issued primary row"))?;
            if selected.original().is_some() {
                return Err(source_failure("TYPE delta cannot select an implementation"));
            }
            let (row, fact) = materialize_primary_type_row(
                &selected,
                &prior.producer,
                directory.path(),
                ordinal,
                &mut rows,
                &mut materializer,
                &mut remaining_raw,
            )?;
            if association_facts
                .insert(selected.owner().clone(), fact)
                .is_some_and(|old| old != fact)
            {
                return Err(source_failure(
                    "TYPE delta changes an existing exact primary association",
                ));
            }
            primary_changes.push(world_array(vec![
                world_text(owner.unit.clone()),
                world_text(owner.module.clone()),
                row,
            ]));
        }
        let primary_associations = materializer
            .materialize_map(Some(&prior.primary_associations), &association_facts)
            .map_err(|error| source_failure(error.to_string()))?;
        let lexical_root = if lexical_changed {
            inputs
                .lexical
                .materialize(primary_inputs.context(), &mut materializer)?
        } else {
            prior.lexical_root
        };
        let staged = materializer
            .finish()
            .map_err(|error| source_failure(error.to_string()))?;
        let changed_blocks = staged
            .blocks()
            .iter()
            .map(|(cid, bytes)| world_array(vec![world_blob(cid.to_bytes()), world_blob(bytes)]))
            .collect();
        let roots = SemanticRoots {
            primary: *primary_root.root().cid(),
            associations: *primary_associations.root().cid(),
            bindings: *prior.bindings.root().cid(),
            lexical: lexical_root,
        };
        let primary_blocks = prior
            .primary_blocks
            .replace_roots(
                &staged,
                vec![roots.associations, roots.bindings, roots.lexical],
            )
            .map_err(|error| source_failure(error.to_string()))?;
        let product = world_restoration_product(
            &roots,
            inputs.sources.seal(),
            &prior.producer,
            &prior.profile,
            inputs.bindings.query_scope,
        )?;
        let logical_id = world_digest(&product);
        let compiler_input_identity = compiler_input_identity(
            &roots,
            inputs.sources.seal(),
            &prior.producer,
            &prior.profile,
            inputs.bindings.query_scope,
        )?;
        let files: Vec<_> = rows.files.into_values().collect();
        let acquisition = rows
            .arenas
            .acquisition(rows.images.into_values().collect())?;
        let descriptor = directory.path().join("successor.cbor");
        std::fs::write(
            &descriptor,
            world_bytes(&world_array(vec![
                world_text("TPCWSUCC5"),
                world_blob(&product),
                world_array(changed_blocks),
                world_array(primary_changes),
                world_array(Vec::new()),
                acquisition,
            ]))?,
        )?;
        let restoration = Arc::new(CompilerWorldRestoration {
            logical_id,
            compiler_input_identity,
            inputs,
            product: product.into(),
            primary_root,
            runtime_context_blocks,
            primary_blocks,
            primary_associations,
            primary_inputs: advance.primary,
            bindings: prior.bindings.clone(),
            binding_rows: prior.binding_rows.clone(),
            lexical_root,
            producer: prior.producer,
            profile: prior.profile,
            descriptor,
            input_files: files.iter().cloned().collect(),
            successor: Some(WorldSuccessorAcquisition {
                predecessor: *self.logical_id(),
                realization: self.realization.clone(),
                files,
                cold: Mutex::new(None),
            }),
        });
        Ok(Arc::new(Self {
            restoration,
            realization: Arc::new(Mutex::new(None)),
            position: None,
        }))
    }

    fn project_bindings(
        self: &Arc<Self>,
        query_scope: u64,
        changes: impl IntoIterator<Item = (String, Option<WorldBindingFact>)>,
        position: Option<WorldCellPosition>,
    ) -> Result<Arc<Self>, CompileError> {
        self.project_bindings_with_primary(query_scope, changes, position, None)
    }

    fn project_bindings_with_primary(
        self: &Arc<Self>,
        query_scope: u64,
        changes: impl IntoIterator<Item = (String, Option<WorldBindingFact>)>,
        position: Option<WorldCellPosition>,
        primary: Option<WorldPrimaryAdvance>,
    ) -> Result<Arc<Self>, CompileError> {
        let prior = &self.restoration;
        let mut inputs = prior.inputs.clone();
        let primary_inputs = primary.as_ref().map_or_else(
            || prior.primary_inputs.clone(),
            |advance| advance.support().primary.clone(),
        );
        if let Some(advance) = &primary {
            if let Some(lexical) = advance.lexical() {
                inputs.lexical = lexical.clone();
            }
        }
        let directory = Arc::new(crate::artifacts::compiler_scratch_directory()?);
        inputs.directory = directory.clone();
        let mut facts = prior.bindings.facts().clone();
        let mut row_roots = prior.binding_rows.clone();
        let mut visible_changes = BTreeMap::new();
        let mut rows = WorldColdRows {
            producer: prior.producer,
            files: BTreeMap::new(),
            images: BTreeMap::new(),
            rows: BTreeMap::new(),
            arenas: Default::default(),
            diagnostics: None,
        };
        let mut materializer = Materializer::new(&prior.primary_blocks);
        let mut primary_root = prior.primary_root.clone();
        let mut runtime_context_blocks = prior.runtime_context_blocks.clone();
        let mut primary_associations = prior.primary_associations.clone();
        let mut primary_changes = Vec::new();
        let mut lexical_root = prior.lexical_root;
        if let Some(primary) = &primary {
            let advance = primary.support();
            let mut runtime_materializer = Materializer::new(&prior.runtime_context_blocks);
            primary_root = runtime_materializer
                .materialize_context(
                    Some(&prior.primary_root),
                    &primary_inputs.context().snapshot(),
                )
                .map_err(|error| source_failure(error.to_string()))?;
            let runtime_staged = runtime_materializer
                .finish()
                .map_err(|error| source_failure(error.to_string()))?;
            runtime_context_blocks = prior
                .runtime_context_blocks
                .replace_roots(&runtime_staged, vec![*primary_root.root().cid()])
                .map_err(|error| source_failure(error.to_string()))?;
            let mut associations = prior.primary_associations.facts().clone();
            let mut remaining = capture_allowance()?;
            let support = advance.primary.inputs().compiler_source_support();
            for (ordinal, owner) in advance.changed_owners.iter().enumerate() {
                let selected = support.compiler_primary_input(owner)?.ok_or_else(|| {
                    source_failure("declaration delta omits its issued primary row")
                })?;
                let (row, fact) = materialize_primary_type_row(
                    &selected,
                    &prior.producer,
                    directory.path(),
                    ordinal,
                    &mut rows,
                    &mut materializer,
                    &mut remaining,
                )?;
                associations.insert(selected.owner().clone(), fact);
                primary_changes.push(world_array(vec![
                    world_text(&owner.unit),
                    world_text(&owner.module),
                    row,
                ]));
            }
            primary_associations = materializer
                .materialize_map(Some(&prior.primary_associations), &associations)
                .map_err(|error| source_failure(error.to_string()))?;
            if primary.lexical().is_some() {
                lexical_root = inputs
                    .lexical
                    .materialize(primary_inputs.context(), &mut materializer)?;
            }
        }
        let delta = prior.inputs.bindings.advance(query_scope, changes)?;
        inputs.bindings = delta.bindings;
        for owner in delta.rows {
            let selected = inputs.bindings.rows.get(&owner);
            let previous = prior.inputs.bindings.rows.get(&owner);
            if let Some(artifact) = selected {
                if previous.is_some_and(|old| old.same_world_type_row(artifact)) {
                    if !row_roots.contains_key(&owner) {
                        return Err(source_failure(
                            "predecessor checked row lacks its semantic root",
                        ));
                    }
                    continue;
                }
                let (entry, parts) = artifact.world_type_parts()?;
                let transport = rows.row(&entry, parts)?.0;
                let semantic = world_semantic_row(&transport, &prior.producer)?;
                let (_, receipt) = artifact.world_issuer();
                let root = materializer
                    .materialize_value(&world_array(vec![
                        semantic,
                        world_blob(artifact.certified_interface().artifact_id().0),
                        world_blob(receipt),
                    ]))
                    .map_err(|error| source_failure(error.to_string()))?;
                row_roots.insert(owner, root);
                visible_changes.insert(
                    (
                        entry.descriptor.owner.unit.clone(),
                        entry.descriptor.owner.module.clone(),
                    ),
                    transport,
                );
            } else if let Some(old) = previous {
                row_roots.remove(&owner);
                let interface = old.certified_interface().interface();
                visible_changes.insert(
                    (interface.unit().to_owned(), interface.module().to_owned()),
                    ciborium::value::Value::Null,
                );
            }
        }
        for occurrence in delta.occurrences {
            let Some(fact) = inputs.bindings.entries.get(&occurrence) else {
                facts.remove(&occurrence);
                continue;
            };
            let symbol = &fact.symbol;
            facts.insert(
                occurrence,
                (
                    (
                        symbol.unit.clone(),
                        symbol.module.clone(),
                        symbol.namespace.clone(),
                        symbol.occurrence.clone(),
                        symbol.record_parent.clone(),
                    ),
                    fact.session_var.raw(),
                    fact.defining_scope,
                    fact.row.owner().gen.0,
                    *row_roots
                        .get(&fact.row.owner().module_name())
                        .ok_or_else(|| source_failure("captured binder lacks its row root"))?,
                ),
            );
        }
        let bindings = materializer
            .materialize_map(Some(&prior.bindings), &facts)
            .map_err(|error| source_failure(error.to_string()))?;
        let staged = materializer
            .finish()
            .map_err(|error| source_failure(error.to_string()))?;
        let changed_blocks = staged
            .blocks()
            .iter()
            .map(|(cid, bytes)| world_array(vec![world_blob(cid.to_bytes()), world_blob(bytes)]))
            .collect();
        let roots = SemanticRoots {
            primary: *primary_root.root().cid(),
            associations: *primary_associations.root().cid(),
            bindings: *bindings.root().cid(),
            lexical: lexical_root,
        };
        let primary_blocks = prior
            .primary_blocks
            .replace_roots(
                &staged,
                vec![roots.associations, roots.bindings, roots.lexical],
            )
            .map_err(|error| source_failure(error.to_string()))?;
        let product = world_restoration_product(
            &roots,
            inputs.sources.seal(),
            &prior.producer,
            &prior.profile,
            query_scope,
        )?;
        let logical_id = world_digest(&product);
        let compiler_input_identity = compiler_input_identity(
            &roots,
            inputs.sources.seal(),
            &prior.producer,
            &prior.profile,
            inputs.bindings.query_scope,
        )?;
        let files: Vec<_> = rows.files.into_values().collect();
        let acquisition = rows
            .arenas
            .acquisition(rows.images.into_values().collect())?;
        let descriptor = directory.path().join("successor.cbor");
        std::fs::write(
            &descriptor,
            world_bytes(&world_array(vec![
                world_text("TPCWSUCC5"),
                world_blob(&product),
                world_array(changed_blocks),
                world_array(primary_changes),
                world_array(
                    visible_changes
                        .into_iter()
                        .map(|((unit, module), row)| {
                            world_array(vec![world_text(unit), world_text(module), row])
                        })
                        .collect(),
                ),
                acquisition,
            ]))?,
        )?;
        // Only the changed transport is retained. Each current immutable row
        // owns the bytes needed for cold recovery after every ancestor drops.
        let input_files = files.iter().cloned().collect();
        let restoration = Arc::new(CompilerWorldRestoration {
            logical_id,
            compiler_input_identity,
            inputs,
            product: product.into(),
            primary_root,
            runtime_context_blocks,
            primary_blocks,
            primary_associations,
            primary_inputs,
            bindings,
            binding_rows: row_roots,
            lexical_root,
            producer: prior.producer,
            profile: prior.profile,
            descriptor,
            input_files,
            successor: Some(WorldSuccessorAcquisition {
                predecessor: *self.logical_id(),
                realization: self.realization.clone(),
                files,
                cold: Mutex::new(None),
            }),
        });
        Ok(Arc::new(CompilerWorld {
            restoration,
            realization: Arc::new(Mutex::new(None)),
            position,
        }))
    }
}

pub use tidepool_extract_report::world_query::{
    ClosedModuleAbsence, WorldAnswer, WorldModuleOrigin, WorldModuleReceipt, WorldModuleResolution,
    WorldObservationOutcome, WorldObservationQuestion, WorldQueryRejection,
};

#[derive(Clone, Debug)]
pub struct WorldQueryResult {
    world: Arc<CompilerWorld>,
    question: PublicModuleQuestion,
    answer: WorldAnswer,
}

impl WorldQueryResult {
    pub fn world(&self) -> &Arc<CompilerWorld> {
        &self.world
    }
    pub fn question(&self) -> &PublicModuleQuestion {
        &self.question
    }
    pub fn resolution(&self) -> WorldModuleResolution {
        self.answer.resolution()
    }
    pub fn defining_module(&self) -> Option<&WorldModuleReceipt> {
        self.answer.defining_module()
    }
    pub fn answer(&self) -> &WorldAnswer {
        &self.answer
    }
    pub fn into_answer(self) -> WorldAnswer {
        self.answer
    }
}

/// A scoped answer issued only by an actual admitted service operation.
/// Runtime END, cancellation and the current actor view remain separate gates.
#[derive(Clone, Debug)]
pub struct ScopedWorldQueryResult {
    world: Arc<CompilerWorld>,
    question: WorldObservationQuestion,
    decoded: tidepool_extract_report::world_query::DecodedWorldObservation,
}
impl ScopedWorldQueryResult {
    pub fn world(&self) -> &Arc<CompilerWorld> {
        &self.world
    }
    pub fn question(&self) -> &WorldObservationQuestion {
        &self.question
    }
    pub fn provenance(&self) -> &crate::inspection::ScopeProvenance {
        &self.decoded.provenance
    }
    pub fn into_outcome(self) -> WorldObservationOutcome {
        self.decoded.outcome
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

fn world_restoration_product(
    roots: &SemanticRoots,
    source_seal: &[u8; 32],
    producer: &[u8; 32],
    profile: &[u8; 32],
    query_scope: u64,
) -> Result<Vec<u8>, CompileError> {
    world_bytes(&world_array(vec![
        world_text("TPCWREST"),
        5u64.into(),
        world_blob(roots.primary.to_bytes()),
        world_blob(source_seal),
        world_blob(producer),
        world_blob(profile),
        query_scope.into(),
        world_blob(roots.associations.to_bytes()),
        world_blob(roots.bindings.to_bytes()),
        world_blob(roots.lexical.to_bytes()),
    ]))
}

fn compiler_input_identity(
    roots: &SemanticRoots,
    source_seal: &[u8; 32],
    producer: &[u8; 32],
    profile: &[u8; 32],
    query_scope: u64,
) -> Result<[u8; 32], CompileError> {
    Ok(world_digest(&world_bytes(&world_array(vec![
        world_text("TPCWINPUT1"),
        world_blob(source_seal),
        world_blob(producer),
        world_blob(profile),
        query_scope.into(),
        world_blob(roots.associations.to_bytes()),
        world_blob(roots.bindings.to_bytes()),
        world_blob(roots.lexical.to_bytes()),
    ]))?))
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
            ArtifactPayload::Interface(..) | ArtifactPayload::PackageCode(_) => None,
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
    selected: &crate::artifact_inventory::CompilerPrimaryInput<'_>,
    semantic: &ciborium::value::Value,
) -> Result<cid::Cid, CompileError> {
    let binding = match selected.original() {
        None => world_array(vec![
            0u64.into(),
            ciborium::value::Value::serialized(&selected.interface())
                .map_err(|error| source_failure(error.to_string()))?,
        ]),
        Some(original) => world_array(vec![
            1u64.into(),
            ciborium::value::Value::serialized(&selected.interface())
                .map_err(|error| source_failure(error.to_string()))?,
            ciborium::value::Value::serialized(&original)
                .map_err(|error| source_failure(error.to_string()))?,
        ]),
    };
    materializer
        .materialize_value(&world_array(vec![
            world_text("TPCWPRIMARY2"),
            binding,
            semantic.clone(),
        ]))
        .map_err(|error| source_failure(error.to_string()))
}

fn materialize_primary_type_row<BS: fvm_ipld_blockstore::Blockstore>(
    selected: &crate::artifact_inventory::CompilerPrimaryInput<'_>,
    producer: &[u8; 32],
    directory: &Path,
    ordinal: usize,
    rows: &mut WorldColdRows,
    materializer: &mut Materializer<'_, BS>,
    remaining_raw: &mut u64,
) -> Result<(ciborium::value::Value, cid::Cid), CompileError> {
    use crate::declaration_context::original_inputs::{OriginalInputKind as K, OriginalInputPart};
    let owner = selected.owner();
    let entry = selected.entry();
    if owner != &entry.descriptor.owner {
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
    let mut builder = crate::owned_input_arena::OwnedInputArenaBuilder::new(capture_allowance()?)
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
    let fact = materialize_primary_association(materializer, selected, &semantic)?;
    Ok((row, fact))
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
        MaterializedMap<crate::declaration_join::ExactModuleIdentity, cid::Cid>,
    ),
    CompileError,
> {
    let mut primary_facts = imbl::OrdMap::new();
    let mut base = Vec::new();
    for (ordinal, selected) in context
        .compiler_source_support()
        .compiler_primary_inputs()
        .enumerate()
    {
        let selected = selected?;
        let (row, fact) = materialize_primary_type_row(
            &selected,
            producer,
            directory,
            ordinal,
            rows,
            materializer,
            remaining_raw,
        )?;
        if primary_facts
            .insert(selected.owner().clone(), fact)
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
    let mut runtime_materializer = Materializer::new(&store);
    let primary = runtime_materializer
        .materialize_context(None, &snapshot)
        .map_err(|error| source_failure(error.to_string()))?;
    let mut materializer = Materializer::new(&store);
    let mut rows = WorldColdRows {
        producer,
        files: BTreeMap::new(),
        images: BTreeMap::new(),
        rows: BTreeMap::new(),
        arenas: Default::default(),
        diagnostics: None,
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
    let lexical = CompilerWorldLexicalSurface::from_import_lines(Vec::new(), None)?
        .materialize(context, &mut materializer)?;
    let mut acquisition = rows
        .arenas
        .acquisition(rows.images.into_values().collect())?;
    let Value::Array(fields) = &mut acquisition else {
        unreachable!()
    };
    fields[0] = world_text("TPCWORLDFIXTUREBYTES2");
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
        world_blob(lexical.to_bytes()),
    ]))
}

impl CompilerWorldOpenInputs {
    fn prepare(
        self,
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
        self.prepare_for(producer, profile)
    }

    fn prepare_for(
        self,
        producer: [u8; 32],
        profile: [u8; 32],
    ) -> Result<Arc<CompilerWorldRestoration>, CompileError> {
        let context = Arc::new((*self.primary_context).clone().admit_producer(producer)?);
        crate::host_work::record_host_work(
            crate::host_work::HostWorkCounter::ColdWorldCompilerBasisCapture,
            1,
        );
        let primary = context.capture_world_primary_inputs(producer)?;
        let advance =
            primary.with_type_interfaces(self.resources.lexical.observation_type_interfaces())?;
        self.resources
            .prepare_for(advance.primary, producer, profile)
    }
}

impl WorldInputResources {
    fn materialize_primary(
        primary: &crate::declaration_context::WorldPrimaryInputs,
    ) -> Result<(MaterializedContext, ReachableBlocks), CompileError> {
        let snapshot = primary.context().snapshot();
        let store = fvm_ipld_blockstore::MemoryBlockstore::new();
        let mut materializer = Materializer::new(&store);
        let context = materializer
            .materialize_context(None, &snapshot)
            .map_err(|error| source_failure(error.to_string()))?;
        let primary_root = context;
        let staged = materializer
            .finish()
            .map_err(|error| source_failure(error.to_string()))?;
        let primary_blocks = ReachableBlocks::default()
            .replace_roots(&staged, vec![*primary_root.root().cid()])
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
        Ok((primary_root, primary_blocks))
    }

    fn prepare_for(
        self,
        primary_inputs: crate::declaration_context::WorldPrimaryInputs,
        producer: [u8; 32],
        profile: [u8; 32],
    ) -> Result<Arc<CompilerWorldRestoration>, CompileError> {
        let (primary_root, runtime_context_blocks) = Self::materialize_primary(&primary_inputs)?;
        let primary_blocks = ReachableBlocks::default();
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
            .and_then(|n| n.checked_add(self.lexical.metadata_bytes))
            .ok_or_else(source_size_failure)?;
        let mut remaining_raw = capture_allowance()?
            .checked_sub(envelope_bytes)
            .ok_or_else(source_size_failure)?;
        let mut binding_materializer = Materializer::new(&primary_blocks);
        let (base, primary_associations) = materialize_primary_associations(
            primary_inputs.context(),
            &producer,
            self.directory.path(),
            &mut rows,
            &mut binding_materializer,
            &mut remaining_raw,
        )?;
        let mut binding_rows = imbl::OrdMap::new();
        let mut visible = Vec::with_capacity(self.bindings.rows.len());
        for artifact in self.bindings.rows.values() {
            crate::host_work::checkpoint()?;
            crate::host_work::record_host_work(
                crate::host_work::HostWorkCounter::ColdWorldBuilderRowVisit,
                1,
            );
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
        let lexical_root = self
            .lexical
            .materialize(primary_inputs.context(), &mut binding_materializer)?;
        let staged = binding_materializer
            .finish()
            .map_err(|error| source_failure(error.to_string()))?;
        let semantic_roots = SemanticRoots {
            primary: *primary_root.root().cid(),
            associations: *primary_associations.root().cid(),
            bindings: *bindings.root().cid(),
            lexical: lexical_root,
        };
        let primary_blocks = primary_blocks
            .replace_roots(
                &staged,
                vec![
                    semantic_roots.associations,
                    semantic_roots.bindings,
                    semantic_roots.lexical,
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
        let product = world_restoration_product(
            &semantic_roots,
            self.sources.seal(),
            &producer,
            &profile,
            self.bindings.query_scope,
        )?;
        let logical_id = world_digest(&product);
        let compiler_input_identity = compiler_input_identity(
            &semantic_roots,
            self.sources.seal(),
            &producer,
            &profile,
            self.bindings.query_scope,
        )?;
        let source_path = self.directory.path().join("sources.bin");
        std::fs::write(&source_path, self.sources.encoded())?;
        let files = rows.files.into_values().collect();
        let acquisition = rows
            .arenas
            .acquisition(rows.images.into_values().collect())?;
        let descriptor = self.directory.path().join("world.cbor");
        let encoded = world_bytes(&world_array(vec![
            world_text("TPCWORLD"),
            5u64.into(),
            world_blob(&product),
            world_path(&source_path)?,
            world_blob(producer),
            world_blob(profile),
            world_array(vec![world_array(base), world_array(visible)]),
            acquisition,
            world_blob(&visibility),
            world_blob(primary_associations.root().cid().to_bytes()),
            world_array(blocks),
        ]))?;
        std::fs::write(&descriptor, encoded)?;
        Ok(Arc::new(CompilerWorldRestoration {
            logical_id,
            compiler_input_identity,
            inputs: self,
            product: product.into(),
            primary_root,
            runtime_context_blocks,
            primary_blocks,
            primary_associations,
            primary_inputs,
            bindings,
            binding_rows,
            lexical_root,
            producer,
            profile,
            descriptor,
            input_files: files,
            successor: None,
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
    affinity: Option<&tidepool_extract_cmd::CompilerWorkerLocation>,
) -> Result<
    (
        tidepool_extract_cmd::ExtractCmd,
        tidepool_extract_cmd::CompilerEndpoint,
    ),
    CompileError,
> {
    crate::host_work::checkpoint()?;
    let mut cmd =
        tidepool_extract_cmd::ExtractCmd::new().map_err(|e| source_failure(e.to_string()))?;
    cmd.set_worker_affinity_hint(affinity.cloned());
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

enum WorldAcquisitionOutcome {
    Ready(WorldRealizationObservation),
    Missing(tidepool_extract_cmd::WorldAcquisitionMiss),
}

fn world_acquisition_outcome(
    run: tidepool_extract_cmd::ExtractRun,
    root: &str,
) -> Result<WorldAcquisitionOutcome, CompileError> {
    use tidepool_extract_cmd::EnsureExactInputsOutcome as O;
    match O::decode(&run.output.stdout).map_err(world_response_decode_error)? {
        O::Ready {
            world,
            realization,
            residency,
        } if world == root => {
            WorldRealizationObservation::received(realization, run.worker_location, residency)
                .map(WorldAcquisitionOutcome::Ready)
        }
        O::NeedsAcquisition { world, reason } if world == root => {
            Ok(WorldAcquisitionOutcome::Missing(reason))
        }
        O::Rejected(reason) => Err(source_failure(format!(
            "compiler refused world acquisition: {reason:?}"
        ))),
        _ => Err(world_response_error(
            CompilerResponseRejection::ProvenanceMismatch,
        )),
    }
}

fn cold_world_acquisition(
    restoration: &CompilerWorldRestoration,
) -> Result<
    (
        tidepool_extract_cmd::ExactInputsAcquisition,
        Vec<Arc<std::fs::File>>,
    ),
    CompileError,
> {
    use tidepool_extract_cmd::ExactInputsAcquisition as A;
    match &restoration.successor {
        Some(successor) => {
            let mut cached = successor
                .cold
                .lock()
                .map_err(|_| source_failure("successor recovery lock poisoned"))?;
            if cached.is_none() {
                *cached = Some(restoration.prepare_cold()?);
            }
            let cold = cached.as_ref().expect("checked successor cold restoration");
            Ok((
                A::Complete(cold.descriptor.clone()),
                cold.input_files.iter().cloned().collect(),
            ))
        }
        None => Ok((
            A::Complete(restoration.descriptor.clone()),
            restoration.input_files.iter().cloned().collect(),
        )),
    }
}

fn ensure_world(
    world: &CompilerWorld,
    identity: &tidepool_extract_cmd::CompilerIdentity,
) -> Result<tidepool_extract_cmd::WorldRealizationRef, CompileError> {
    use tidepool_extract_cmd::{ExactInputsAcquisition as A, ExactInputsRequest};
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
    let observed = world
        .realization
        .lock()
        .map_err(|_| source_failure("world realization lock poisoned"))?
        .clone();
    if let Some(previous) = &observed {
        // Only an actual issued reference can make a resident lookup meaningful.
        let (mut cmd, endpoint) = world_command(identity, previous.location.as_ref())?;
        cmd.ensure_exact_inputs(ExactInputsRequest {
            root: root.clone(),
            acquisition: A::Resident(previous.reference.clone()),
        });
        crate::host_work::record_host_work(
            crate::host_work::HostWorkCounter::WorldEnsureResidentProbe,
            1,
        );
        match world_acquisition_outcome(world_execute(&cmd, endpoint, Vec::new())?, &root)? {
            WorldAcquisitionOutcome::Ready(ready) => {
                crate::host_work::record_host_work(
                    crate::host_work::HostWorkCounter::WorldEnsureResidentHit,
                    1,
                );
                let reference = ready.reference.clone();
                *world
                    .realization
                    .lock()
                    .map_err(|_| source_failure("world realization lock poisoned"))? = Some(ready);
                return Ok(reference);
            }
            WorldAcquisitionOutcome::Missing(_reason) => {
                crate::host_work::record_host_work(
                    crate::host_work::HostWorkCounter::WorldEnsureNeedsAcquisition,
                    1,
                );
            }
        }
    }
    if !tidepool_extract_cmd::retain_compiler_input_directory(Arc::clone(
        &restoration.inputs.directory,
    )) {
        return Err(source_failure(
            "world acquisition requires its compiler scope",
        ));
    }
    let predecessor = restoration
        .successor
        .as_ref()
        .map(|successor| {
            successor
                .realization
                .lock()
                .map(|held| held.clone())
                .map_err(|_| source_failure("predecessor realization lock poisoned"))
        })
        .transpose()?
        .flatten();
    let (acquisition, files, is_successor) = match (&restoration.successor, &predecessor) {
        (Some(successor), Some(previous)) => (
            A::Successor {
                predecessor: world_hex(&successor.predecessor),
                realization: previous.reference.clone(),
                descriptor: restoration.descriptor.clone(),
            },
            successor.files.clone(),
            true,
        ),
        _ => {
            let (acquisition, files) = cold_world_acquisition(restoration)?;
            (acquisition, files, false)
        }
    };
    let affinity = observed
        .as_ref()
        .or(predecessor.as_ref())
        .and_then(|previous| previous.location.as_ref());
    let submit = |acquisition,
                  files: Vec<Arc<std::fs::File>>|
     -> Result<WorldAcquisitionOutcome, CompileError> {
        let (mut cmd, endpoint) = world_command(identity, affinity)?;
        cmd.ensure_exact_inputs(ExactInputsRequest {
            root: root.clone(),
            acquisition,
        });
        crate::host_work::record_host_work(
            crate::host_work::HostWorkCounter::InputFdExport,
            files.len() as u64,
        );
        crate::host_work::record_host_work(
            crate::host_work::HostWorkCounter::WorldEnsureComplete,
            1,
        );
        let diagnostics = crate::artifacts::CompilerDiagnosticCapture::start(
            restoration.inputs.directory.path(),
            &cmd,
        );
        let run = world_execute(&cmd, endpoint, files)?;
        diagnostics.completed(
            restoration.inputs.directory.path(),
            &cmd,
            run.success(),
            &run.output.stderr,
        );
        world_acquisition_outcome(run, &root)
    };
    let ready = match submit(acquisition, files)? {
        WorldAcquisitionOutcome::Ready(ready) => ready,
        WorldAcquisitionOutcome::Missing(_reason) if is_successor => {
            // A known missing predecessor is reconstructed from this exact
            // retained World owner. No query, accepted compile, or effect is retried.
            let (acquisition, files) = cold_world_acquisition(restoration)?;
            match submit(acquisition, files)? {
                WorldAcquisitionOutcome::Ready(ready) => ready,
                WorldAcquisitionOutcome::Missing(_) => {
                    return Err(world_response_error(
                        CompilerResponseRejection::ProvenanceMismatch,
                    ))
                }
            }
        }
        WorldAcquisitionOutcome::Missing(_) => {
            return Err(world_response_error(
                CompilerResponseRejection::ProvenanceMismatch,
            ))
        }
    };
    crate::host_work::record_host_work(
        crate::host_work::HostWorkCounter::WorldEnsureCompleteReady,
        1,
    );
    let reference = ready.reference.clone();
    *world
        .realization
        .lock()
        .map_err(|_| source_failure("world realization lock poisoned"))? = Some(ready);
    Ok(reference)
}

#[cfg(test)]
pub(crate) fn assert_actual_world_reference_pairing(
    world: &CompilerWorld,
    identity: &tidepool_extract_cmd::CompilerIdentity,
) -> Result<(), CompileError> {
    use tidepool_extract_cmd::{ExactInputsAcquisition, ExactInputsRequest};
    ensure_world(world, identity)?;
    let before = world
        .realization
        .lock()
        .unwrap()
        .clone()
        .expect("actual acquired World");
    assert!(before.location.is_some(), "real daemon BEGIN location");
    let root = world_hex(world.logical_id());
    let (mut cmd, endpoint) = world_command(identity, before.location.as_ref())?;
    cmd.ensure_exact_inputs(ExactInputsRequest {
        root: root.clone(),
        acquisition: ExactInputsAcquisition::Resident(before.reference.clone()),
    });
    let mut run = world_execute(&cmd, endpoint, Vec::new())?;
    assert!(before
        .reference
        .matches_location(run.worker_location.as_ref().expect("actual BEGIN")));
    let decoded = tidepool_extract_cmd::EnsureExactInputsOutcome::decode(&run.output.stdout)
        .expect("actual Ready response");
    assert!(
        matches!(decoded, tidepool_extract_cmd::EnsureExactInputsOutcome::Ready { ref realization, .. }
        if realization == &before.reference)
    );
    // Alter only transport DATA, retaining the actual BEGIN observation and
    // the same production response decoder used before cache publication.
    let reference = before.reference.as_str().as_bytes();
    let offset = run
        .output
        .stdout
        .windows(reference.len())
        .position(|bytes| bytes == reference)
        .expect("actual encoded reference");
    run.output.stdout[offset] = if reference[0] == b'0' { b'1' } else { b'0' };
    let error = world_acquisition_outcome(run, &root)
        .err()
        .expect("crossed worker must refuse");
    let CompileError::CompilerEndpoint(error) = error else {
        panic!("typed protocol refusal")
    };
    assert_eq!(
        error.source.get_ref().and_then(|cause| cause
            .downcast_ref::<tidepool_extract_cmd::CompilerResponseRejection>(
        )),
        Some(&tidepool_extract_cmd::CompilerResponseRejection::ProvenanceMismatch)
    );
    let after = world.realization.lock().unwrap();
    assert_eq!(after.as_ref().unwrap().reference, before.reference);
    assert_eq!(after.as_ref().unwrap().location, before.location);
    Ok(())
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
            position: None,
        });
        ensure_world(&world, endpoint.identity())?;
        Ok(world)
    })
}

/// Observe the held logical world with a fresh operation authority. Residency
/// loss restores its retained product; accepted/uncertain queries are not replayed.
pub fn query_public_module(
    endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
    world: &Arc<CompilerWorld>,
    question: PublicModuleQuestion,
) -> Result<WorldQueryResult, CompileError> {
    scoped_world_action(|| query_world_inner(endpoint.identity(), Arc::clone(world), question))
}

/// Observe current lexical/type facts of the same held World. The supplied
/// view is descriptive correlation; it does not issue runtime admission.
pub fn query_scope(
    endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
    world: &Arc<CompilerWorld>,
    question: WorldObservationQuestion,
    provenance: &crate::inspection::ScopeProvenance,
) -> Result<ScopedWorldQueryResult, CompileError> {
    if provenance.scope != crate::inspection::NameScope::Current {
        return Err(world_response_error(
            CompilerResponseRejection::ProvenanceMismatch,
        ));
    }
    scoped_world_action(|| {
        let bytes = query_world_transport(
            endpoint.identity(),
            world,
            scoped_question_wire(&question, provenance),
        )?;
        let decoded = tidepool_extract_report::world_query::decode_world_observation(
            &bytes,
            &tidepool_extract_report::world_query::WorldObservationExpectation {
                world: world.logical_id(),
                question: &question,
                generation: provenance.generation,
                fingerprint: &provenance.fingerprint,
            },
        )
        .map_err(world_answer_decode_error)?;
        Ok(ScopedWorldQueryResult {
            world: Arc::clone(world),
            question,
            decoded,
        })
    })
}

fn scoped_question_wire(
    question: &WorldObservationQuestion,
    provenance: &crate::inspection::ScopeProvenance,
) -> tidepool_extract_cmd::WorldQueryQuestion {
    use crate::inspection::{IdentifierNamespace, NameNamespace};
    use tidepool_extract_cmd::WorldQueryQuestion as Q;
    let structured = |namespace, name: String| StructuredInspection {
        scope: InspectionScope::Current,
        namespace,
        name,
        generation: provenance.generation,
        fingerprint: provenance.fingerprint.clone(),
    };
    let query_namespace = |namespace| match namespace {
        NameNamespace::Any => InspectionNamespace::Any,
        NameNamespace::Value => InspectionNamespace::Value,
        NameNamespace::Type => InspectionNamespace::Type,
        NameNamespace::Constructor => InspectionNamespace::Constructor,
    };
    match question {
        WorldObservationQuestion::Info(query) => Q::ScopeInfo(structured(
            query_namespace(query.namespace),
            query.name.clone(),
        )),
        WorldObservationQuestion::Type(query) => Q::ScopeType(structured(
            query_namespace(query.namespace),
            query.name.clone(),
        )),
        WorldObservationQuestion::RowFit(reference) => Q::RowFit {
            query: structured(
                match reference.namespace {
                    IdentifierNamespace::Value => InspectionNamespace::Value,
                    IdentifierNamespace::Type => InspectionNamespace::Type,
                    IdentifierNamespace::Constructor => InspectionNamespace::Constructor,
                    IdentifierNamespace::Field => InspectionNamespace::Field,
                },
                reference.name.clone(),
            ),
            defining_module: reference.module.clone(),
        },
        WorldObservationQuestion::Expression(expression) => Q::Expression {
            generation: provenance.generation,
            fingerprint: provenance.fingerprint.clone(),
            expression: expression.clone(),
        },
        WorldObservationQuestion::TypeSearch { pattern, limit } => Q::TypeSearch {
            generation: provenance.generation,
            fingerprint: provenance.fingerprint.clone(),
            pattern: pattern.clone(),
            limit: *limit,
        },
    }
}

/// Parse current authored input in the logical world. Source authority and
/// physical END remain with the caller's runtime source compiler transaction.
pub fn plan_cell(
    endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
    world: &Arc<CompilerWorld>,
    recipe: Arc<tidepool_extract_cmd::WorldCellRecipe>,
    operand: Option<Arc<CompilerWorldCellOperand>>,
) -> Result<Arc<crate::cell_plan::CompilerWorldCellPlan>, CompileError> {
    scoped_world_action(|| {
        let producer = CanonicalProducerIdentity::from_compiler(endpoint.identity()).sha256();
        let profile = endpoint
            .identity()
            .package_source_association()
            .ok_or_else(|| {
                source_failure("cell planning requires its retained compiler deployment")
            })?;
        if producer != world.restoration.producer
            || profile.profile_id() != &world.restoration.profile
            || operand
                .as_ref()
                .is_some_and(|operand| operand.row.world_issuer().0 != producer)
        {
            return Err(source_failure(
                "cell operand or World belongs to another compiler deployment",
            ));
        }
        let directory = Arc::new(crate::artifacts::compiler_scratch_directory()?);
        let output = directory.path().join("cell-plan.cbor");
        if !tidepool_extract_cmd::retain_compiler_input_directory(Arc::clone(&directory)) {
            return Err(source_failure(
                "world cell planning requires its compiler scope",
            ));
        }
        let world_id = world_hex(world.logical_id());
        tidepool_extract_cmd::WorldCellPlanRequest::validate_borrowed(
            &world_id,
            &recipe,
            operand.as_ref().map(|operand| operand.request()),
            &output,
            None,
        )
        .map_err(|error| source_failure(error.to_string()))?;
        let request = tidepool_extract_cmd::WorldCellPlanRequest {
            world: world_id,
            realization: ensure_world(world, endpoint.identity())?,
            recipe: Arc::clone(&recipe),
            operand: operand.as_ref().map(|operand| operand.request().clone()),
            output: output.clone(),
        };
        let realization = request.realization.clone();
        let (mut command, bound) = world_command(endpoint.identity(), None)?;
        command
            .world_cell_plan(request)
            .map_err(|error| source_failure(error.to_string()))?;
        let diagnostics =
            crate::artifacts::CompilerDiagnosticCapture::start(directory.path(), &command);
        let files = operand
            .as_ref()
            .map(|operand| operand.retain_for_request())
            .transpose()?
            .unwrap_or_default();
        let result = world_execute(&command, bound, files)?;
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
            Arc::clone(world),
            recipe,
            operand,
            CanonicalProducerIdentity::from_compiler(endpoint.identity()).sha256(),
            &realization,
            receipt,
        )
    })
}

/// Physical completion of one world-bound preparation. Only the checked
/// program issuer in this module can turn these observations into authority.
struct WorldCellOutputAdmission {
    request_correlation: String,
    plan_receipt_sha256: [u8; 32],
    pub(crate) origin: tidepool_extract_cmd::CompilerTransactionOrigin,
    pub(crate) reservation: Arc<crate::cell_plan::CompilerWorldCellReservation>,
    pub(crate) directory: Arc<tempfile::TempDir>,
    pub(crate) observations: Arc<[u8]>,
    pub(crate) common_files: BTreeMap<PathBuf, [u8; 32]>,
    pub(crate) demanded_rows: Vec<Arc<CheckedValueArtifact>>,
    demanded_session_bindings: Vec<tidepool_repr::SessionVarId>,
    pub(crate) captured_files: BTreeMap<PathBuf, WorldOutputFile>,
    pub(crate) validation: crate::recovery_artifacts::PackageInterfaceValidation,
    native_demands: Arc<native_demand::WorldNativeDemandCompletion>,
    segments: Vec<Arc<crate::checked_cell::CheckedTypedSegmentPlan>>,
    stages: Vec<Arc<CheckedWorldCellStage>>,
    item_proofs: Vec<Arc<WorldItemProof>>,
}

impl WorldCellOutputAdmission {
    pub(crate) fn reservation(&self) -> &Arc<crate::cell_plan::CompilerWorldCellReservation> {
        &self.reservation
    }

    pub(crate) fn primary_context(&self) -> &Arc<ExactDeclarationContext> {
        self.native_demands.primary.context()
    }
    pub(crate) fn segments(&self) -> &[Arc<crate::checked_cell::CheckedTypedSegmentPlan>] {
        &self.segments
    }

    /// One decoded item proof per reservation slot, correlated with the same
    /// physical output that produced the captured files.
    pub(crate) fn item_proofs(&self) -> &[Arc<WorldItemProof>] {
        &self.item_proofs
    }

    pub(crate) fn source_path_domain<'a>(
        &'a self,
        aliases: &'a [PathBuf],
    ) -> Result<WorldSourcePathDomain<'a>, CompileError> {
        WorldSourcePathDomain::new(
            &self.reservation.plan().world().restoration.inputs.sources,
            aliases,
            &self.validation.inventory,
        )
    }
}

/// Bytes and their observed seal from the one named output acquisition. This
/// carries physical custody, independently of semantic or native admission.
#[derive(Clone, Debug)]
pub(crate) struct WorldOutputFile {
    bytes: Arc<[u8]>,
    sha256: [u8; 32],
}

impl WorldOutputFile {
    pub(crate) fn bytes(&self) -> &Arc<[u8]> {
        &self.bytes
    }
    pub(crate) fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
}

/// Closed identity of one actual compiler stage. Original stages do not own a
/// typed executable plan or a native entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorldCellStageIdentity {
    Executable {
        typed_digest: String,
    },
    Declaration {
        ordinal: u64,
        owner: crate::declaration_join::ExactModuleIdentity,
    },
    Prologue {
        ordinal: u64,
        owner: crate::declaration_join::ExactModuleIdentity,
    },
}

impl WorldCellStageIdentity {
    pub(crate) fn decode(value: &ciborium::value::Value) -> Result<Self, CompileError> {
        let ciborium::value::Value::Array(fields) = value else {
            return Err(world_response_error(CompilerResponseRejection::Malformed));
        };
        match fields.first().map(answer_text).transpose()? {
            Some("exec") if fields.len() == 2 => {
                let typed_digest = answer_text(&fields[1])?;
                let digest = crate::execution_source::parse_digest(typed_digest)
                    .map_err(world_response_decode_error)?;
                if world_hex(&digest) != typed_digest {
                    return Err(world_response_error(
                        CompilerResponseRejection::ProvenanceMismatch,
                    ));
                }
                Ok(Self::Executable {
                    typed_digest: typed_digest.to_owned(),
                })
            }
            Some(kind @ ("declaration" | "prologue")) if fields.len() == 4 => {
                let ordinal = answer_u64(&fields[1])?;
                let owner = crate::declaration_join::ExactModuleIdentity {
                    unit: answer_text(&fields[2])?.to_owned(),
                    module: answer_text(&fields[3])?.to_owned(),
                };
                Ok(if kind == "declaration" {
                    Self::Declaration { ordinal, owner }
                } else {
                    Self::Prologue { ordinal, owner }
                })
            }
            _ => Err(world_response_error(CompilerResponseRejection::Malformed)),
        }
    }

    pub(crate) fn original_owner(&self) -> Option<&crate::declaration_join::ExactModuleIdentity> {
        match self {
            Self::Executable { .. } => None,
            Self::Declaration { owner, .. } | Self::Prologue { owner, .. } => Some(owner),
        }
    }

    pub(crate) fn identity_bytes(&self) -> Vec<u8> {
        let fields = match self {
            Self::Executable { typed_digest } => vec![
                ciborium::value::Value::Text("exec".into()),
                ciborium::value::Value::Text(typed_digest.clone()),
            ],
            Self::Declaration { ordinal, owner } | Self::Prologue { ordinal, owner } => vec![
                ciborium::value::Value::Text(
                    if matches!(self, Self::Declaration { .. }) {
                        "declaration"
                    } else {
                        "prologue"
                    }
                    .into(),
                ),
                ciborium::value::Value::Integer((*ordinal).into()),
                ciborium::value::Value::Text(owner.unit.clone()),
                ciborium::value::Value::Text(owner.module.clone()),
            ],
        };
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&ciborium::value::Value::Array(fields), &mut bytes)
            .expect("in-memory stage identity");
        bytes
    }
}

#[derive(Clone, Debug)]
pub(crate) struct WorldStageFileRef {
    relative: PathBuf,
    sha256: [u8; 32],
}
impl WorldStageFileRef {
    pub(crate) fn relative(&self) -> &Path {
        &self.relative
    }
    pub(crate) fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
}
#[derive(Clone, Debug)]
pub(crate) struct WorldStageOutputs {
    source_receipt: WorldStageFileRef,
    module_products: WorldStageFileRef,
    certified_products: WorldStageFileRef,
}
impl WorldStageOutputs {
    pub(crate) fn source_receipt(&self) -> &WorldStageFileRef {
        &self.source_receipt
    }
    pub(crate) fn module_products(&self) -> &WorldStageFileRef {
        &self.module_products
    }
    pub(crate) fn certified_products(&self) -> &WorldStageFileRef {
        &self.certified_products
    }
}
/// Stage ownership comes only from the same completed preparation and its
/// exact ordered reservation. Named outputs remain in that physical owner.
#[derive(Clone, Debug)]
pub(crate) struct CheckedWorldCellStage {
    index: usize,
    identity: WorldCellStageIdentity,
    ordinals: Vec<usize>,
    outputs: WorldStageOutputs,
    selected_inputs: Vec<WorldCellRuntimeInput>,
    dependencies: Arc<SealedStageDependencies>,
    demanded_rows: Vec<Arc<CheckedValueArtifact>>,
    demanded_session_bindings: Vec<tidepool_repr::SessionVarId>,
}
impl CheckedWorldCellStage {
    pub(crate) fn index(&self) -> usize {
        self.index
    }
    pub(crate) fn identity(&self) -> &WorldCellStageIdentity {
        &self.identity
    }
    pub(crate) fn ordinals(&self) -> &[usize] {
        &self.ordinals
    }
    pub(crate) fn outputs(&self) -> &WorldStageOutputs {
        &self.outputs
    }
    pub(crate) fn contains_output(&self, path: &Path) -> bool {
        path.starts_with(format!("stage-{}", self.ordinals[0]))
    }
    pub(crate) fn demanded_rows(&self) -> &[Arc<CheckedValueArtifact>] {
        &self.demanded_rows
    }
    pub(crate) fn demanded_session_bindings(&self) -> &[tidepool_repr::SessionVarId] {
        &self.demanded_session_bindings
    }
    pub(crate) fn dependencies(&self) -> &Arc<SealedStageDependencies> {
        &self.dependencies
    }
}

/// Runtime roots selected before execution and those produced inside this cell.
/// Both lists derive only from the same authenticated stage references.
pub struct WorldCellBindingDemand {
    existing: Vec<tidepool_repr::SessionVarId>,
    prior_stage: Vec<tidepool_repr::SessionVarId>,
}
impl WorldCellBindingDemand {
    pub fn existing(&self) -> &[tidepool_repr::SessionVarId] {
        &self.existing
    }
    pub fn prior_stage(&self) -> &[tidepool_repr::SessionVarId] {
        &self.prior_stage
    }
}

/// A borrowed view of one authenticated stage's selected runtime inputs.
/// These observations do not grant access to a live runtime value.
pub struct WorldCellRuntimeInputs<'a> {
    stage: &'a CheckedWorldCellStage,
}
impl<'a> WorldCellRuntimeInputs<'a> {
    pub fn stage_first_ordinal(&self) -> usize {
        self.stage.ordinals[0]
    }
    pub fn inputs(&self) -> &'a [WorldCellRuntimeInput] {
        &self.stage.selected_inputs
    }
}

#[derive(Clone, Debug)]
enum WorldCellRuntimeInputSource {
    BaseWorld {
        binding: WorldBindingFact,
        seal: ParsedWorldRowSeal,
    },
    CellOperand {
        operand: Arc<CompilerWorldCellOperand>,
        seal: ParsedWorldRowSeal,
    },
    PriorStage(WorldPriorProducedInput),
}

#[derive(Clone, Debug)]
struct WorldPriorProducedInput {
    producer: Arc<WorldItemOutputEvidence>,
    consumer: usize,
    binder_index: usize,
    symbol: tidepool_repr::execution_schema::SymbolIdentity,
    defining_scope: u64,
    interface: WorldOutputFile,
}

/// Input evidence linked to its actual issuing binding, operand or captured
/// producer output. It grants no access to a live runtime value.
#[derive(Clone, Debug)]
pub struct WorldCellRuntimeInput {
    occurrence: String,
    source: WorldCellRuntimeInputSource,
}
impl WorldCellRuntimeInput {
    fn view(&self) -> WorldInputRefView<'_> {
        let (origin, symbol, session_var, defining_scope, generation, seal): (
            _,
            _,
            _,
            _,
            _,
            &[u8],
        ) = match &self.source {
            WorldCellRuntimeInputSource::BaseWorld { binding, seal } => (
                WorldCellRuntimeInputOrigin::BaseWorld,
                &binding.symbol,
                binding.session_var.raw(),
                binding.defining_scope,
                binding.row.owner().gen.0,
                seal.encoded.as_slice(),
            ),
            WorldCellRuntimeInputSource::CellOperand { operand, seal } => (
                WorldCellRuntimeInputOrigin::CellOperand,
                operand.symbol(),
                operand.session_var().raw(),
                operand.defining_scope(),
                operand.generation(),
                seal.encoded.as_slice(),
            ),
            WorldCellRuntimeInputSource::PriorStage(prior) => {
                let binder = &prior
                    .producer
                    .executable()
                    .expect("issued prior producer")
                    .binders()[prior.binder_index];
                (
                    WorldCellRuntimeInputOrigin::PriorStage {
                        producer: prior.producer.index,
                        consumer: prior.consumer,
                    },
                    &prior.symbol,
                    binder.varid(),
                    prior.defining_scope,
                    prior.producer.generation,
                    prior.interface.sha256(),
                )
            }
        };
        WorldInputRefView {
            origin,
            occurrence: &self.occurrence,
            symbol,
            session_var,
            defining_scope,
            generation,
            seal,
        }
    }
    pub fn origin(&self) -> WorldCellRuntimeInputOrigin {
        self.view().origin
    }
    pub fn occurrence(&self) -> &str {
        &self.occurrence
    }
    pub fn symbol(&self) -> &tidepool_repr::execution_schema::SymbolIdentity {
        self.view().symbol
    }
    pub fn session_var(&self) -> tidepool_repr::SessionVarId {
        tidepool_repr::SessionVarId::from_extract(self.view().session_var)
    }
    pub fn defining_scope(&self) -> u64 {
        self.view().defining_scope
    }
    pub fn generation(&self) -> u64 {
        self.view().generation
    }
    pub fn seal(&self) -> &[u8] {
        self.view().seal
    }
    pub fn checked_row(&self) -> Option<&Arc<CheckedValueArtifact>> {
        match &self.source {
            WorldCellRuntimeInputSource::BaseWorld { binding, .. } => Some(&binding.row),
            WorldCellRuntimeInputSource::CellOperand { operand, .. } => Some(operand.checked_row()),
            WorldCellRuntimeInputSource::PriorStage(_) => None,
        }
    }
}

/// Parsed stage data remains private until its input owners and captured
/// producer evidence have been authenticated by preparation admission.
#[derive(Clone)]
struct DecodedWorldCellStage {
    index: usize,
    identity: WorldCellStageIdentity,
    ordinals: Vec<usize>,
    outputs: WorldStageOutputs,
    selected_inputs: Vec<WorldInputRef>,
    dependency_terminals: Vec<DependencyTerminalReference>,
}
#[cfg(test)]
impl DecodedWorldCellStage {
    fn index(&self) -> usize {
        self.index
    }
    fn identity(&self) -> &WorldCellStageIdentity {
        &self.identity
    }
    fn ordinals(&self) -> &[usize] {
        &self.ordinals
    }
    fn contains_output(&self, path: &Path) -> bool {
        path.starts_with(format!("stage-{}", self.ordinals[0]))
    }
}

#[derive(Debug)]
pub(crate) struct WorldExecutableItemProof {
    entry: String,
    native_ordinal: u32,
    segment_digest: String,
    symbol: tidepool_repr::execution_schema::SymbolIdentity,
    binders: Vec<crate::checked_cell::WorldItemBinder>,
    signatures: Vec<crate::checked_cell::ExactCheckedSignature>,
    manifest: BTreeMap<PathBuf, [u8; 32]>,
}
impl WorldExecutableItemProof {
    pub(crate) fn entry(&self) -> &str {
        &self.entry
    }
    pub(crate) fn native_ordinal(&self) -> u32 {
        self.native_ordinal
    }
    pub(crate) fn segment_digest(&self) -> &str {
        &self.segment_digest
    }
    pub(crate) fn symbol(&self) -> &tidepool_repr::execution_schema::SymbolIdentity {
        &self.symbol
    }
    pub(crate) fn binders(&self) -> &[crate::checked_cell::WorldItemBinder] {
        &self.binders
    }
    pub(crate) fn manifest(&self) -> &BTreeMap<PathBuf, [u8; 32]> {
        &self.manifest
    }
}
#[derive(Debug)]
pub(crate) struct WorldOriginalItemProof {
    owner: crate::declaration_join::ExactModuleIdentity,
    iface_sha256: [u8; 32],
    inventory_json: String,
}
impl WorldOriginalItemProof {
    pub(crate) fn owner(&self) -> &crate::declaration_join::ExactModuleIdentity {
        &self.owner
    }
    pub(crate) fn iface_sha256(&self) -> &[u8; 32] {
        &self.iface_sha256
    }
    pub(crate) fn inventory_json(&self) -> &str {
        &self.inventory_json
    }
}
#[derive(Debug)]
pub(crate) enum WorldItemProofKind {
    Executable(WorldExecutableItemProof),
    Declaration(WorldOriginalItemProof),
    Prologue(WorldOriginalItemProof),
}
/// One normalized proof shared by the physical output and checked item. Raw
/// transport trees and copied native manifests are not retained alongside it.
#[derive(Debug)]
pub(crate) struct WorldItemProof {
    output: Arc<WorldItemOutputEvidence>,
    stage: Arc<CheckedWorldCellStage>,
}
#[derive(Debug)]
struct WorldItemOutputEvidence {
    index: usize,
    ordinal: u64,
    generation: u64,
    stage_index: usize,
    kind: WorldItemProofKind,
}
impl WorldItemOutputEvidence {
    fn executable(&self) -> Option<&WorldExecutableItemProof> {
        match &self.kind {
            WorldItemProofKind::Executable(proof) => Some(proof),
            _ => None,
        }
    }
}
impl WorldItemProof {
    pub(crate) fn index(&self) -> usize {
        self.output.index
    }
    pub(crate) fn ordinal(&self) -> u64 {
        self.output.ordinal
    }
    pub(crate) fn generation(&self) -> u64 {
        self.output.generation
    }
    pub(crate) fn stage(&self) -> &Arc<CheckedWorldCellStage> {
        &self.stage
    }
    pub(crate) fn kind(&self) -> &WorldItemProofKind {
        &self.output.kind
    }
    pub(crate) fn executable(&self) -> Option<&WorldExecutableItemProof> {
        self.output.executable()
    }
    pub(crate) fn signatures(&self) -> &[crate::checked_cell::ExactCheckedSignature] {
        self.executable()
            .map_or(&[], |proof| proof.signatures.as_slice())
    }
}

/// Exact source selection supplied by the runtime source-admission owner.
/// The toolchain cannot name runtime scope leases, so the Host supplies these
/// plain facts and retains the leases themselves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldSourceSelection {
    sha256: [u8; 32],
    revision: String,
}

impl WorldSourceSelection {
    pub fn new(sha256: [u8; 32], revision: impl Into<String>) -> Result<Self, CompileError> {
        let revision = revision.into();
        if sha256 == [0; 32] || revision.is_empty() || revision.len() > 65536 {
            return Err(source_failure("world source selection is incomplete"));
        }
        Ok(Self { sha256, revision })
    }

    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }

    pub fn revision(&self) -> &str {
        &self.revision
    }
}

/// Exact logical world issued with a checked cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldCellLogicalIdentity {
    logical_id: [u8; 32],
    producer_sha256: [u8; 32],
    profile_sha256: [u8; 32],
}

impl WorldCellLogicalIdentity {
    pub fn logical_id(&self) -> &[u8; 32] {
        &self.logical_id
    }

    pub fn producer_sha256(&self) -> &[u8; 32] {
        &self.producer_sha256
    }

    pub fn profile_sha256(&self) -> &[u8; 32] {
        &self.profile_sha256
    }
}

/// One checked World item retained with the certificate that issued it. The
/// ordered generation and native ordinal are the exact rows the compiler
/// preparation observed; they grant no runtime value or execution permission.
#[derive(Clone, Debug)]
pub struct CheckedWorldCellItem {
    proof: Arc<WorldItemProof>,
    checked: crate::checked_cell::CheckedItem,
}
impl CheckedWorldCellItem {
    pub(crate) fn checked_item_observation(&self) -> &crate::checked_cell::CheckedItem {
        &self.checked
    }
    pub(crate) fn selected_runtime_inputs(&self) -> WorldCellRuntimeInputs<'_> {
        WorldCellRuntimeInputs {
            stage: self.proof.stage(),
        }
    }
    pub fn ordinal(&self) -> usize {
        self.proof.index()
    }
    pub fn generation(&self) -> u64 {
        self.proof.generation()
    }
    pub(crate) fn proof(&self) -> &WorldItemProof {
        &self.proof
    }
    pub(crate) fn kind(&self) -> &WorldItemProofKind {
        self.proof.kind()
    }
    pub(crate) fn executable(&self) -> Option<&WorldExecutableItemProof> {
        self.proof.executable()
    }
    pub(crate) fn binders(&self) -> &[crate::checked_cell::WorldItemBinder] {
        self.executable().map_or(&[], |proof| proof.binders())
    }
    pub(crate) fn signatures(&self) -> &[crate::checked_cell::ExactCheckedSignature] {
        self.checked.signatures()
    }
}

/// The SAME-certified checked World CellProgram. It can only be issued from
/// one physical `WorldCellOutputAdmission`; a raw completion or a decoded
/// receipt is not authority. It binds the reservation, logical world, labeled
/// original/target selections, producer and issuing transaction origin,
/// per-item generations and the exact output certificate.
// boundary: see CELL-PREPARATION-BOUNDARY.md
pub struct CheckedWorldCellProgram {
    output: WorldCellOutputAdmission,
    original: WorldSourceSelection,
    target: WorldSourceSelection,
    logical: WorldCellLogicalIdentity,
    items: Vec<CheckedWorldCellItem>,
    checked_source: Arc<str>,
    certificate_sha256: [u8; 32],
    producer_identity: [u8; 32],
}

impl CheckedWorldCellProgram {
    pub(crate) fn checked_source(&self) -> &Arc<str> {
        &self.checked_source
    }

    pub(crate) fn observations(&self) -> &Arc<[u8]> {
        &self.output.observations
    }

    /// Replay data-only refusal controls after the actual compiler transaction
    /// has closed; this never returns an admitted owner to its caller.
    #[cfg(test)]
    pub(crate) fn assert_captured_stage_output_refusals(&self) {
        answer_tests::assert_captured_stage_output_refusals(self);
    }

    pub fn reservation(&self) -> &Arc<crate::cell_plan::CompilerWorldCellReservation> {
        &self.output.reservation
    }

    pub fn origin(&self) -> &tidepool_extract_cmd::CompilerTransactionOrigin {
        &self.output.origin
    }

    pub fn original_source(&self) -> &WorldSourceSelection {
        &self.original
    }

    pub fn target_source(&self) -> &WorldSourceSelection {
        &self.target
    }

    pub fn logical(&self) -> &WorldCellLogicalIdentity {
        &self.logical
    }

    pub fn producer_sha256(&self) -> &[u8; 32] {
        self.logical.producer_sha256()
    }

    /// Raw endpoint producer identity bytes retained by the same admission
    /// that bound the plan. Their canonical digest is `producer_sha256`.
    // boundary: see CELL-PREPARATION-BOUNDARY.md
    pub(crate) fn producer_identity_bytes(&self) -> &[u8; 32] {
        &self.producer_identity
    }

    pub fn plan_digest(&self) -> [u8; 32] {
        self.output.plan_receipt_sha256
    }

    pub fn certificate_sha256(&self) -> &[u8; 32] {
        &self.certificate_sha256
    }

    pub fn items(&self) -> &[CheckedWorldCellItem] {
        &self.items
    }

    /// Runtime identities observed in the same admitted preparation demand.
    /// These descriptions grant no value access; runtime leases remain owned
    /// by the receiving session and its source admission.
    pub fn demanded_session_bindings(&self) -> &[tidepool_repr::SessionVarId] {
        &self.output.demanded_session_bindings
    }
    /// Select only already existing roots for the preexecution lease. A prior
    /// stage's reserved value is installed by its ordered runtime item first.
    pub fn runtime_binding_demand(&self) -> WorldCellBindingDemand {
        let mut existing = std::collections::BTreeSet::new();
        let mut prior_stage = std::collections::BTreeSet::new();
        for stage in &self.output.stages {
            for input in &stage.selected_inputs {
                match input.origin() {
                    WorldCellRuntimeInputOrigin::BaseWorld
                    | WorldCellRuntimeInputOrigin::CellOperand => {
                        existing.insert(input.session_var().raw());
                    }
                    WorldCellRuntimeInputOrigin::PriorStage { .. } => {
                        prior_stage.insert(input.session_var().raw());
                    }
                }
            }
        }
        WorldCellBindingDemand {
            existing: existing
                .into_iter()
                .map(tidepool_repr::SessionVarId::from_extract)
                .collect(),
            prior_stage: prior_stage
                .into_iter()
                .map(tidepool_repr::SessionVarId::from_extract)
                .collect(),
        }
    }

    pub fn item(&self, ordinal: usize) -> Option<&CheckedWorldCellItem> {
        self.items.get(ordinal)
    }

    pub(crate) fn primary_context(&self) -> &Arc<ExactDeclarationContext> {
        self.output.primary_context()
    }
    pub(crate) fn primary_inputs(&self) -> &crate::declaration_context::WorldPrimaryInputs {
        &self.output.native_demands.primary
    }
    pub(crate) fn native_demand_completion(
        &self,
    ) -> &Arc<native_demand::WorldNativeDemandCompletion> {
        &self.output.native_demands
    }
    pub(crate) fn segments(&self) -> &[Arc<crate::checked_cell::CheckedTypedSegmentPlan>] {
        &self.output.segments
    }

    pub(crate) fn stages(&self) -> &[Arc<CheckedWorldCellStage>] {
        &self.output.stages
    }

    pub(crate) fn validation(&self) -> &crate::recovery_artifacts::PackageInterfaceValidation {
        &self.output.validation
    }

    pub(crate) fn directory(&self) -> &Arc<tempfile::TempDir> {
        &self.output.directory
    }

    pub(crate) fn common_files(&self) -> &BTreeMap<PathBuf, [u8; 32]> {
        &self.output.common_files
    }

    pub(crate) fn captured_files(&self) -> &BTreeMap<PathBuf, WorldOutputFile> {
        &self.output.captured_files
    }

    pub(crate) fn demanded_rows(&self) -> &[Arc<CheckedValueArtifact>] {
        &self.output.demanded_rows
    }

    pub(crate) fn item_proofs(&self) -> &[Arc<WorldItemProof>] {
        &self.output.item_proofs
    }

    pub(crate) fn source_path_domain<'a>(
        &'a self,
        aliases: &'a [PathBuf],
    ) -> Result<WorldSourcePathDomain<'a>, CompileError> {
        self.output.source_path_domain(aliases)
    }

    fn admit(
        output: WorldCellOutputAdmission,
        original: WorldSourceSelection,
        target: WorldSourceSelection,
        origin: &tidepool_extract_cmd::CompilerTransactionOrigin,
        producer_identity: [u8; 32],
    ) -> Result<Self, CompileError> {
        // These labeled descriptions may agree. Runtime admission retains and
        // checks the actual original transaction and receiving placement.
        if output.origin != *origin {
            return Err(source_failure(
                "world cell preparation belongs to another compiler transaction",
            ));
        }
        let reservation = &output.reservation;
        if *reservation.admission_digest() == [0; 32] {
            return Err(source_failure(
                "world cell reservation has no issued admission",
            ));
        }
        let plan = reservation.plan();
        let logical_id = *plan.world().logical_id();
        if logical_id == [0; 32] {
            return Err(source_failure("world cell plan has no logical world"));
        }
        let producer_sha256 = *plan.producer_sha256();
        if producer_sha256 == [0; 32] {
            return Err(source_failure("world cell plan has no producer"));
        }
        if CanonicalProducerIdentity::from_producer_bytes(&producer_identity).sha256()
            != producer_sha256
        {
            return Err(source_failure(
                "world cell plan belongs to another compiler producer",
            ));
        }
        if output.item_proofs.len() != reservation.slots().len() {
            return Err(source_failure(
                "world cell preparation does not cover every reserved slot",
            ));
        }
        let (checked_source, checked_items) = crate::checked_cell::admit_world_checked_items(
            &output.observations,
            reservation,
            &output.segments,
            &output.item_proofs,
            &output.validation.inventory,
        )?;
        let mut items = Vec::with_capacity(output.item_proofs.len());
        for (ordinal, ((proof, slot), checked)) in output
            .item_proofs
            .iter()
            .zip(reservation.slots())
            .zip(checked_items)
            .enumerate()
        {
            if proof.index() != ordinal
                || proof.ordinal() != ordinal as u64
                || proof.generation() != world_cell_generation(&slot)
            {
                return Err(source_failure(
                    "world cell item proof differs from its reservation",
                ));
            }
            items.push(CheckedWorldCellItem {
                proof: Arc::clone(proof),
                checked,
            });
        }
        let logical = WorldCellLogicalIdentity {
            logical_id,
            producer_sha256,
            profile_sha256: plan.world().restoration.profile,
        };
        let certificate_sha256 = world_cell_output_certificate(
            reservation,
            &original,
            &target,
            &logical,
            &items,
            &output,
        );
        if certificate_sha256 == [0; 32] {
            return Err(source_failure("world cell output certificate is empty"));
        }
        tracing::debug!(target: "exomonad_harness::timing",
            logical_world = ?logical.logical_id(),
            compile_request = %output.request_correlation,
            output_certificate = ?certificate_sha256,
            visible_bindings = plan.world().restoration.inputs.bindings.entries.len(),
            demanded_runtime_roots = output.demanded_session_bindings.len(),
            demanded_binding_ids = ?output.demanded_session_bindings.iter().map(|id| id.raw()).collect::<Vec<_>>(),
            retained_type_rows = output.demanded_rows.len(),
            "checked World cell output admitted");
        Ok(Self {
            output,
            original,
            target,
            logical,
            items,
            checked_source,
            certificate_sha256,
            producer_identity,
        })
    }
}

fn world_cell_output_certificate(
    reservation: &crate::cell_plan::CompilerWorldCellReservation,
    original: &WorldSourceSelection,
    target: &WorldSourceSelection,
    logical: &WorldCellLogicalIdentity,
    items: &[CheckedWorldCellItem],
    output: &WorldCellOutputAdmission,
) -> [u8; 32] {
    let mut digest = blake3::Hasher::new();
    let mut frame = |bytes: &[u8]| {
        digest.update(&(bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    };
    frame(b"TidepoolCheckedWorldCellProgram6");
    frame(reservation.admission_digest());
    frame(&output.plan_receipt_sha256);
    frame(logical.logical_id());
    frame(logical.producer_sha256());
    frame(logical.profile_sha256());
    for selection in [original, target] {
        frame(selection.sha256());
        frame(selection.revision().as_bytes());
    }
    for item in items {
        frame(&(item.ordinal() as u64).to_le_bytes());
        frame(&item.generation().to_le_bytes());
        let Some(item) = item.executable() else {
            frame(&item.proof.stage.identity.identity_bytes());
            let (WorldItemProofKind::Declaration(original)
            | WorldItemProofKind::Prologue(original)) = item.kind()
            else {
                unreachable!()
            };
            frame(original.owner.unit.as_bytes());
            frame(original.owner.module.as_bytes());
            frame(&original.iface_sha256);
            frame(original.inventory_json.as_bytes());
            continue;
        };
        frame(item.entry.as_bytes());
        frame(&item.native_ordinal.to_le_bytes());
        frame(item.segment_digest.as_bytes());
        for part in [
            item.symbol.unit.as_str(),
            item.symbol.module.as_str(),
            item.symbol.namespace.as_str(),
            item.symbol.occurrence.as_str(),
        ] {
            frame(part.as_bytes());
        }
        frame(
            item.symbol
                .record_parent
                .as_deref()
                .unwrap_or("")
                .as_bytes(),
        );
        for (path, digest) in &item.manifest {
            frame(path.as_os_str().as_encoded_bytes());
            frame(digest);
        }
        for binder in &item.binders {
            frame(binder.name().as_bytes());
            frame(&binder.varid().to_le_bytes());
            frame(binder.module().as_bytes());
            frame(binder.type_display().as_bytes());
            frame(&[match binder.tier() {
                crate::checked_cell::WorldItemBinderTier::ForceData => 0,
                crate::checked_cell::WorldItemBinderTier::RetainOpaque => 1,
            }]);
        }
        for signature in &item.signatures {
            frame(signature.key().as_bytes());
            frame(signature.presentation().as_bytes());
            for name in signature.names() {
                frame(name.unit().as_bytes());
                frame(name.module().as_bytes());
                frame(name.namespace().as_bytes());
                frame(name.occurrence().as_bytes());
            }
        }
    }
    frame(&(output.stages.len() as u64).to_le_bytes());
    for stage in &output.stages {
        frame_world_stage_selection(
            &stage.identity,
            &stage.ordinals,
            stage
                .selected_inputs
                .iter()
                .map(WorldCellRuntimeInput::view),
            &mut frame,
        );
        frame(&(stage.dependencies.rows.len() as u64).to_le_bytes());
        for row in &stage.dependencies.rows {
            let terminal = &row.reference;
            frame_dependency_terminal(terminal, &mut frame);
        }
    }
    for id in &output.demanded_session_bindings {
        frame(&id.raw().to_le_bytes());
    }
    for row in &output.demanded_rows {
        let (producer, interface) = row.world_issuer();
        frame(&producer);
        frame(&interface);
    }
    for (path, digest) in &output.common_files {
        frame(path.as_os_str().as_encoded_bytes());
        frame(digest);
    }
    for (path, file) in &output.captured_files {
        frame(path.as_os_str().as_encoded_bytes());
        frame(file.sha256());
    }
    *digest.finalize().as_bytes()
}

fn frame_dependency_terminal(
    terminal: &DependencyTerminalReference,
    frame: &mut impl FnMut(&[u8]),
) {
    let symbol = |symbol: &tidepool_repr::execution_schema::SymbolIdentity,
                  frame: &mut dyn FnMut(&[u8])| {
        for part in [
            &symbol.unit,
            &symbol.module,
            &symbol.namespace,
            &symbol.occurrence,
        ] {
            frame(part.as_bytes());
        }
        match &symbol.record_parent {
            None => frame(&[0]),
            Some(parent) => {
                frame(&[1]);
                frame(parent.as_bytes());
            }
        }
    };
    let binding = |binding: crate::artifact_inventory::ArtifactBinding,
                   frame: &mut dyn FnMut(&[u8])| {
        frame(&binding.artifact.0);
        frame(&binding.selection.0);
    };
    frame(&[match terminal.origin {
        DependencyTerminalOrigin::SourceDefinition => 0,
        DependencyTerminalOrigin::NativeOriginal => 1,
    }]);
    let crate::certified_products::ReceiptImportOwner::OriginalBinding {
        unit,
        module,
        interface,
        original,
        binder,
    } = &terminal.original_use
    else {
        unreachable!("a decoded terminal names an Original binding")
    };
    frame(unit.as_bytes());
    frame(module.as_bytes());
    binding(*interface, frame);
    binding(*original, frame);
    symbol(binder, frame);
    frame(&terminal.demander.binding.artifact().0);
    frame(&terminal.demander.binding.selection().0);
    frame(&terminal.demander.original_ordinal.to_le_bytes());
    frame(&terminal.global_index.to_le_bytes());
    binding(terminal.binding, frame);
    symbol(&terminal.identity, frame);
    frame(&terminal.generation.to_le_bytes());
}

fn frame_world_stage_selection<'a>(
    identity: &WorldCellStageIdentity,
    ordinals: &[usize],
    selected: impl ExactSizeIterator<Item = WorldInputRefView<'a>>,
    frame: &mut impl FnMut(&[u8]),
) {
    frame(&identity.identity_bytes());
    frame(&(ordinals.len() as u64).to_le_bytes());
    for ordinal in ordinals {
        frame(&(*ordinal as u64).to_le_bytes());
    }
    frame(&(selected.len() as u64).to_le_bytes());
    for input in selected {
        match input.origin {
            WorldCellRuntimeInputOrigin::BaseWorld => frame(&[0]),
            WorldCellRuntimeInputOrigin::PriorStage { producer, consumer } => {
                frame(&[1]);
                frame(&(producer as u64).to_le_bytes());
                frame(&(consumer as u64).to_le_bytes());
            }
            WorldCellRuntimeInputOrigin::CellOperand => frame(&[2]),
        }
        frame(input.occurrence.as_bytes());
        for part in [
            &input.symbol.unit,
            &input.symbol.module,
            &input.symbol.namespace,
            &input.symbol.occurrence,
        ] {
            frame(part.as_bytes());
        }
        match &input.symbol.record_parent {
            None => frame(&[0]),
            Some(parent) => {
                frame(&[1]);
                frame(parent.as_bytes());
            }
        }
        frame(&input.session_var.to_le_bytes());
        frame(&input.defining_scope.to_le_bytes());
        frame(&input.generation.to_le_bytes());
        frame(&input.seal);
    }
}

/// Prepare and issue the SAME-certified checked World CellProgram. Preparation
/// refusal remains the typed `WorldQueryRejection`; a decoded completion or a
/// caller-built reservation cannot enter this owner.
// boundary: see CELL-PREPARATION-BOUNDARY.md
pub fn admit_world_cell_program(
    endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
    reservation: Arc<crate::cell_plan::CompilerWorldCellReservation>,
    original: WorldSourceSelection,
    target: WorldSourceSelection,
    origin: &tidepool_extract_cmd::CompilerTransactionOrigin,
) -> Result<Result<Arc<CheckedWorldCellProgram>, WorldQueryRejection>, CompileError> {
    let producer_identity = *endpoint.identity().producer_bytes();
    match prepare_world_cell_outputs(endpoint, reservation)? {
        Ok(output) => {
            CheckedWorldCellProgram::admit(output, original, target, origin, producer_identity)
                .map(|program| Ok(Arc::new(program)))
        }
        Err(rejection) => Ok(Err(rejection)),
    }
}

/// Obtain the actual held-world completion. This is kept private to the
/// artifact front door: parsing a completion cannot issue checked execution.
fn prepare_world_cell_outputs(
    endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
    reservation: Arc<crate::cell_plan::CompilerWorldCellReservation>,
) -> Result<Result<WorldCellOutputAdmission, WorldQueryRejection>, CompileError> {
    scoped_world_action(|| {
        let origin = tidepool_extract_cmd::CompilerTransactionOrigin::current()
            .ok_or_else(|| source_failure("world cell preparation requires its compiler scope"))?;
        let plan = reservation.plan();
        if *plan.producer_sha256()
            != CanonicalProducerIdentity::from_compiler(endpoint.identity()).sha256()
        {
            return Err(source_failure(
                "world cell plan belongs to another compiler producer",
            ));
        }
        let directory = Arc::new(crate::artifacts::compiler_scratch_directory()?);
        let output = directory.path().join("cell-prepared.cbor");
        let world_id = world_hex(plan.world().logical_id());
        tidepool_extract_cmd::WorldCellPrepareRequest::validate_borrowed(
            &world_id,
            None,
            plan.recipe(),
            plan.operand().map(|operand| operand.request()),
            plan.receipt(),
            reservation
                .items()
                .iter()
                .enumerate()
                .map(|(ordinal, item)| (ordinal as u64, item.request_slot())),
            directory.path(),
            &output,
        )
        .map_err(|error| source_failure(error.to_string()))?;
        if !tidepool_extract_cmd::retain_compiler_input_directory(Arc::clone(&directory)) {
            return Err(source_failure(
                "world cell preparation requires its compiler scope",
            ));
        }
        let request = tidepool_extract_cmd::WorldCellPrepareRequest {
            world: world_id,
            realization: ensure_world(plan.world(), endpoint.identity())?,
            recipe: plan.recipe_owned(),
            operand: plan.operand().map(|operand| operand.request().clone()),
            plan: plan.receipt_owned(),
            admission: *reservation.admission_digest(),
            defining_scope: reservation.defining_scope(),
            slots: world_cell_slots(&reservation),
            directory: directory.path().to_path_buf(),
            output: output.clone(),
        };
        // Borrowed payload preflight preceded acquisition. Validate the complete
        // request against those same limits before transport encoding.
        request
            .validate_transport()
            .map_err(|error| source_failure(error.to_string()))?;
        let (mut command, bound) = world_command(endpoint.identity(), None)?;
        command
            .world_cell_prepare(request)
            .map_err(|error| source_failure(error.to_string()))?;
        let diagnostics =
            crate::artifacts::CompilerDiagnosticCapture::start(directory.path(), &command);
        let files = plan
            .operand()
            .map(|operand| operand.retain_for_request())
            .transpose()?
            .unwrap_or_default();
        for file in files {
            if !tidepool_extract_cmd::retain_compiler_input_file(file) {
                return Err(source_failure(
                    "native preparation lost its compiler input scope",
                ));
            }
        }
        let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
        let mut resolver =
            native_demand::WorldNativeDemandResolver::new(&reservation, &mut validation)?;
        let owner = resolver.owner();
        let run = bound
            .execute_with_native_demand(&command, owner, &mut resolver)
            .map_err(CompileError::CompilerEndpoint);
        let native_demands = resolver.finish();
        let run = run?;

        diagnostics.completed(
            directory.path(),
            &command,
            run.success(),
            &run.output.stderr,
        );
        let native_demands = native_demands?;
        crate::diag::decode_extract_result(run.success(), &run.output.stdout, &run.output.stderr)?;
        let bytes = crate::certified_products::read_bounded_with_operation(
            &output,
            8 << 20,
            &validation.inventory,
        )
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
        admit_world_preparation_response(
            origin,
            run.request_correlation,
            reservation,
            directory,
            &bytes,
            validation,
            native_demands,
        )
    })
}

fn world_cell_slots(
    reservation: &crate::cell_plan::CompilerWorldCellReservation,
) -> Vec<(u64, tidepool_extract_cmd::PlannedCellSlot)> {
    use crate::checked_cell::CheckedPlannedCellSlot as S;
    use tidepool_extract_cmd::PlannedCellSlot as P;
    reservation
        .slots()
        .enumerate()
        .map(|(index, slot)| {
            (
                index as u64,
                match slot {
                    S::Prologue { declaration } => P::Prologue(declaration),
                    S::Declaration { declaration } => P::Declaration(declaration),
                    S::Bind { value } => P::Bind(value),
                    S::Expression {
                        capture,
                        observation_name,
                    } => P::Expression {
                        generation: capture,
                        observation: observation_name,
                    },
                },
            )
        })
        .collect()
}

fn world_cell_generation(slot: &crate::checked_cell::CheckedPlannedCellSlot) -> u64 {
    use crate::checked_cell::CheckedPlannedCellSlot as S;
    match slot {
        S::Prologue { declaration } | S::Declaration { declaration } => *declaration,
        S::Bind { value } => *value,
        S::Expression { capture, .. } => *capture,
    }
}

fn world_output_manifest(
    value: &ciborium::value::Value,
    inventory: &tidepool_repr::execution_schema::InventoryOperation,
) -> Result<BTreeMap<PathBuf, [u8; 32]>, CompileError> {
    let ciborium::value::Value::Array(entries) = value else {
        return Err(world_response_error(CompilerResponseRejection::Malformed));
    };
    inventory
        .charge_value_copies(value, 1)
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
    let mut files = BTreeMap::new();
    for entry in entries {
        crate::host_work::checkpoint()?;
        let entry = answer_array(entry, 2)?;
        let path = PathBuf::from(answer_text(&entry[0])?);
        relative_source_path(&path).map_err(world_response_decode_error)?;
        if files.insert(path, answer_hash(&entry[1])?).is_some() {
            return Err(world_response_error(CompilerResponseRejection::Malformed));
        }
    }
    Ok(files)
}

fn world_output_symbol(
    value: &ciborium::value::Value,
) -> Result<tidepool_repr::execution_schema::SymbolIdentity, CompileError> {
    let fields = answer_array(value, 5)?;
    // ExecutionEncode's option codec is deliberately distinct from the
    // nullable record-parent field in the persistent binding HAMT.
    let parent = match &fields[4] {
        ciborium::value::Value::Array(v) if v.len() == 1 && answer_u64(&v[0])? == 0 => None,
        ciborium::value::Value::Array(v) if v.len() == 2 && answer_u64(&v[0])? == 1 => {
            Some(answer_text(&v[1])?.to_owned())
        }
        _ => return Err(world_response_error(CompilerResponseRejection::Malformed)),
    };
    Ok(tidepool_repr::execution_schema::SymbolIdentity {
        unit: answer_text(&fields[0])?.to_owned(),
        module: answer_text(&fields[1])?.to_owned(),
        namespace: answer_text(&fields[2])?.to_owned(),
        occurrence: answer_text(&fields[3])?.to_owned(),
        record_parent: parent,
    })
}

/// Validate the native entry observation without confusing it with a captured
/// value's owner. The original-product issuer authenticates the generated
/// scaffold owner and native ordinal; the value-interface issuer independently
/// authenticates captured binders in their reserved Val module.
fn validate_world_native_entry_observation(
    symbol: &tidepool_repr::execution_schema::SymbolIdentity,
    typed_item: &crate::checked_cell::CheckedTypedSegmentItem,
) -> Result<(), CompileError> {
    let bounded = |value: &str| !value.is_empty() && value.len() <= 65536;
    if symbol.namespace != "value"
        || symbol.record_parent.is_some()
        || !bounded(&symbol.unit)
        || !bounded(&symbol.module)
        || !bounded(&symbol.namespace)
        || !bounded(&symbol.occurrence)
        || symbol.occurrence != typed_item.entry()
    {
        return Err(world_response_error(
            CompilerResponseRejection::ProvenanceMismatch,
        ));
    }
    Ok(())
}

/// The origin is part of the correlation key. A prior row may be demanded by
/// more than one later stage, and an authored alias may shadow a base binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum WorldCellRuntimeInputOrigin {
    BaseWorld,
    PriorStage { producer: usize, consumer: usize },
    CellOperand,
}

impl WorldCellRuntimeInputOrigin {
    fn decode(value: &ciborium::value::Value) -> Result<Self, CompileError> {
        let ciborium::value::Value::Array(fields) = value else {
            return Err(world_response_error(CompilerResponseRejection::Malformed));
        };
        match fields.as_slice() {
            [tag] if answer_u64(tag)? == 0 => Ok(Self::BaseWorld),
            [tag, producer, consumer] if answer_u64(tag)? == 1 => Ok(Self::PriorStage {
                producer: usize::try_from(answer_u64(producer)?)
                    .map_err(world_response_decode_error)?,
                consumer: usize::try_from(answer_u64(consumer)?)
                    .map_err(world_response_decode_error)?,
            }),
            [tag] if answer_u64(tag)? == 2 => Ok(Self::CellOperand),
            _ => Err(world_response_error(CompilerResponseRejection::Malformed)),
        }
    }
}

/// A row seal is parsed as a CID at ingress; its encoded form is retained for
/// exact observation framing. It cannot be confused with an interface digest.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedWorldRowSeal {
    cid: cid::Cid,
    encoded: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ParsedWorldInputSource {
    BaseWorld(ParsedWorldRowSeal),
    PriorStage {
        producer: usize,
        consumer: usize,
        interface: [u8; 32],
    },
    CellOperand(ParsedWorldRowSeal),
}

/// Decoded observations contain no checked row or producer authority.
#[derive(Clone, Debug, PartialEq, Eq)]
struct WorldInputRef {
    source: ParsedWorldInputSource,
    occurrence: String,
    symbol: tidepool_repr::execution_schema::SymbolIdentity,
    session_var: u64,
    defining_scope: u64,
    generation: u64,
}

/// Borrowed facts used only for ordering, comparison and receipt framing.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct WorldInputRefView<'a> {
    origin: WorldCellRuntimeInputOrigin,
    occurrence: &'a str,
    symbol: &'a tidepool_repr::execution_schema::SymbolIdentity,
    session_var: u64,
    defining_scope: u64,
    generation: u64,
    seal: &'a [u8],
}

impl PartialOrd for WorldInputRef {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for WorldInputRef {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.view().cmp(&other.view())
    }
}

impl WorldInputRef {
    fn decode(value: &ciborium::value::Value) -> Result<Self, CompileError> {
        let fields = answer_array(value, 7)?;
        let origin = WorldCellRuntimeInputOrigin::decode(&fields[0])?;
        let bytes = answer_blob(&fields[6])?;
        let source = match origin {
            WorldCellRuntimeInputOrigin::PriorStage { producer, consumer } => {
                ParsedWorldInputSource::PriorStage {
                    producer,
                    consumer,
                    interface: answer_hash(&fields[6])?,
                }
            }
            WorldCellRuntimeInputOrigin::BaseWorld | WorldCellRuntimeInputOrigin::CellOperand => {
                let seal = ParsedWorldRowSeal {
                    cid: cid::Cid::try_from(bytes).map_err(world_response_decode_error)?,
                    encoded: bytes.to_vec(),
                };
                if origin == WorldCellRuntimeInputOrigin::BaseWorld {
                    ParsedWorldInputSource::BaseWorld(seal)
                } else {
                    ParsedWorldInputSource::CellOperand(seal)
                }
            }
        };
        Ok(Self {
            source,
            occurrence: answer_text(&fields[1])?.to_owned(),
            symbol: world_output_symbol(&fields[2])?,
            session_var: answer_u64(&fields[3])?,
            defining_scope: answer_u64(&fields[4])?,
            generation: answer_u64(&fields[5])?,
        })
    }
    fn view(&self) -> WorldInputRefView<'_> {
        let (origin, seal): (_, &[u8]) = match &self.source {
            ParsedWorldInputSource::BaseWorld(seal) => {
                (WorldCellRuntimeInputOrigin::BaseWorld, &seal.encoded)
            }
            ParsedWorldInputSource::CellOperand(seal) => {
                (WorldCellRuntimeInputOrigin::CellOperand, &seal.encoded)
            }
            ParsedWorldInputSource::PriorStage {
                producer,
                consumer,
                interface,
            } => (
                WorldCellRuntimeInputOrigin::PriorStage {
                    producer: *producer,
                    consumer: *consumer,
                },
                interface,
            ),
        };
        WorldInputRefView {
            origin,
            occurrence: &self.occurrence,
            symbol: &self.symbol,
            session_var: self.session_var,
            defining_scope: self.defining_scope,
            generation: self.generation,
            seal,
        }
    }
    fn matches(
        &self,
        symbol: &tidepool_repr::execution_schema::SymbolIdentity,
        session_var: tidepool_repr::SessionVarId,
        defining_scope: u64,
        generation: u64,
    ) -> bool {
        self.symbol == *symbol
            && self.session_var == session_var.raw()
            && self.defining_scope == defining_scope
            && self.generation == generation
    }
}

/// The compiler reports why this exact acquired dependency was reached. The
/// tag does not grant source or native permission without its request owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DependencyTerminalOrigin {
    SourceDefinition,
    NativeOriginal,
}

/// Untrusted observations decoded from one emitted stage. In particular, a
/// retained value has no SessionVarId until the runtime binds the selected
/// occurrence to its own live value and custody.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct DependencyTerminalReference {
    origin: DependencyTerminalOrigin,
    original_use: crate::certified_products::ReceiptImportOwner,
    demander: crate::artifact_inventory::NativeGroupBinding,
    global_index: u32,
    binding: crate::artifact_inventory::ArtifactBinding,
    identity: tidepool_repr::execution_schema::SymbolIdentity,
    generation: u64,
}

impl DependencyTerminalReference {
    fn occurrence(&self) -> crate::declaration_context::NativeRetainedOccurrence<'_> {
        crate::declaration_context::NativeRetainedOccurrence {
            demander: self.demander,
            global_index: self.global_index,
            binding: &self.binding,
            identity: &self.identity,
            generation: self.generation,
        }
    }

    fn decode(value: &ciborium::value::Value) -> Result<Self, CompileError> {
        use crate::artifact_inventory::{
            ArtifactBinding, ArtifactId, ArtifactSelectionId, NativeGroupBinding, NativeGroupIssuer,
        };
        let fields = answer_array(value, 7)?;
        let origin = match answer_u64(&fields[0])? {
            0 => DependencyTerminalOrigin::SourceDefinition,
            1 => DependencyTerminalOrigin::NativeOriginal,
            _ => return Err(world_response_error(CompilerResponseRejection::Malformed)),
        };
        let decode_binding = |value: &ciborium::value::Value| {
            let fields = answer_array(value, 2)?;
            Ok::<_, CompileError>(ArtifactBinding {
                artifact: ArtifactId(answer_hash(&fields[0])?),
                selection: ArtifactSelectionId(answer_hash(&fields[1])?),
            })
        };
        // Dependency receipts use the certified-product identity codec. Its
        // nullable parent is distinct from WorldInputRef's execution option.
        let decode_identity = |value: &ciborium::value::Value| {
            let fields = answer_array(value, 5)?;
            Ok::<_, CompileError>(tidepool_repr::execution_schema::SymbolIdentity {
                unit: answer_text(&fields[0])?.to_owned(),
                module: answer_text(&fields[1])?.to_owned(),
                namespace: answer_text(&fields[2])?.to_owned(),
                occurrence: answer_text(&fields[3])?.to_owned(),
                record_parent: match &fields[4] {
                    ciborium::value::Value::Null => None,
                    parent => Some(answer_text(parent)?.to_owned()),
                },
            })
        };
        let original = answer_array(&fields[1], 5)?;
        let original_use = crate::certified_products::ReceiptImportOwner::OriginalBinding {
            unit: answer_text(&original[0])?.to_owned(),
            module: answer_text(&original[1])?.to_owned(),
            interface: decode_binding(&original[2])?,
            original: decode_binding(&original[3])?,
            binder: decode_identity(&original[4])?,
        };
        let demander = answer_array(&fields[2], 3)?;
        Ok(Self {
            origin,
            original_use,
            demander: NativeGroupBinding {
                binding: NativeGroupIssuer::from_observation(
                    ArtifactId(answer_hash(&demander[0])?),
                    ArtifactSelectionId(answer_hash(&demander[1])?),
                ),
                original_ordinal: u32::try_from(answer_u64(&demander[2])?)
                    .map_err(world_response_decode_error)?,
            },
            global_index: u32::try_from(answer_u64(&fields[3])?)
                .map_err(world_response_decode_error)?,
            binding: decode_binding(&fields[4])?,
            identity: decode_identity(&fields[5])?,
            generation: answer_u64(&fields[6])?,
        })
    }
}

/// Once-authenticated dependency rows for an actually emitted stage.
/// Rows retain immutable facts, never a backreference to their stage or handles.
#[derive(Debug)]
pub struct SealedStageDependencies {
    request: Arc<native_demand::DependencyRequestIdentity>,
    first_ordinal: usize,
    rows: Vec<SealedDependencyRow>,
}

#[derive(Debug)]
struct SealedDependencyRow {
    reference: DependencyTerminalReference,
    evidence: crate::declaration_context::RetainedValueEvidence,
}

impl SealedStageDependencies {
    pub fn terminals(
        self: &Arc<Self>,
    ) -> impl ExactSizeIterator<Item = SelectedDependencyTerminal> + '_ {
        (0..self.rows.len()).map(|index| SelectedDependencyTerminal {
            owner: Arc::clone(self),
            index,
        })
    }
}

/// An exact terminal selected by the issuing source or native dependency
/// owner of this request and stage. Runtime value authority remains separate.
#[derive(Clone, Debug)]
pub struct SelectedDependencyTerminal {
    owner: Arc<SealedStageDependencies>,
    index: usize,
}

impl SelectedDependencyTerminal {
    fn reference(&self) -> &DependencyTerminalReference {
        &self.owner.rows[self.index].reference
    }
    /// Compare immutable compiler evidence only. The receiving runtime must
    /// independently resolve and retain its actual scoped prepared value.
    pub fn matches_checked_value(&self, row: &CheckedValueArtifact) -> Result<bool, CompileError> {
        self.owner.rows[self.index]
            .evidence
            .matches_checked_value(row)
    }

    pub fn origin(&self) -> DependencyTerminalOrigin {
        self.reference().origin
    }
    pub fn original_use(&self) -> &crate::certified_products::ReceiptImportOwner {
        &self.reference().original_use
    }
    pub fn demander(&self) -> crate::artifact_inventory::NativeGroupBinding {
        self.reference().demander
    }
    pub fn global_index(&self) -> u32 {
        self.reference().global_index
    }
    pub fn binding(&self) -> crate::artifact_inventory::ArtifactBinding {
        self.reference().binding
    }
    pub fn identity(&self) -> &tidepool_repr::execution_schema::SymbolIdentity {
        &self.reference().identity
    }
    pub fn generation(&self) -> u64 {
        self.reference().generation
    }
    pub fn stage_first_ordinal(&self) -> usize {
        self.owner.first_ordinal
    }
}

struct LinkedWorldStageInputs {
    selected: Vec<WorldCellRuntimeInput>,
    rows: Vec<Arc<CheckedValueArtifact>>,
    session_bindings: Vec<tidepool_repr::SessionVarId>,
}

fn link_world_stage_inputs(
    decoded: &DecodedWorldCellStage,
    stage_table: &[DecodedWorldCellStage],
    outputs: &[Arc<WorldItemOutputEvidence>],
    captured_files: &BTreeMap<PathBuf, WorldOutputFile>,
    reservation: &crate::cell_plan::CompilerWorldCellReservation,
    operation: &tidepool_repr::execution_schema::InventoryOperation,
) -> Result<LinkedWorldStageInputs, CompileError> {
    operation
        .reserve::<WorldCellRuntimeInput>(decoded.selected_inputs.len())
        .and_then(|_| {
            operation.reserve::<(String, Arc<CheckedValueArtifact>)>(decoded.selected_inputs.len())
        })
        .and_then(|_| {
            operation.reserve::<tidepool_repr::SessionVarId>(decoded.selected_inputs.len())
        })
        .map_err(|e| CompileError::CompilerEvidence(Box::new(e.into())))?;
    let plan = reservation.plan();
    let world = plan.world();
    let mut rows = BTreeMap::<String, Arc<CheckedValueArtifact>>::new();
    let mut bindings = std::collections::BTreeSet::new();
    let mut selected_inputs = Vec::with_capacity(decoded.selected_inputs.len());
    for input in &decoded.selected_inputs {
        crate::host_work::checkpoint()?;
        let source = match &input.source {
            ParsedWorldInputSource::BaseWorld(seal) => {
                let actual = world
                    .restoration
                    .inputs
                    .bindings
                    .entries
                    .get(&input.occurrence)
                    .ok_or_else(|| {
                        world_response_error(CompilerResponseRejection::ProvenanceMismatch)
                    })?;
                let expected = world
                    .restoration
                    .bindings
                    .facts()
                    .get(&input.occurrence)
                    .ok_or_else(|| {
                        world_response_error(CompilerResponseRejection::ProvenanceMismatch)
                    })?;
                if !input.matches(
                    &actual.symbol,
                    actual.session_var,
                    actual.defining_scope,
                    actual.row.owner().gen.0,
                ) || seal.cid != expected.4
                {
                    return Err(world_response_error(
                        CompilerResponseRejection::ProvenanceMismatch,
                    ));
                }
                WorldCellRuntimeInputSource::BaseWorld {
                    binding: actual.clone(),
                    seal: seal.clone(),
                }
            }
            ParsedWorldInputSource::CellOperand(seal) => {
                let operand = plan.operand().ok_or_else(|| {
                    world_response_error(CompilerResponseRejection::ProvenanceMismatch)
                })?;
                let alias = match operand.exposure() {
                    WorldCellOperandExposure::Input => "input",
                    WorldCellOperandExposure::SessionInput => "sessionInput",
                };
                if input.occurrence != alias
                    || !input.matches(
                        operand.symbol(),
                        operand.session_var(),
                        operand.defining_scope(),
                        operand.generation(),
                    )
                    || seal.cid != *operand.row_fact_cid()
                {
                    return Err(world_response_error(
                        CompilerResponseRejection::ProvenanceMismatch,
                    ));
                }
                WorldCellRuntimeInputSource::CellOperand {
                    operand: Arc::clone(operand),
                    seal: seal.clone(),
                }
            }
            ParsedWorldInputSource::PriorStage {
                producer, consumer, ..
            } => {
                let output = outputs.get(*producer).ok_or_else(|| {
                    world_response_error(CompilerResponseRejection::ProvenanceMismatch)
                })?;
                let consumer = stage_table
                    .binary_search_by_key(consumer, |stage| stage.ordinals[0])
                    .ok()
                    .and_then(|index| stage_table.get(index))
                    .ok_or_else(|| {
                        world_response_error(CompilerResponseRejection::ProvenanceMismatch)
                    })?;
                WorldCellRuntimeInputSource::PriorStage(link_world_prior_input(
                    input,
                    output,
                    consumer,
                    decoded,
                    reservation.defining_scope(),
                    captured_files,
                )?)
            }
        };
        let selected = WorldCellRuntimeInput {
            occurrence: input.occurrence.clone(),
            source,
        };
        bindings.insert(selected.session_var().raw());
        if let Some(row) = selected.checked_row() {
            operation
                .charge(64)
                .map_err(|e| CompileError::CompilerEvidence(Box::new(e.into())))?;
            match rows.entry(row.owner().module_name()) {
                std::collections::btree_map::Entry::Occupied(old)
                    if !old.get().same_world_type_row(row) =>
                {
                    return Err(world_response_error(
                        CompilerResponseRejection::ProvenanceMismatch,
                    ));
                }
                std::collections::btree_map::Entry::Occupied(_) => {}
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(Arc::clone(row));
                }
            }
        }
        selected_inputs.push(selected);
    }
    Ok(LinkedWorldStageInputs {
        selected: selected_inputs,
        rows: rows.into_values().collect(),
        session_bindings: bindings
            .into_iter()
            .map(tidepool_repr::SessionVarId::from_extract)
            .collect(),
    })
}

fn admit_world_stage_inputs(
    decoded: DecodedWorldCellStage,
    linked: LinkedWorldStageInputs,
    dependencies: Arc<SealedStageDependencies>,
) -> Arc<CheckedWorldCellStage> {
    Arc::new(CheckedWorldCellStage {
        index: decoded.index,
        identity: decoded.identity,
        ordinals: decoded.ordinals,
        outputs: decoded.outputs,
        selected_inputs: linked.selected,
        dependencies,
        demanded_rows: linked.rows,
        demanded_session_bindings: linked.session_bindings,
    })
}

fn link_world_prior_input(
    input: &WorldInputRef,
    producer: &Arc<WorldItemOutputEvidence>,
    consumer: &DecodedWorldCellStage,
    enclosing: &DecodedWorldCellStage,
    defining_scope: u64,
    captured_files: &BTreeMap<PathBuf, WorldOutputFile>,
) -> Result<WorldPriorProducedInput, CompileError> {
    let ParsedWorldInputSource::PriorStage {
        producer: ordinal,
        consumer: consumer_ordinal,
        interface: seal,
    } = &input.source
    else {
        return Err(world_response_error(
            CompilerResponseRejection::ProvenanceMismatch,
        ));
    };
    let executable = producer
        .executable()
        .ok_or_else(|| world_response_error(CompilerResponseRejection::ProvenanceMismatch))?;
    if producer.index != *ordinal
        || consumer.ordinals.first() != Some(consumer_ordinal)
        || producer.stage_index >= consumer.index
        || consumer.index > enclosing.index
        || (consumer.index < enclosing.index
            && consumer.selected_inputs.binary_search(input).is_err())
        || input.defining_scope != defining_scope
        || input.generation != producer.generation
    {
        return Err(world_response_error(
            CompilerResponseRejection::ProvenanceMismatch,
        ));
    }
    let binder_index = executable
        .binders()
        .iter()
        .position(|binder| binder.name() == input.occurrence)
        .ok_or_else(|| world_response_error(CompilerResponseRejection::ProvenanceMismatch))?;
    let binder = &executable.binders()[binder_index];
    if binder.module()
        != tidepool_repr::SessionModule::val(tidepool_repr::Generation(producer.generation))
            .module_name()
        || input.session_var != binder.varid()
        || input.symbol.unit != "main"
        || input.symbol.module != binder.module()
        || input.symbol.namespace != "value"
        || input.symbol.occurrence != binder.name()
        || input.symbol.record_parent.is_some()
    {
        return Err(world_response_error(
            CompilerResponseRejection::ProvenanceMismatch,
        ));
    }
    let interface = captured_files
        .get(&PathBuf::from(format!("item-{ordinal}/value.hi")))
        .ok_or_else(|| world_response_error(CompilerResponseRejection::ProvenanceMismatch))?;
    if seal != interface.sha256() {
        return Err(world_response_error(
            CompilerResponseRejection::ProvenanceMismatch,
        ));
    }
    Ok(WorldPriorProducedInput {
        producer: Arc::clone(producer),
        consumer: *consumer_ordinal,
        binder_index,
        symbol: input.symbol.clone(),
        defining_scope,
        interface: interface.clone(),
    })
}

fn admit_world_preparation_response(
    origin: tidepool_extract_cmd::CompilerTransactionOrigin,
    request_correlation: String,
    reservation: Arc<crate::cell_plan::CompilerWorldCellReservation>,
    directory: Arc<tempfile::TempDir>,
    bytes: &[u8],
    mut validation: crate::recovery_artifacts::PackageInterfaceValidation,
    mut native_demands: native_demand::WorldDependencyObservations,
) -> Result<Result<WorldCellOutputAdmission, WorldQueryRejection>, CompileError> {
    if !Arc::ptr_eq(&reservation, &native_demands.reservation)
        || !Arc::ptr_eq(&validation.inventory, &native_demands.operation)
    {
        return Err(source_failure(
            "World native demand completion belongs to another request",
        ));
    }
    let packet = validation
        .inventory
        .decode_value(bytes, 8 << 20)
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
    let fields = answer_array(&packet, 9)?;
    let plan = reservation.plan();
    let base = plan.world().logical_id();
    let plan_digest = world_digest(plan.receipt());
    if answer_text(&fields[0])? != "TPCWPREP6"
        || answer_hash(&fields[1])? != *base
        || answer_hash(&fields[2])? != plan_digest
        || answer_hash(&fields[3])? != *reservation.admission_digest()
    {
        return Err(world_response_error(
            CompilerResponseRejection::ProvenanceMismatch,
        ));
    }
    let outcome = answer_array(&fields[4], 2)?;
    if answer_text(&outcome[0])? == "rejected" {
        if !answer_blob(&fields[5])?.is_empty()
            || !answer_array(&fields[6], 0)?.is_empty()
            || !answer_array(&fields[7], 0)?.is_empty()
            || !answer_array(&fields[8], 0)?.is_empty()
        {
            return Err(world_response_error(CompilerResponseRejection::Malformed));
        }
        return Ok(Err(match answer_u64(&outcome[1])? {
            0 => WorldQueryRejection::Unsupported,
            1 => WorldQueryRejection::Unavailable,
            2 => WorldQueryRejection::Corrupt,
            _ => return Err(world_response_error(CompilerResponseRejection::Malformed)),
        }));
    }
    if answer_text(&outcome[0])? != "prepared" {
        return Err(world_response_error(CompilerResponseRejection::Malformed));
    }
    validation
        .inventory
        .charge_value_copies(&fields[7], 1)
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
    let segments = crate::checked_cell::decode_world_typed_segment_plans(&fields[7], &reservation)?;
    validation
        .inventory
        .reserve::<Arc<crate::checked_cell::CheckedTypedSegmentPlan>>(segments.len())
        .and_then(|_| {
            validation
                .inventory
                .reserve::<(usize, (usize, usize))>(reservation.slots().len())
        })
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
    let segments: Vec<_> = segments.into_iter().map(Arc::new).collect();
    let by_ordinal: BTreeMap<_, _> = segments
        .iter()
        .enumerate()
        .flat_map(|(segment, plan)| {
            plan.items()
                .iter()
                .enumerate()
                .map(move |(item, planned)| (planned.ordinal(), (segment, item)))
        })
        .collect();
    let completed = answer_array(&outcome[1], reservation.slots().len())?;
    validation
        .inventory
        .reserve::<ciborium::value::Value>(completed.len())
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
    let common_files = world_output_manifest(&fields[6], &validation.inventory)?;
    let decoded_stages = decode_world_stages(
        &fields[8],
        &reservation.slots().collect::<Vec<_>>(),
        &segments,
        &common_files,
        &validation.inventory,
    )?;
    validation
        .inventory
        .reserve::<Arc<WorldItemOutputEvidence>>(completed.len())
        .and_then(|_| validation.inventory.reserve::<usize>(completed.len()))
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
    let stage_indices_by_ordinal: Vec<_> = decoded_stages
        .iter()
        .flat_map(|stage| stage.ordinals.iter().map(move |_| stage.index))
        .collect();
    let mut output_evidence = Vec::with_capacity(completed.len());
    let mut captured_files = BTreeMap::new();
    for (index, (completed, slot)) in completed.iter().zip(reservation.slots()).enumerate() {
        crate::host_work::checkpoint()?;
        let completed = answer_array(completed, 3)?;
        if answer_u64(&completed[0])? != index as u64
            || answer_u64(&completed[1])? != world_cell_generation(&slot)
        {
            return Err(world_response_error(
                CompilerResponseRejection::ProvenanceMismatch,
            ));
        }
        let item_path = PathBuf::from(format!("item-{index}/world-item.cbor"));
        let item_bytes = crate::certified_products::read_bounded_with_operation(
            &directory.path().join(&item_path),
            4 << 20,
            &validation.inventory,
        )
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
        validation
            .inventory
            .charge(item_bytes.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        if world_digest(&item_bytes) != answer_hash(&completed[2])? {
            return Err(world_response_error(
                CompilerResponseRejection::ProvenanceMismatch,
            ));
        }
        let item = validation
            .inventory
            .decode_value(&item_bytes, 4 << 20)
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let stage = &decoded_stages[stage_indices_by_ordinal[index]];
        let original = matches!(
            slot,
            crate::checked_cell::CheckedPlannedCellSlot::Prologue { .. }
                | crate::checked_cell::CheckedPlannedCellSlot::Declaration { .. }
        );
        let observed = answer_array(&item, if original { 8 } else { 13 })?;
        let source_index = if original { 6 } else { 7 };
        if answer_hash(&observed[1])? != *base
            || answer_hash(&observed[2])? != plan_digest
            || answer_hash(&observed[3])? != *reservation.admission_digest()
            || answer_u64(&observed[4])? != index as u64
            || answer_u64(&observed[5])? != world_cell_generation(&slot)
            || answer_hash(&observed[source_index])?
                != world_digest(plan.items()[index].source().as_bytes())
        {
            return Err(world_response_error(
                CompilerResponseRejection::ProvenanceMismatch,
            ));
        }
        let kind = if original {
            validation
                .inventory
                .charge_value_copies(&observed[7], 1)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            if answer_text(&observed[0])? != "TPCWORIGINAL1" {
                return Err(world_response_error(CompilerResponseRejection::Malformed));
            }
            let fields = answer_array(&observed[7], 5)?;
            let expected_kind = match slot {
                crate::checked_cell::CheckedPlannedCellSlot::Prologue { .. } => "prologue",
                _ => "declaration",
            };
            let owner = stage.identity.original_owner().ok_or_else(|| {
                world_response_error(CompilerResponseRejection::ProvenanceMismatch)
            })?;
            if answer_text(&fields[0])? != expected_kind
                || answer_text(&fields[1])? != owner.unit
                || answer_text(&fields[2])? != owner.module
            {
                return Err(world_response_error(
                    CompilerResponseRejection::ProvenanceMismatch,
                ));
            }
            let original = WorldOriginalItemProof {
                owner: owner.clone(),
                iface_sha256: answer_hash(&fields[3])?,
                inventory_json: answer_text(&fields[4])?.to_owned(),
            };
            if expected_kind == "prologue" {
                WorldItemProofKind::Prologue(original)
            } else {
                WorldItemProofKind::Declaration(original)
            }
        } else {
            let &(segment_index, item_index) = by_ordinal.get(&index).ok_or_else(|| {
                world_response_error(CompilerResponseRejection::ProvenanceMismatch)
            })?;
            let segment = &segments[segment_index];
            let typed_item = &segment.items()[item_index];
            if answer_text(&observed[0])? != "TPCWITEM2"
                || answer_text(&observed[6])? != segment.digest()
                || answer_text(&observed[8])? != typed_item.entry()
            {
                return Err(world_response_error(
                    CompilerResponseRejection::ProvenanceMismatch,
                ));
            }
            validation
                .inventory
                .charge_value_copies(&observed[9], 1)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            let symbol = world_output_symbol(&observed[9])?;
            validate_world_native_entry_observation(&symbol, typed_item)?;
            // Native original-group membership is authenticated per item by
            // CheckedTypedEntry against its exact selected original product.
            validation
                .inventory
                .charge_value_copies(&observed[10], 1)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            let native_ordinal =
                u32::try_from(answer_u64(&observed[10])?).map_err(world_response_decode_error)?;
            // Field 11 is the two-list `[binders, signatures]`. Binder order and
            // identity must match the reservation, and each signature must name
            // this typed item's entry and ordered capture.
            validation
                .inventory
                .charge_value_copies(&observed[11], 1)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            let evidence = answer_array(&observed[11], 2)?;
            let expected_bindings = reservation.items().get(index).ok_or_else(|| {
                world_response_error(CompilerResponseRejection::ProvenanceMismatch)
            })?;
            let binders = crate::checked_cell::decode_world_item_binders(
                &evidence[0],
                expected_bindings.bindings(),
            )?;
            let captures = typed_item.body().captures();
            let signatures = crate::checked_cell::decode_world_item_signatures(
                &evidence[1],
                typed_item.entry(),
                &captures,
            )?;
            let manifest = world_output_manifest(&observed[12], &validation.inventory)?;
            // One item owns exactly one prepared physical program output. The
            // writer emits `writePreparedArtifacts child [output]` per item.
            if manifest
                .keys()
                .filter(|path| {
                    path.to_str()
                        .is_some_and(|path| path.ends_with(".prepared.cbor"))
                })
                .count()
                != 1
            {
                return Err(world_response_error(CompilerResponseRejection::Malformed));
            }
            for (path, digest) in &manifest {
                validation
                    .inventory
                    .charge(path.as_os_str().len() + 32)
                    .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
                capture_world_output(
                    &directory,
                    PathBuf::from(format!("item-{index}")).join(path),
                    *digest,
                    &mut captured_files,
                    &mut validation,
                )?;
            }
            WorldItemProofKind::Executable(WorldExecutableItemProof {
                entry: typed_item.entry().to_owned(),
                native_ordinal,
                segment_digest: segment.digest().to_owned(),
                symbol,
                binders,
                signatures,
                manifest,
            })
        };
        validation
            .inventory
            .reserve::<(PathBuf, WorldOutputFile)>(1)
            .and_then(|_| validation.inventory.charge(item_bytes.len()))
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        captured_files.insert(
            item_path,
            WorldOutputFile {
                bytes: item_bytes.into(),
                sha256: answer_hash(&completed[2])?,
            },
        );
        output_evidence.push(Arc::new(WorldItemOutputEvidence {
            index,
            ordinal: index as u64,
            generation: world_cell_generation(&slot),
            stage_index: stage.index,
            kind,
        }));
    }
    let linked_inputs = decoded_stages
        .iter()
        .map(|stage| {
            link_world_stage_inputs(
                stage,
                &decoded_stages,
                &output_evidence,
                &captured_files,
                &reservation,
                &validation.inventory,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let stages = decoded_stages
        .into_iter()
        .zip(linked_inputs)
        .map(|(mut stage, linked)| {
            let dependencies = native_demands.seal_stage_dependencies(
                stage.ordinals[0],
                std::mem::take(&mut stage.dependency_terminals),
            )?;
            Ok(admit_world_stage_inputs(stage, linked, dependencies))
        })
        .collect::<Result<Vec<_>, CompileError>>()?;
    let native_demands = native_demands.bind_emitted_stages(&stages)?;
    validation
        .inventory
        .reserve::<Arc<WorldItemProof>>(output_evidence.len())
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
    let item_proofs = output_evidence
        .into_iter()
        .map(|output| {
            Arc::new(WorldItemProof {
                stage: Arc::clone(&stages[output.stage_index]),
                output,
            })
        })
        .collect();
    let mut demanded = BTreeMap::<String, Arc<CheckedValueArtifact>>::new();
    let mut demanded_session_bindings = std::collections::BTreeSet::new();
    for stage in &stages {
        validation
            .inventory
            .reserve::<u64>(stage.demanded_session_bindings().len())
            .and_then(|_| {
                validation
                    .inventory
                    .reserve::<(String, Arc<CheckedValueArtifact>)>(stage.demanded_rows().len())
            })
            .map_err(|e| CompileError::CompilerEvidence(Box::new(e.into())))?;
        demanded_session_bindings
            .extend(stage.demanded_session_bindings().iter().map(|id| id.raw()));
        for row in stage.demanded_rows() {
            validation
                .inventory
                .charge(64)
                .map_err(|e| CompileError::CompilerEvidence(Box::new(e.into())))?;
            match demanded.entry(row.owner().module_name()) {
                std::collections::btree_map::Entry::Occupied(old)
                    if !old.get().same_world_type_row(row) =>
                {
                    return Err(world_response_error(
                        CompilerResponseRejection::ProvenanceMismatch,
                    ));
                }
                std::collections::btree_map::Entry::Occupied(_) => {}
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(Arc::clone(row));
                }
            }
        }
    }

    for (path, digest) in &common_files {
        validation
            .inventory
            .charge(path.as_os_str().len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        capture_world_output(
            &directory,
            path.clone(),
            *digest,
            &mut captured_files,
            &mut validation,
        )?;
    }
    validation
        .inventory
        .charge_value_copies(&fields[5], 1)
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
    validation
        .inventory
        .reserve::<Arc<CheckedValueArtifact>>(demanded.len())
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
    Ok(Ok(WorldCellOutputAdmission {
        request_correlation,
        plan_receipt_sha256: plan_digest,
        origin,
        reservation,
        directory,
        observations: Arc::from(answer_blob(&fields[5])?),
        common_files,
        captured_files,
        validation,
        native_demands,
        segments,
        stages,
        item_proofs,
        demanded_rows: demanded.into_values().collect(),
        demanded_session_bindings: demanded_session_bindings
            .into_iter()
            .map(tidepool_repr::SessionVarId::from_extract)
            .collect(),
    }))
}

fn decode_world_stages(
    value: &ciborium::value::Value,
    slots: &[crate::checked_cell::CheckedPlannedCellSlot],
    segments: &[Arc<crate::checked_cell::CheckedTypedSegmentPlan>],
    manifest: &BTreeMap<PathBuf, [u8; 32]>,
    operation: &tidepool_repr::execution_schema::InventoryOperation,
) -> Result<Vec<DecodedWorldCellStage>, CompileError> {
    use crate::checked_cell::CheckedPlannedCellSlot as Slot;
    let ciborium::value::Value::Array(rows) = value else {
        return Err(world_response_error(CompilerResponseRejection::Malformed));
    };
    if rows.len() > slots.len() {
        return Err(world_response_error(CompilerResponseRejection::Malformed));
    }
    operation
        .charge_value_copies(value, 1)
        .and_then(|_| operation.reserve::<DecodedWorldCellStage>(rows.len()))
        .map_err(|e| CompileError::CompilerEvidence(Box::new(e.into())))?;
    let mut stages = Vec::with_capacity(rows.len());
    let mut next = 0usize;
    let mut next_segment = 0usize;
    for row in rows {
        let fields = answer_array(row, 5)?;
        let identity = WorldCellStageIdentity::decode(&fields[0])?;
        let ciborium::value::Value::Array(raw_ordinals) = &fields[1] else {
            return Err(world_response_error(CompilerResponseRejection::Malformed));
        };
        if raw_ordinals.is_empty() || raw_ordinals.len() > slots.len().saturating_sub(next) {
            return Err(world_response_error(
                CompilerResponseRejection::ProvenanceMismatch,
            ));
        }
        let ordinals = raw_ordinals
            .iter()
            .map(|value| usize::try_from(answer_u64(value)?).map_err(world_response_decode_error))
            .collect::<Result<Vec<_>, _>>()?;
        if ordinals.iter().copied().ne(next..next + ordinals.len()) {
            return Err(world_response_error(
                CompilerResponseRejection::ProvenanceMismatch,
            ));
        }
        match &identity {
            WorldCellStageIdentity::Executable { typed_digest } => {
                let segment = segments.get(next_segment).ok_or_else(|| {
                    world_response_error(CompilerResponseRejection::ProvenanceMismatch)
                })?;
                if typed_digest != segment.digest()
                    || segment
                        .items()
                        .iter()
                        .map(|item| item.ordinal())
                        .ne(ordinals.iter().copied())
                {
                    return Err(world_response_error(
                        CompilerResponseRejection::ProvenanceMismatch,
                    ));
                }
                next_segment += 1;
            }
            WorldCellStageIdentity::Declaration { ordinal, owner }
            | WorldCellStageIdentity::Prologue { ordinal, owner } => {
                if ordinals.len() != 1 || *ordinal != next as u64 {
                    return Err(world_response_error(
                        CompilerResponseRejection::ProvenanceMismatch,
                    ));
                }
                let generation = match (slots.get(next), &identity) {
                    (
                        Some(Slot::Declaration { declaration }),
                        WorldCellStageIdentity::Declaration { .. },
                    )
                    | (
                        Some(Slot::Prologue { declaration }),
                        WorldCellStageIdentity::Prologue { .. },
                    ) => *declaration,
                    _ => {
                        return Err(world_response_error(
                            CompilerResponseRejection::ProvenanceMismatch,
                        ))
                    }
                };
                let expected =
                    tidepool_repr::SessionModule::lib(tidepool_repr::Generation(generation))
                        .module_name();
                if owner.unit != "main" || owner.module != expected {
                    return Err(world_response_error(
                        CompilerResponseRejection::ProvenanceMismatch,
                    ));
                }
            }
        }
        let bundle = answer_array(&fields[2], 3)?;
        let namespace = PathBuf::from(format!("stage-{next}"));
        let decode_ref = |value: &ciborium::value::Value,
                          expected: &str|
         -> Result<WorldStageFileRef, CompileError> {
            let fields = answer_array(value, 2)?;
            let relative = PathBuf::from(answer_text(&fields[0])?);
            relative_source_path(&relative).map_err(world_response_decode_error)?;
            let sha256 = answer_hash(&fields[1])?;
            if relative != namespace.join(expected) || manifest.get(&relative) != Some(&sha256) {
                return Err(world_response_error(
                    CompilerResponseRejection::ProvenanceMismatch,
                ));
            }
            Ok(WorldStageFileRef { relative, sha256 })
        };
        let outputs = WorldStageOutputs {
            source_receipt: decode_ref(&bundle[0], "world-source.cbor")?,
            module_products: decode_ref(&bundle[1], "module-products.cbor")?,
            certified_products: decode_ref(&bundle[2], "certified-products.cbor")?,
        };
        let ciborium::value::Value::Array(raw_inputs) = &fields[3] else {
            return Err(world_response_error(CompilerResponseRejection::Malformed));
        };
        operation
            .reserve::<WorldInputRef>(raw_inputs.len())
            .map_err(|e| CompileError::CompilerEvidence(Box::new(e.into())))?;
        let mut selected_inputs = raw_inputs
            .iter()
            .map(WorldInputRef::decode)
            .collect::<Result<Vec<_>, _>>()?;
        selected_inputs.sort_unstable();
        if selected_inputs.windows(2).any(|pair| {
            pair[0].view().origin == pair[1].view().origin
                && pair[0].occurrence == pair[1].occurrence
        }) {
            return Err(world_response_error(
                CompilerResponseRejection::ProvenanceMismatch,
            ));
        }
        let ciborium::value::Value::Array(raw_terminals) = &fields[4] else {
            return Err(world_response_error(CompilerResponseRejection::Malformed));
        };
        operation
            .reserve::<DependencyTerminalReference>(raw_terminals.len())
            .map_err(|e| CompileError::CompilerEvidence(Box::new(e.into())))?;
        let mut dependency_terminals = raw_terminals
            .iter()
            .map(DependencyTerminalReference::decode)
            .collect::<Result<Vec<_>, _>>()?;
        dependency_terminals.sort_unstable();
        if dependency_terminals
            .windows(2)
            .any(|pair| pair[0] == pair[1])
        {
            return Err(world_response_error(
                CompilerResponseRejection::ProvenanceMismatch,
            ));
        }
        next += ordinals.len();
        stages.push(DecodedWorldCellStage {
            index: stages.len(),
            identity,
            ordinals,
            outputs,
            selected_inputs,
            dependency_terminals,
        });
    }
    if next != slots.len() || next_segment != segments.len() {
        return Err(world_response_error(
            CompilerResponseRejection::ProvenanceMismatch,
        ));
    }
    Ok(stages)
}

fn capture_world_output(
    root: &tempfile::TempDir,
    relative: PathBuf,
    digest: [u8; 32],
    files: &mut BTreeMap<PathBuf, WorldOutputFile>,
    validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
) -> Result<(), CompileError> {
    relative_source_path(&relative).map_err(world_response_decode_error)?;
    if files.contains_key(&relative) {
        return Err(world_response_error(CompilerResponseRejection::Malformed));
    }
    validation
        .inventory
        .reserve::<(PathBuf, WorldOutputFile)>(1)
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
    validation
        .inventory
        .charge(root.path().as_os_str().len() + relative.as_os_str().len() + 1)
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
    let path = root.path().join(&relative);
    let bytes = crate::certified_products::read_bounded_with_operation(
        &path,
        validation.inventory.limits().max_bytes as u64,
        &validation.inventory,
    )
    .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
    validation
        .inventory
        .charge(bytes.len().checked_mul(2).ok_or_else(source_size_failure)?)
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
    if world_digest(&bytes) != digest {
        return Err(world_response_error(
            CompilerResponseRejection::ProvenanceMismatch,
        ));
    }
    validation
        .record_captured_bytes(&path, &bytes)
        .map_err(|error| source_failure(error.to_string()))?;
    files.insert(
        relative,
        WorldOutputFile {
            bytes: bytes.into(),
            sha256: digest,
        },
    );
    Ok(())
}

fn query_world_inner(
    identity: &tidepool_extract_cmd::CompilerIdentity,
    world: Arc<CompilerWorld>,
    question: PublicModuleQuestion,
) -> Result<WorldQueryResult, CompileError> {
    let wire = match question.kind {
        PublicModuleQuestionKind::StructuredInfo => {
            tidepool_extract_cmd::WorldQueryQuestion::PublicInfo(question.query.clone())
        }
        PublicModuleQuestionKind::StructuredType => {
            tidepool_extract_cmd::WorldQueryQuestion::PublicType(question.query.clone())
        }
    };
    let bytes = query_world_transport(identity, &world, wire)?;
    decode_world_answer(world, question, &bytes)
}

fn query_world_transport(
    identity: &tidepool_extract_cmd::CompilerIdentity,
    world: &Arc<CompilerWorld>,
    question: tidepool_extract_cmd::WorldQueryQuestion,
) -> Result<Vec<u8>, CompileError> {
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
    let (mut cmd, endpoint) = world_command(identity, None)?;
    cmd.world_query(tidepool_extract_cmd::WorldQueryRequest {
        world: world_hex(world.logical_id()),
        realization,
        question,
        output: output.clone(),
    });
    let run = world_execute(&cmd, endpoint, Vec::new())?;
    crate::diag::decode_extract_result(run.success(), &run.output.stdout, &run.output.stderr)?;
    let file = std::fs::File::open(&output).map_err(world_response_io_error)?;
    let allowance = capture_allowance()?;
    if file.metadata()?.len() > allowance {
        return Err(world_response_error(CompilerResponseRejection::Malformed));
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    crate::host_work::read_to_end(&mut file.take(allowance.saturating_add(1)), &mut bytes)?;
    if bytes.len() as u64 > allowance {
        return Err(world_response_error(CompilerResponseRejection::Malformed));
    }
    tracing::debug!(
        reply_bytes = bytes.len(),
        allowance,
        "compiler World query reply consumed"
    );
    Ok(bytes)
}

fn world_response_error(reason: CompilerResponseRejection) -> CompileError {
    CompileError::CompilerEndpoint(tidepool_extract_cmd::reject_compiler_response(reason))
}

fn world_response_decode_error(
    cause: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> CompileError {
    CompileError::CompilerEndpoint(tidepool_extract_cmd::reject_compiler_response_with_cause(
        CompilerResponseRejection::Malformed,
        std::io::Error::new(std::io::ErrorKind::InvalidData, cause),
    ))
}

fn world_response_io_error(cause: std::io::Error) -> CompileError {
    CompileError::CompilerEndpoint(tidepool_extract_cmd::reject_compiler_response_with_cause(
        CompilerResponseRejection::UnreadableOutput,
        cause,
    ))
}

fn world_answer_decode_error(
    cause: tidepool_extract_report::world_query::WorldAnswerDecodeError,
) -> CompileError {
    use tidepool_extract_cmd::CompilerResponseRejection as R;
    use tidepool_extract_report::world_query::WorldAnswerDecodeError as E;
    let reason = match &cause {
        E::Malformed(_) => R::Malformed,
        E::ProvenanceMismatch(_) => R::ProvenanceMismatch,
    };
    CompileError::CompilerEndpoint(tidepool_extract_cmd::reject_compiler_response_with_cause(
        reason,
        std::io::Error::new(std::io::ErrorKind::InvalidData, cause),
    ))
}

fn answer_array(
    value: &ciborium::value::Value,
    count: usize,
) -> Result<&[ciborium::value::Value], CompileError> {
    match value {
        ciborium::value::Value::Array(v) if v.len() == count => Ok(v),
        _ => Err(world_response_error(CompilerResponseRejection::Malformed)),
    }
}
fn answer_text(value: &ciborium::value::Value) -> Result<&str, CompileError> {
    match value {
        ciborium::value::Value::Text(v) => Ok(v),
        _ => Err(world_response_error(CompilerResponseRejection::Malformed)),
    }
}
fn answer_u64(value: &ciborium::value::Value) -> Result<u64, CompileError> {
    match value {
        ciborium::value::Value::Integer(v) => {
            u64::try_from(*v).map_err(world_response_decode_error)
        }
        _ => Err(world_response_error(CompilerResponseRejection::Malformed)),
    }
}
fn answer_blob(value: &ciborium::value::Value) -> Result<&[u8], CompileError> {
    match value {
        ciborium::value::Value::Bytes(v) => Ok(v),
        _ => Err(world_response_error(CompilerResponseRejection::Malformed)),
    }
}
fn answer_hash(value: &ciborium::value::Value) -> Result<[u8; 32], CompileError> {
    answer_blob(value)?
        .try_into()
        .map_err(world_response_decode_error)
}
fn answer_cbor(bytes: &[u8]) -> Result<ciborium::value::Value, CompileError> {
    let mut input = std::io::Cursor::new(bytes);
    let value = ciborium::de::from_reader(&mut input).map_err(world_response_decode_error)?;
    if input.position() != bytes.len() as u64 {
        return Err(world_response_error(CompilerResponseRejection::Malformed));
    }
    Ok(value)
}
#[derive(Clone, Copy)]
enum ModuleReceiptUse {
    Requested,
    Defining,
}

fn authenticate_module_receipt(
    receipt: &WorldModuleReceipt,
    sources: &CapturedCompilerSources,
    expected_profile: &[u8; 32],
    receipt_use: ModuleReceiptUse,
) -> Result<(), CompileError> {
    match &receipt.origin {
        WorldModuleOrigin::Package { profile } => {
            if profile != expected_profile
                || (matches!(receipt_use, ModuleReceiptUse::Requested)
                    && sources.selected_sources.contains_key(&receipt.module))
            {
                return Err(world_response_error(
                    CompilerResponseRejection::ProvenanceMismatch,
                ));
            }
        }
        origin @ WorldModuleOrigin::Home { path, .. } => {
            if relative_source_path(Path::new(path)).is_err()
                || sources.selected_sources.get(&receipt.module) != Some(origin)
            {
                return Err(world_response_error(
                    CompilerResponseRejection::ProvenanceMismatch,
                ));
            }
        }
    }
    Ok(())
}

fn decode_world_answer(
    world: Arc<CompilerWorld>,
    question: PublicModuleQuestion,
    bytes: &[u8],
) -> Result<WorldQueryResult, CompileError> {
    use crate::inspection::IdentifierNamespace as N;
    use tidepool_extract_report::world_query::{self, WorldQueryExpectation, WorldQueryKind};
    let namespace = match question.namespace() {
        InspectionNamespace::Any => None,
        InspectionNamespace::Value => Some(N::Value),
        InspectionNamespace::Type => Some(N::Type),
        InspectionNamespace::Constructor => Some(N::Constructor),
        InspectionNamespace::Field => Some(N::Field),
    };
    let answer = world_query::decode_world_answer(
        bytes,
        &WorldQueryExpectation {
            world: world.logical_id(),
            kind: match question.kind() {
                PublicModuleQuestionKind::StructuredInfo => WorldQueryKind::Info,
                PublicModuleQuestionKind::StructuredType => WorldQueryKind::Type,
            },
            module: question.module(),
            namespace,
            name: question.name(),
            generation: question.generation(),
            fingerprint: question.fingerprint(),
        },
    )
    .map_err(world_answer_decode_error)?;
    let sources = &world.restoration.inputs.sources;
    let profile = &world.restoration.profile;
    match &answer {
        WorldAnswer::Info {
            requested,
            defining,
            ..
        }
        | WorldAnswer::Type {
            requested,
            defining,
            ..
        } => {
            authenticate_module_receipt(requested, sources, profile, ModuleReceiptUse::Requested)?;
            authenticate_module_receipt(defining, sources, profile, ModuleReceiptUse::Defining)?;
        }
        WorldAnswer::NameRefused { requested, .. } => {
            authenticate_module_receipt(requested, sources, profile, ModuleReceiptUse::Requested)?
        }
        WorldAnswer::Absent(absence) => {
            if sources.selected_sources.contains_key(&absence.module)
                || absence.source_domain != *sources.seal()
                || absence.package_profile != *profile
            {
                return Err(world_response_error(
                    CompilerResponseRejection::ProvenanceMismatch,
                ));
            }
        }
        WorldAnswer::Rejected(_) => {}
    }
    Ok(WorldQueryResult {
        world,
        question,
        answer,
    })
}

/// Re-establish an issued logical world from its retained restoration owner.
/// This rehydrates compiler facts; it never runs notebook cells or effects.
pub fn restore_world(
    restoration: &Arc<CompilerWorldRestoration>,
    endpoint: &crate::toolchain::AdmittedCompilerEndpoint,
    question: PublicModuleQuestion,
) -> Result<WorldQueryResult, CompileError> {
    let world = Arc::new(CompilerWorld {
        restoration: Arc::clone(restoration),
        realization: Arc::new(Mutex::new(None)),
        position: None,
    });
    query_public_module(endpoint, &world, question)
}

#[cfg(test)]
mod answer_tests {
    use super::*;
    fn assert_response_rejection<T>(
        result: Result<T, CompileError>,
        expected: CompilerResponseRejection,
    ) -> tidepool_extract_cmd::SpawnError {
        let Err(CompileError::CompilerEndpoint(error)) = result else {
            panic!("expected a typed accepted-response refusal: {expected:?}");
        };
        assert_eq!(error.source.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(error.response_rejection(), Some(expected));
        assert!(!error.definitely_unsubmitted());
        assert!(!error.permits_rebind());
        error
    }

    #[test]
    fn response_rejections_preserve_typed_cause_and_uncertain_settlement() {
        use tidepool_extract_cmd::CompilerResponseRejection as R;
        for expected in [R::Malformed, R::ProvenanceMismatch] {
            let CompileError::CompilerEndpoint(error) = world_response_error(expected) else {
                panic!("response refusal must retain endpoint cause");
            };
            assert_eq!(error.source.kind(), std::io::ErrorKind::InvalidData);
            assert_eq!(
                error
                    .source
                    .get_ref()
                    .and_then(|cause| cause.downcast_ref::<R>()),
                Some(&expected),
            );
            assert!(!error.definitely_unsubmitted());
            assert!(!error.permits_rebind());
        }
    }

    #[test]
    fn malformed_cbor_retains_decoder_cause_and_refuses_replay() {
        let error = answer_cbor(&[0xff]).expect_err("invalid CBOR must not become absence");
        let CompileError::CompilerEndpoint(rejection) = error else {
            panic!("decoder refusal must preserve endpoint settlement");
        };
        let failure = rejection
            .source
            .get_ref()
            .and_then(|cause| cause.downcast_ref::<tidepool_extract_cmd::CompilerResponseFailure>())
            .expect("response boundary reason");
        assert_eq!(failure.reason(), CompilerResponseRejection::Malformed);
        assert!(failure
            .cause()
            .get_ref()
            .is_some_and(|cause| cause.is::<ciborium::de::Error<std::io::Error>>()));
        assert!(!rejection.definitely_unsubmitted());
        assert!(!rejection.permits_rebind());
    }

    #[test]
    fn unreadable_response_preserves_missing_and_inaccessible_output() {
        for kind in [
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::PermissionDenied,
        ] {
            let error = world_response_io_error(std::io::Error::from(kind));
            let CompileError::CompilerEndpoint(rejection) = error else {
                panic!("unreadable accepted output must refuse");
            };
            assert_eq!(rejection.source.kind(), kind);
            let failure = rejection
                .source
                .get_ref()
                .and_then(|cause| {
                    cause.downcast_ref::<tidepool_extract_cmd::CompilerResponseFailure>()
                })
                .expect("response boundary reason");
            assert_eq!(
                failure.reason(),
                CompilerResponseRejection::UnreadableOutput
            );
            assert_eq!(failure.cause().kind(), kind);
            assert!(!rejection.definitely_unsubmitted());
            assert!(!rejection.permits_rebind());
        }
    }

    pub(super) fn assert_captured_stage_output_refusals(program: &CheckedWorldCellProgram) {
        use ciborium::value::Value;
        use tidepool_extract_cmd::CompilerResponseRejection as Rejection;

        // A malformed accepted response poisons an active transaction. These
        // controls inspect a completed producer's bytes, outside that scope.
        assert!(tidepool_extract_cmd::CompilerTransactionOrigin::current().is_none());
        let bytes = std::fs::read(program.directory().path().join("cell-prepared.cbor"))
            .expect("actual retained preparation output");
        let output_evidence = program
            .output
            .item_proofs
            .iter()
            .map(|proof| Arc::clone(&proof.output))
            .collect::<Vec<_>>();
        let slots = program.reservation().slots().collect::<Vec<_>>();
        let replay = |bytes: &[u8]| -> Result<(), CompileError> {
            let packet = program
                .output
                .validation
                .inventory
                .decode_value(bytes, 8 << 20)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            let fields = answer_array(&packet, 9)?;
            let stages = decode_world_stages(
                &fields[8],
                &slots,
                &program.output.segments,
                &program.output.common_files,
                &program.output.validation.inventory,
            )?;
            for stage in &stages {
                link_world_stage_inputs(
                    stage,
                    &stages,
                    &output_evidence,
                    &program.output.captured_files,
                    program.reservation(),
                    &program.output.validation.inventory,
                )?;
            }
            Ok(())
        };
        replay(&bytes).expect("actual retained stage table and linked inputs");
        let packet = answer_cbor(&bytes).expect("actual preparation packet");
        let fields = answer_array(&packet, 9).expect("actual PREP6 fields");
        let Value::Array(stages) = &fields[8] else {
            panic!("actual preparation stage table");
        };
        assert!(stages.len() >= 2, "requires a genuine multi-stage producer");
        let last = stages.len() - 1;
        let bundle_ref = |stage: usize, role: usize| {
            answer_array(&answer_array(&stages[stage], 5).unwrap()[2], 3).unwrap()[role].clone()
        };
        let assert_refused = |mutated: Value, expected: Rejection| {
            // All correlation, reservation, manifest and item fields are the
            // unchanged actual output. Only the named stage bundle is altered.
            assert_eq!(&answer_array(&mutated, 9).unwrap()[..8], &fields[..8]);
            let mut encoded = Vec::new();
            ciborium::ser::into_writer(&mutated, &mut encoded).unwrap();
            assert_response_rejection(replay(&encoded), expected);
        };
        for stage in [0, last] {
            for role in 0..3 {
                let mut missing = packet.clone();
                let Value::Array(fields) = &mut missing else {
                    unreachable!()
                };
                let Value::Array(stages) = &mut fields[8] else {
                    unreachable!()
                };
                let Value::Array(row) = &mut stages[stage] else {
                    unreachable!()
                };
                let Value::Array(bundle) = &mut row[2] else {
                    unreachable!()
                };
                bundle.remove(role);
                assert_refused(missing, Rejection::Malformed);

                let mut crossed = packet.clone();
                let Value::Array(fields) = &mut crossed else {
                    unreachable!()
                };
                let Value::Array(stages) = &mut fields[8] else {
                    unreachable!()
                };
                let Value::Array(row) = &mut stages[stage] else {
                    unreachable!()
                };
                let Value::Array(bundle) = &mut row[2] else {
                    unreachable!()
                };
                bundle[role] = bundle_ref(if stage == 0 { last } else { 0 }, role);
                assert_refused(crossed, Rejection::ProvenanceMismatch);
            }
        }
        let (prior_stage, prior_input) = stages
            .iter()
            .enumerate()
            .find_map(|(stage_index, stage)| {
                let stage = answer_array(stage, 5).unwrap();
                let Value::Array(inputs) = &stage[3] else {
                    unreachable!()
                };
                inputs.iter().enumerate().find_map(|(index, input)| {
                    let reference = WorldInputRef::decode(input).unwrap();
                    matches!(reference.source, ParsedWorldInputSource::PriorStage { .. })
                        .then_some((stage_index, index))
                })
            })
            .expect("actual historical producer fixture selects a prior value");
        for mutation in 0..5 {
            let mut altered = packet.clone();
            let Value::Array(fields) = &mut altered else {
                unreachable!()
            };
            let Value::Array(stages) = &mut fields[8] else {
                unreachable!()
            };
            let Value::Array(stage) = &mut stages[prior_stage] else {
                unreachable!()
            };
            let Value::Array(inputs) = &mut stage[3] else {
                unreachable!()
            };
            let Value::Array(reference) = &mut inputs[prior_input] else {
                unreachable!()
            };
            match mutation {
                0 => {
                    let Value::Array(origin) = &mut reference[0] else {
                        unreachable!()
                    };
                    origin[1] = (slots.len() as u64).into();
                }
                1 => reference[1] = world_text("missingProducerBinder"),
                2 => reference[4] = answer_u64(&reference[4]).unwrap().saturating_add(1).into(),
                3 => reference[5] = answer_u64(&reference[5]).unwrap().saturating_add(1).into(),
                _ => reference[6] = world_blob([5; 31]),
            }
            assert_refused(
                altered,
                if mutation == 4 {
                    Rejection::Malformed
                } else {
                    Rejection::ProvenanceMismatch
                },
            );
        }
        eprintln!("actual stage decoder/link controls: 1 baseline, 12 output and 5 prior-input typed refusals");
    }
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
    fn home(root: u32, body: &[u8]) -> WorldModuleReceipt {
        WorldModuleReceipt {
            unit: "main".into(),
            module: "Module".into(),
            origin: WorldModuleOrigin::Home {
                root,
                path: "Module.hs".into(),
                source_sha256: world_digest(body),
            },
        }
    }
    #[test]
    fn dependency_terminal_wire_preserves_occurrence_and_refuses_other_identity_codecs() {
        use ciborium::value::Value;
        let integer = |n: u64| Value::Integer(n.into());
        let binding = |artifact, selection| {
            world_array(vec![
                world_blob([artifact; 32]),
                world_blob([selection; 32]),
            ])
        };
        let symbol = world_array(vec![
            world_text("main"),
            world_text("Tidepool.Session.Val.G7"),
            world_text("value"),
            world_text("captured"),
            Value::Null,
        ]);
        let packet = world_array(vec![
            integer(0),
            world_array(vec![
                world_text("main"),
                world_text("Source"),
                binding(1, 2),
                binding(3, 4),
                symbol.clone(),
            ]),
            world_array(vec![world_blob([5; 32]), world_blob([6; 32]), integer(9)]),
            integer(2),
            binding(7, 8),
            symbol,
            integer(7),
        ]);
        let decoded = DependencyTerminalReference::decode(&packet).unwrap();
        assert_eq!(decoded.origin, DependencyTerminalOrigin::SourceDefinition);
        assert_eq!(decoded.demander.binding.artifact().0, [5; 32]);
        assert_eq!(decoded.demander.binding.selection().0, [6; 32]);
        assert_eq!(decoded.demander.original_ordinal, 9);
        assert_eq!(decoded.global_index, 2);
        assert_eq!(decoded.binding.artifact.0, [7; 32]);
        assert_eq!(decoded.binding.selection.0, [8; 32]);
        assert_eq!(decoded.generation, 7);
        let commitment = |terminal: &DependencyTerminalReference| {
            let mut digest = blake3::Hasher::new();
            frame_dependency_terminal(terminal, &mut |bytes| {
                digest.update(&(bytes.len() as u64).to_le_bytes());
                digest.update(bytes);
            });
            *digest.finalize().as_bytes()
        };
        let original_commitment = commitment(&decoded);
        for field in 0..5 {
            let mut different = decoded.clone();
            match field {
                0 => different.origin = DependencyTerminalOrigin::NativeOriginal,
                1 => different.demander.original_ordinal += 1,
                2 => different.global_index += 1,
                3 => different.binding.selection.0[0] ^= 1,
                _ => {
                    let crate::certified_products::ReceiptImportOwner::OriginalBinding {
                        original,
                        ..
                    } = &mut different.original_use
                    else {
                        unreachable!()
                    };
                    original.selection.0[0] ^= 1;
                }
            }
            assert_ne!(original_commitment, commitment(&different));
        }
        for (field, replacement) in [
            (0, integer(2)),
            (
                2,
                world_array(vec![world_blob([5; 32]), world_blob([6; 32])]),
            ),
            (3, integer(u64::MAX)),
            (
                4,
                world_array(vec![world_blob([7; 31]), world_blob([8; 32])]),
            ),
        ] {
            let Value::Array(mut fields) = packet.clone() else {
                unreachable!()
            };
            fields[field] = replacement;
            assert!(DependencyTerminalReference::decode(&world_array(fields)).is_err());
        }
        let Value::Array(mut fields) = packet else {
            unreachable!()
        };
        let Value::Array(identity) = &mut fields[5] else {
            unreachable!()
        };
        identity[4] = world_array(vec![integer(0)]);
        assert!(DependencyTerminalReference::decode(&world_array(fields)).is_err());
    }

    #[test]
    fn input_refs_keep_closed_origins_and_actual_prior_stage_scope_distinct() {
        use ciborium::value::Value;
        let integer = |value: u64| Value::Integer(value.into());
        let symbol = tidepool_repr::execution_schema::SymbolIdentity {
            unit: "main".into(),
            module: "Tidepool.Session.Val.G7".into(),
            namespace: "value".into(),
            occurrence: "x".into(),
            record_parent: None,
        };
        let encoded_symbol = world_array(vec![
            world_text("main"),
            world_text("Tidepool.Session.Val.G7"),
            world_text("value"),
            world_text("x"),
            world_array(vec![integer(0)]),
        ]);
        let packet = world_array(vec![
            world_array(vec![integer(1), integer(0), integer(2)]),
            world_text("x"),
            encoded_symbol,
            integer(71),
            integer(900),
            integer(7),
            world_blob([5; 32]),
        ]);
        let input = WorldInputRef::decode(&packet).unwrap();
        assert_eq!(
            input.view().origin,
            WorldCellRuntimeInputOrigin::PriorStage {
                producer: 0,
                consumer: 2
            }
        );
        assert!(input.matches(
            &symbol,
            tidepool_repr::SessionVarId::from_extract(71),
            900,
            7
        ));
        assert!(!input.matches(
            &symbol,
            tidepool_repr::SessionVarId::from_extract(71),
            100,
            7
        ));
        let file = |name: &str| WorldStageFileRef {
            relative: PathBuf::from(name),
            sha256: [1; 32],
        };
        let stage = |index, ordinal| DecodedWorldCellStage {
            index,
            identity: WorldCellStageIdentity::Executable {
                typed_digest: "11".repeat(32),
            },
            ordinals: vec![ordinal],
            outputs: WorldStageOutputs {
                source_receipt: file("source"),
                module_products: file("modules"),
                certified_products: file("certified"),
            },
            selected_inputs: vec![],
            dependency_terminals: vec![],
        };
        let consumer = stage(1, 2);
        // DATA for the correlation check, without a checked-program issuer or
        // native/interface authority. Joined producer tests own that evidence.
        let binders = crate::checked_cell::decode_world_item_binders(
            &world_array(vec![world_array(vec![
                world_text("x"),
                integer(71),
                world_text("Tidepool.Session.Val.G7"),
                world_text("ForceData"),
                world_text("Int"),
                Value::Null,
                Value::Null,
            ])]),
            &[("x".into(), tidepool_repr::SessionVarId::from_extract(71))],
        )
        .unwrap();
        let producer = Arc::new(WorldItemOutputEvidence {
            index: 0,
            ordinal: 0,
            generation: 7,
            stage_index: 0,
            kind: WorldItemProofKind::Executable(WorldExecutableItemProof {
                entry: "entry".into(),
                native_ordinal: 4,
                segment_digest: "11".repeat(32),
                symbol: symbol.clone(),
                binders,
                signatures: vec![],
                manifest: BTreeMap::new(),
            }),
        });
        let files = BTreeMap::from([(
            PathBuf::from("item-0/value.hi"),
            WorldOutputFile {
                bytes: Arc::from(&b"observed"[..]),
                sha256: [5; 32],
            },
        )]);
        assert!(
            link_world_prior_input(&input, &producer, &consumer, &consumer, 900, &files).is_ok()
        );
        assert_response_rejection(
            link_world_prior_input(&input, &producer, &consumer, &consumer, 100, &files),
            CompilerResponseRejection::ProvenanceMismatch,
        );
        assert_response_rejection(
            link_world_prior_input(&input, &producer, &stage(0, 2), &consumer, 900, &files),
            CompilerResponseRejection::ProvenanceMismatch,
        );
        assert_response_rejection(
            link_world_prior_input(&input, &producer, &stage(1, 1), &consumer, 900, &files),
            CompilerResponseRejection::ProvenanceMismatch,
        );
        assert_response_rejection(
            link_world_prior_input(
                &input,
                &producer,
                &consumer,
                &consumer,
                900,
                &BTreeMap::new(),
            ),
            CompilerResponseRejection::ProvenanceMismatch,
        );
        let wrong = BTreeMap::from([(
            PathBuf::from("item-0/value.hi"),
            WorldOutputFile {
                bytes: Arc::clone(&files[Path::new("item-0/value.hi")].bytes),
                sha256: [6; 32],
            },
        )]);
        assert_response_rejection(
            link_world_prior_input(&input, &producer, &consumer, &consumer, 900, &wrong),
            CompilerResponseRejection::ProvenanceMismatch,
        );
        let later = stage(3, 5);
        assert_response_rejection(
            link_world_prior_input(&input, &producer, &consumer, &later, 900, &files),
            CompilerResponseRejection::ProvenanceMismatch,
        );
        let mut selected_consumer = consumer.clone();
        selected_consumer.selected_inputs.push(input.clone());
        assert!(
            link_world_prior_input(&input, &producer, &selected_consumer, &later, 900, &files)
                .is_ok()
        );
        for changed in [0, 1, 2, 3] {
            let mut altered = selected_consumer.clone();
            let reference = &mut altered.selected_inputs[0];
            match changed {
                0 => {
                    let ParsedWorldInputSource::PriorStage { interface, .. } =
                        &mut reference.source
                    else {
                        unreachable!()
                    };
                    interface[0] ^= 1;
                }
                1 => reference.defining_scope += 1,
                2 => reference.session_var += 1,
                _ => reference.generation += 1,
            }
            assert_response_rejection(
                link_world_prior_input(&input, &producer, &altered, &later, 900, &files),
                CompilerResponseRejection::ProvenanceMismatch,
            );
        }
        assert_response_rejection(
            link_world_prior_input(&input, &producer, &consumer, &stage(0, 0), 900, &files),
            CompilerResponseRejection::ProvenanceMismatch,
        );
        let Value::Array(fields) = &packet else {
            unreachable!()
        };
        let mut old = fields.clone();
        old.remove(0);
        assert_response_rejection(
            WorldInputRef::decode(&world_array(old)),
            CompilerResponseRejection::Malformed,
        );
        for origin in [
            world_array(vec![integer(3)]),
            world_array(vec![integer(1), integer(0)]),
            world_array(vec![integer(0), integer(0)]),
            world_array(vec![]),
        ] {
            let mut changed = fields.clone();
            changed[0] = origin;
            assert_response_rejection(
                WorldInputRef::decode(&world_array(changed)),
                CompilerResponseRejection::Malformed,
            );
        }
        for bad_seal in [world_blob([5; 31]), world_blob([5; 33])] {
            let mut changed = fields.clone();
            changed[6] = bad_seal;
            assert_response_rejection(
                WorldInputRef::decode(&world_array(changed)),
                CompilerResponseRejection::Malformed,
            );
        }
        let mut as_base = fields.clone();
        as_base[0] = world_array(vec![integer(0)]);
        let error = assert_response_rejection(
            WorldInputRef::decode(&world_array(as_base)),
            CompilerResponseRejection::Malformed,
        );
        let failure = error
            .source
            .get_ref()
            .and_then(|cause| cause.downcast_ref::<tidepool_extract_cmd::CompilerResponseFailure>())
            .unwrap();
        assert!(failure
            .cause()
            .get_ref()
            .is_some_and(|cause| cause.is::<cid::Error>()));
        let a = (
            WorldCellRuntimeInputOrigin::PriorStage {
                producer: 0,
                consumer: 2,
            },
            "x",
        );
        let b = (
            WorldCellRuntimeInputOrigin::PriorStage {
                producer: 0,
                consumer: 4,
            },
            "x",
        );
        let c = (WorldCellRuntimeInputOrigin::BaseWorld, "x");
        let mut seen = std::collections::BTreeSet::from([a]);
        assert!(seen.insert(b));
        assert!(seen.insert(c));
        assert!(!seen.insert(a));
    }

    #[test]
    fn stage_selected_refs_bind_attribution_and_preserve_exact_normalized_identity() {
        use ciborium::value::Value;
        let integer = |value: u64| Value::Integer(value.into());
        let input = |origin, alias: &str, id, seal| {
            WorldInputRef::decode(&world_array(vec![
                origin,
                world_text(alias),
                world_array(vec![
                    world_text("main"),
                    world_text("Value"),
                    world_text("value"),
                    world_text(alias),
                    world_array(vec![integer(0)]),
                ]),
                integer(id),
                integer(900),
                integer(7),
                world_blob(
                    cid::Cid::new_v1(
                        0x71,
                        cid::multihash::Multihash::<64>::wrap(0x12, &[seal; 32]).unwrap(),
                    )
                    .to_bytes(),
                ),
            ]))
            .unwrap()
        };
        let base = input(world_array(vec![integer(0)]), "x", 71, 5);
        let operand = input(world_array(vec![integer(2)]), "input", 72, 6);
        let identities = [0, 1].map(|ordinal| WorldCellStageIdentity::Declaration {
            ordinal,
            owner: crate::declaration_join::ExactModuleIdentity {
                unit: "main".into(),
                module: format!("Tidepool.Session.Lib.G{}", ordinal + 7),
            },
        });
        // Pure normalized framing DATA. Native/runtime authority is established
        // only by the real preparation and checked-item consumers.
        let hash = |selections: [Vec<WorldInputRef>; 2]| {
            let mut digest = blake3::Hasher::new();
            let mut frame = |bytes: &[u8]| {
                digest.update(&(bytes.len() as u64).to_le_bytes());
                digest.update(bytes);
            };
            for (ordinal, mut refs) in selections.into_iter().enumerate() {
                refs.sort_unstable();
                frame_world_stage_selection(
                    &identities[ordinal],
                    &[ordinal],
                    refs.iter().map(WorldInputRef::view),
                    &mut frame,
                );
            }
            *digest.finalize().as_bytes()
        };
        let original = hash([vec![base.clone()], vec![operand.clone()]]);
        assert_ne!(
            original,
            hash([vec![operand.clone()], vec![base.clone()]]),
            "an identical whole-cell union cannot replace stage ownership"
        );
        assert_eq!(
            hash([vec![base.clone(), operand.clone()], vec![]]),
            hash([vec![operand.clone(), base.clone()], vec![]]),
            "normalization order does not change the exact selected facts"
        );
        for changed in [0, 1, 2, 3, 4] {
            let mut different = base.clone();
            match changed {
                0 => {
                    let ParsedWorldInputSource::BaseWorld(seal) = &mut different.source else {
                        unreachable!()
                    };
                    seal.encoded[0] ^= 1;
                }
                1 => different.defining_scope += 1,
                2 => different.session_var += 1,
                3 => different.generation += 1,
                _ => different.symbol.module.push('X'),
            }
            assert_ne!(original, hash([vec![different], vec![operand.clone()]]));
        }
        let repeated = hash([vec![base.clone()], vec![base.clone()]]);
        assert_ne!(
            repeated,
            hash([vec![base], vec![]]),
            "a legitimate repeated base reference remains attributed to both stages"
        );
    }

    #[test]
    fn full_language_stage_table_refuses_crossed_ownership_and_incomplete_coverage() {
        fn world_integer(value: u64) -> ciborium::value::Value {
            ciborium::value::Value::Integer(value.into())
        }
        use crate::checked_cell::CheckedPlannedCellSlot as Slot;
        use ciborium::value::Value;
        use tidepool_repr::execution_schema::{InventoryDecodeLimits, InventoryOperation};
        let slots = [
            Slot::Prologue { declaration: 7 },
            Slot::Declaration { declaration: 8 },
        ];
        let mut manifest = BTreeMap::new();
        let stage =
            |ordinal: u64, kind: &str, module: &str, manifest: &mut BTreeMap<PathBuf, [u8; 32]>| {
                let refs = [
                    "world-source.cbor",
                    "module-products.cbor",
                    "certified-products.cbor",
                ]
                .into_iter()
                .map(|name| {
                    let path = format!("stage-{ordinal}/{name}");
                    manifest.insert(PathBuf::from(&path), [ordinal as u8 + 1; 32]);
                    world_array(vec![
                        world_text(&path),
                        Value::Bytes(vec![ordinal as u8 + 1; 32]),
                    ])
                })
                .collect();
                world_array(vec![
                    world_array(vec![
                        world_text(kind),
                        world_integer(ordinal),
                        world_text("main"),
                        world_text(module),
                    ]),
                    world_array(vec![world_integer(ordinal)]),
                    world_array(refs),
                    world_array(vec![]),
                    world_array(vec![]),
                ])
            };
        let rows = vec![
            stage(0, "prologue", "Tidepool.Session.Lib.G7", &mut manifest),
            stage(1, "declaration", "Tidepool.Session.Lib.G8", &mut manifest),
        ];
        let decode = |rows: Vec<Value>, manifest: &BTreeMap<PathBuf, [u8; 32]>| {
            decode_world_stages(
                &world_array(rows),
                &slots,
                &[],
                manifest,
                &InventoryOperation::new(Default::default()),
            )
        };
        let row_seal = |byte| {
            cid::Cid::new_v1(
                0x71,
                cid::multihash::Multihash::<64>::wrap(0x12, &[byte; 32]).unwrap(),
            )
            .to_bytes()
        };
        let base = world_array(vec![
            world_array(vec![world_integer(0)]),
            world_text("x"),
            world_array(vec![
                world_text("main"),
                world_text("Value"),
                world_text("value"),
                world_text("x"),
                world_array(vec![world_integer(0)]),
            ]),
            world_integer(71),
            world_integer(900),
            world_integer(7),
            world_blob(row_seal(5)),
        ]);
        let mut repeated = rows.clone();
        for row in &mut repeated {
            let Value::Array(fields) = row else {
                unreachable!()
            };
            fields[3] = world_array(vec![base.clone()]);
        }
        let repeated = decode(repeated, &manifest).unwrap();
        assert_eq!(
            repeated[0].selected_inputs, repeated[1].selected_inputs,
            "a base reference may be selected independently by distinct stages"
        );
        let mut separate = rows.clone();
        let Value::Array(fields) = &mut separate[0] else {
            unreachable!()
        };
        fields[3] = world_array(vec![base.clone()]);
        let separate = decode(separate, &manifest).unwrap();
        assert_eq!(separate[0].selected_inputs.len(), 1);
        assert!(
            separate[1].selected_inputs.is_empty(),
            "the whole-cell union is not a later stage selection"
        );
        for (index, mut duplicate) in [base.clone(), base.clone()].into_iter().enumerate() {
            if index == 1 {
                let Value::Array(fields) = &mut duplicate else {
                    unreachable!()
                };
                fields[6] = world_blob(row_seal(9));
            }
            let mut changed = rows.clone();
            let Value::Array(fields) = &mut changed[0] else {
                unreachable!()
            };
            fields[3] = world_array(vec![base.clone(), duplicate]);
            assert_response_rejection(
                decode(changed, &manifest),
                CompilerResponseRejection::ProvenanceMismatch,
            );
        }
        let checked = decode(rows.clone(), &manifest).unwrap();
        assert_eq!(checked[0].ordinals(), &[0]);
        assert_eq!(checked[0].index(), 0);
        assert_eq!(checked[1].index(), 1);
        assert!(
            matches!(checked[1].identity(), WorldCellStageIdentity::Declaration { ordinal: 1, owner } if owner.module == "Tidepool.Session.Lib.G8")
        );
        assert!(checked[0].contains_output(Path::new("stage-0/dependencies.json")));
        assert!(!checked[0].contains_output(Path::new("stage-1/dependencies.json")));
        assert!(!checked[0].contains_output(Path::new("stage-01/dependencies.json")));
        let mut old_rows = rows.clone();
        let Value::Array(old_fields) = &mut old_rows[0] else {
            unreachable!()
        };
        old_fields.pop();
        assert_response_rejection(
            decode(old_rows, &manifest),
            CompilerResponseRejection::Malformed,
        );
        assert_response_rejection(
            decode(vec![], &manifest),
            CompilerResponseRejection::ProvenanceMismatch,
        );
        assert_response_rejection(
            decode(rows[..1].to_vec(), &manifest),
            CompilerResponseRejection::ProvenanceMismatch,
        );
        assert_response_rejection(
            decode(vec![rows[1].clone(), rows[0].clone()], &manifest),
            CompilerResponseRejection::ProvenanceMismatch,
        );
        assert_response_rejection(
            decode(vec![rows[0].clone(), rows[0].clone()], &manifest),
            CompilerResponseRejection::ProvenanceMismatch,
        );
        let mut corrupt = manifest.clone();
        corrupt.insert(PathBuf::from("stage-0/module-products.cbor"), [99; 32]);
        assert_response_rejection(
            decode(rows.clone(), &corrupt),
            CompilerResponseRejection::ProvenanceMismatch,
        );
        corrupt.remove(Path::new("stage-0/module-products.cbor"));
        assert_response_rejection(
            decode(rows.clone(), &corrupt),
            CompilerResponseRejection::ProvenanceMismatch,
        );
        // Every field remains independently checked against the actual slot,
        // its original kind and the exact namespace, rather than just a seal.
        for (field, replacement) in [
            (
                0,
                world_array(vec![
                    world_text("declaration"),
                    world_integer(0),
                    world_text("main"),
                    world_text("Tidepool.Session.Lib.G7"),
                ]),
            ),
            (
                0,
                world_array(vec![
                    world_text("prologue"),
                    world_integer(0),
                    world_text("other-unit"),
                    world_text("Tidepool.Session.Lib.G7"),
                ]),
            ),
            (
                0,
                world_array(vec![
                    world_text("prologue"),
                    world_integer(0),
                    world_text("main"),
                    world_text("Tidepool.Session.Lib.G8"),
                ]),
            ),
            (1, world_array(vec![world_integer(0), world_integer(1)])),
            (
                2,
                match &rows[1] {
                    Value::Array(fields) => fields[2].clone(),
                    _ => unreachable!(),
                },
            ),
        ] {
            let mut changed = rows.clone();
            let Value::Array(fields) = &mut changed[0] else {
                unreachable!()
            };
            fields[field] = replacement;
            assert_response_rejection(
                decode(changed, &manifest),
                CompilerResponseRejection::ProvenanceMismatch,
            );
        }
        let exhausted = InventoryOperation::new(InventoryDecodeLimits {
            max_work: 0,
            ..Default::default()
        });
        assert!(matches!(
            decode_world_stages(&world_array(rows), &slots, &[], &manifest, &exhausted),
            Err(CompileError::CompilerEvidence(error)) if matches!(*error,
                crate::certified_products::CertificationError::Product(
                    tidepool_repr::execution_schema::ParseError::LimitExceeded("work")
                )
            )
        ));
        assert_response_rejection(
            WorldCellStageIdentity::decode(&world_text("exec")),
            CompilerResponseRejection::Malformed,
        );
        assert_response_rejection(
            WorldCellStageIdentity::decode(&world_array(vec![
                world_text("exec"),
                world_text("bad digest"),
            ])),
            CompilerResponseRejection::Malformed,
        );
        assert_response_rejection(
            WorldCellStageIdentity::decode(&world_array(vec![
                world_text("future-stage"),
                world_integer(0),
                world_text("main"),
                world_text("Tidepool.Session.Lib.G7"),
            ])),
            CompilerResponseRejection::Malformed,
        );
    }

    #[test]
    fn captured_source_aliases_survive_transport_directory_retirement() {
        let first = b"module Module where\nvalue = (7 :: Int)\n";
        let second = b"module Module where\nvalue = (8 :: Int)\n";
        let captured = sources(first, second);
        let operation =
            tidepool_repr::execution_schema::InventoryOperation::new(Default::default());
        let materialized = tempfile::tempdir().unwrap();
        let aliases = vec![
            materialized.path().join("root-0"),
            materialized.path().join("root-1"),
        ];
        for (root, body) in aliases.iter().zip([first.as_slice(), second.as_slice()]) {
            std::fs::create_dir_all(root).unwrap();
            std::fs::write(root.join("Module.hs"), body).unwrap();
        }
        drop(materialized);
        assert!(aliases.iter().all(|path| !path.exists()));
        let domain = WorldSourcePathDomain::new(&captured, &aliases, &operation).unwrap();
        assert_eq!(
            domain
                .selected_source("Module", &aliases[0].join("Module.hs"))
                .unwrap(),
            (world_digest(first), first.len())
        );
        // A lower root cannot win even when its own bytes and digest are valid.
        assert!(domain
            .selected_source("Module", &aliases[1].join("Module.hs"))
            .is_err());
        assert!(domain
            .require_absent(&aliases[0].join("Module.hs"))
            .is_err());
        assert!(domain
            .require_absent(&aliases[0].join("Missing.hs"))
            .is_ok());
        assert!(domain
            .require_absent(&aliases[0].join("../escape.hs"))
            .is_err());
        assert!(WorldSourcePathDomain::new(&captured, &aliases[..1], &operation).is_err());
        assert!(WorldSourcePathDomain::new(
            &captured,
            &[aliases[0].clone(), aliases[0].join("nested")],
            &operation,
        )
        .is_err());
        // Relocation changes receiving labels, never the captured selection.
        let moved = vec![
            PathBuf::from("/retired-copy/a"),
            PathBuf::from("/retired-copy/b"),
        ];
        let moved_domain = WorldSourcePathDomain::new(&captured, &moved, &operation).unwrap();
        assert_eq!(
            moved_domain
                .selected_source("Module", &moved[0].join("Module.hs"))
                .unwrap(),
            (world_digest(first), first.len())
        );
        assert!(moved_domain
            .selected_source("Module", &aliases[0].join("Module.hs"))
            .is_err());
    }

    #[test]
    fn world_receipt_refuses_later_source_even_with_its_correct_hash() {
        let a = b"module Module where\nvalue = (1 :: Int)\n";
        let b = b"module Module where\nvalue = True\n";
        let ab = sources(a, b);
        assert!(authenticate_module_receipt(
            &home(0, a),
            &ab,
            &[7; 32],
            ModuleReceiptUse::Requested
        )
        .is_ok());
        assert!(authenticate_module_receipt(
            &home(1, b),
            &ab,
            &[7; 32],
            ModuleReceiptUse::Requested
        )
        .is_err());
        let ba = sources(b, a);
        assert!(authenticate_module_receipt(
            &home(0, b),
            &ba,
            &[7; 32],
            ModuleReceiptUse::Requested
        )
        .is_ok());
        assert!(authenticate_module_receipt(
            &home(1, a),
            &ba,
            &[7; 32],
            ModuleReceiptUse::Requested
        )
        .is_err());
        assert_ne!(ab.seal(), ba.seal());
        let package = WorldModuleReceipt {
            unit: "base".into(),
            module: "Module".into(),
            origin: WorldModuleOrigin::Package { profile: [7; 32] },
        };
        assert!(
            authenticate_module_receipt(&package, &ab, &[7; 32], ModuleReceiptUse::Requested)
                .is_err()
        );
        // A package-qualified reexport may define a name in a different unit
        // despite a same-named home module in the captured search namespace.
        assert!(
            authenticate_module_receipt(&package, &ab, &[7; 32], ModuleReceiptUse::Defining)
                .is_ok()
        );
        assert!(
            authenticate_module_receipt(&package, &ab, &[8; 32], ModuleReceiptUse::Defining)
                .is_err()
        );
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
            VisibleWorldBindings::from_published_bindings(0, std::iter::empty()).unwrap(),
            Arc::clone(&sources),
            CompilerWorldLexicalSurface::from_import_lines(Vec::new(), None).unwrap(),
        )
        .unwrap();
        assert_eq!(cold.primary_context.snapshot().producer_sha256(), [0; 32]);
        let prepared = cold.clone().prepare_for(canonical, [0; 32]).unwrap();
        let bound = prepared.primary_inputs.context().clone();
        let root = prepared.primary_root.clone();
        let blocks = &prepared.runtime_context_blocks;
        assert_eq!(bound.snapshot().producer_sha256(), canonical);
        // The original published owner remains unchanged. Inspect the actual
        // production materializer's root through its independent reader.
        assert_eq!(cold.primary_context.snapshot().producer_sha256(), [0; 32]);
        let store = fvm_ipld_blockstore::MemoryBlockstore::new();
        for (cid, body) in blocks.blocks() {
            store.put_keyed(cid, body).unwrap();
        }
        let restored = crate::exact_context::hydrate_context(&store, root.root()).unwrap();
        assert_eq!(restored.snapshot().producer_sha256(), canonical);
        let issued = crate::artifacts::ModuleCandidateOffer::compiler_world_inputs(
            Some(bound),
            VisibleWorldBindings::from_published_bindings(0, std::iter::empty()).unwrap(),
            Arc::clone(&sources),
            CompilerWorldLexicalSurface::from_import_lines(Vec::new(), None).unwrap(),
        )
        .unwrap();
        assert_eq!(
            issued
                .prepare_for(canonical, [0; 32])
                .unwrap()
                .primary_root
                .root(),
            root.root()
        );
        let wrong = crate::artifacts::ModuleCandidateOffer::compiler_world_inputs(
            Some(Arc::new(
                (*cold.primary_context).clone().admit_producer(raw).unwrap(),
            )),
            VisibleWorldBindings::from_published_bindings(0, std::iter::empty()).unwrap(),
            sources,
            CompilerWorldLexicalSurface::from_import_lines(Vec::new(), None).unwrap(),
        )
        .unwrap();
        assert!(wrong.prepare_for(canonical, [0; 32]).is_err());
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
    fn compiler_identity_excludes_only_passive_runtime_commitment() {
        use cid::multihash::Multihash;
        let cid = |byte| cid::Cid::new_v1(0x71, Multihash::<64>::wrap(0x12, &[byte; 32]).unwrap());
        let roots = SemanticRoots {
            primary: cid(1),
            associations: cid(2),
            bindings: cid(3),
            lexical: cid(4),
        };
        let compiler = |roots: &SemanticRoots, source, producer, profile, scope| {
            compiler_input_identity(roots, &[source; 32], &[producer; 32], &[profile; 32], scope)
                .unwrap()
        };
        let runtime = |roots: &SemanticRoots| {
            world_digest(
                &world_restoration_product(roots, &[5; 32], &[6; 32], &[7; 32], 8).unwrap(),
            )
        };
        let initial = compiler(&roots, 5, 6, 7, 8);
        let passive = SemanticRoots {
            primary: cid(9),
            ..roots.clone()
        };
        assert_ne!(runtime(&roots), runtime(&passive));
        assert_eq!(initial, compiler(&passive, 5, 6, 7, 8));
        for changed in [
            SemanticRoots {
                associations: cid(9),
                ..roots.clone()
            },
            SemanticRoots {
                bindings: cid(9),
                ..roots.clone()
            },
            SemanticRoots {
                lexical: cid(9),
                ..roots.clone()
            },
        ] {
            assert_ne!(initial, compiler(&changed, 5, 6, 7, 8));
        }
        for args in [(9, 6, 7, 8), (5, 9, 7, 8), (5, 6, 9, 8), (5, 6, 7, 9)] {
            assert_ne!(initial, compiler(&roots, args.0, args.1, args.2, args.3));
        }
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
        assert_eq!(fields.len(), 6);
        assert!(fields[2].as_array().unwrap().is_empty());
        let blocks = fields[4].as_array().unwrap();
        assert!(
            !blocks
                .iter()
                .any(|block| &block.as_array().unwrap()[0] == &fields[0]),
            "opaque runtime Context is not exported"
        );
        for root in [&fields[1], &fields[5]] {
            assert!(blocks
                .iter()
                .any(|block| &block.as_array().unwrap()[0] == root));
        }
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

    #[test]
    fn world_output_capture_retains_authenticated_bytes_and_refuses_changed_products() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("product.cbor");
        let original = b"actual compiler product";
        std::fs::write(&path, original).unwrap();
        let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
        let mut files = BTreeMap::new();
        capture_world_output(
            &directory,
            "product.cbor".into(),
            world_digest(original),
            &mut files,
            &mut validation,
        )
        .unwrap();
        std::fs::write(&path, b"changed after capture").unwrap();
        assert_eq!(&**files[Path::new("product.cbor")].bytes(), original);
        assert_eq!(
            *files[Path::new("product.cbor")].sha256(),
            world_digest(original)
        );
        assert!(capture_world_output(
            &directory,
            "product.cbor".into(),
            world_digest(original),
            &mut files,
            &mut validation
        )
        .is_err());
        assert!(capture_world_output(
            &directory,
            "product.cbor".into(),
            world_digest(original),
            &mut BTreeMap::new(),
            &mut validation
        )
        .is_err());
        assert!(capture_world_output(
            &directory,
            "../product.cbor".into(),
            world_digest(original),
            &mut BTreeMap::new(),
            &mut validation
        )
        .is_err());
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    use crate::exact_context::{FrontierWork, Materializer, ReachableBlocks};
    use fvm_ipld_blockstore::MemoryBlockstore;

    #[test]
    fn world_answer_rejection_retains_the_structural_decode_cause() {
        use tidepool_extract_report::world_query::WorldAnswerDecodeError as E;

        for cause in [
            E::Malformed("unknown answer tag".into()),
            E::ProvenanceMismatch("question fingerprint"),
        ] {
            let message = cause.to_string();
            let CompileError::CompilerEndpoint(rejection) = world_answer_decode_error(cause) else {
                panic!("accepted response rejection must retain endpoint settlement");
            };
            let failure = rejection
                .source
                .get_ref()
                .and_then(|error| {
                    error.downcast_ref::<tidepool_extract_cmd::CompilerResponseFailure>()
                })
                .expect("the response boundary reason");
            let retained = failure
                .cause()
                .get_ref()
                .and_then(|error| error.downcast_ref::<E>())
                .expect("the original typed decoder cause");
            assert_eq!(
                failure.reason(),
                match retained {
                    E::Malformed(_) => CompilerResponseRejection::Malformed,
                    E::ProvenanceMismatch(_) => CompilerResponseRejection::ProvenanceMismatch,
                }
            );
            assert_eq!(retained.to_string(), message);
            assert!(rejection.to_string().contains(&message));
        }
    }

    #[test]
    fn source_selection_names_the_captured_namespace_and_refuses_missing_identity() {
        let original = WorldSourceSelection::new([7; 32], "captured-source").unwrap();
        let target = WorldSourceSelection::new([7; 32], "captured-source").unwrap();
        assert_eq!(original, target);
        assert!(WorldSourceSelection::new([0; 32], "captured-source").is_err());
        assert!(WorldSourceSelection::new([7; 32], "").is_err());
        assert!(WorldSourceSelection::new([7; 32], "x".repeat(65537)).is_err());
    }

    #[test]
    #[ignore = "requires the matched Haskell worker and frontend"]
    fn recovered_authored_facade_keeps_its_issuing_primary_product() {
        use crate::declaration_join::{
            ExactLexicalNode, ExactModuleIdentity, RecoveryDeclarationOrigin,
            RecoveryDeclarationSelection,
        };
        use tidepool_repr::{Generation, SessionModule};
        let module = SessionModule::lib(Generation(61));
        let mut branches = Vec::new();
        for body in ["-- source revision A", "-- source revision B"] {
            let root = tempfile::tempdir().unwrap();
            let source = format!(
                "module {} (answer) where\nanswer :: Int\nanswer = 41\n{body}\n",
                module.module_name()
            );
            let path = root.path().join(module.relative_hs_path());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &source).unwrap();
            let certificate = Arc::new(
                crate::artifacts::test_support::certify_authored_declaration(
                    module,
                    &path,
                    &source,
                    &[root.path().to_path_buf()],
                    root.path(),
                )
                .unwrap(),
            );
            let owner = ExactModuleIdentity {
                unit: certificate.product().owner().unit.clone(),
                module: module.module_name(),
            };
            let context = Arc::new(
                ExactDeclarationContext::new(
                    &[certificate.clone()],
                    &[],
                    vec![ExactLexicalNode {
                        owner: owner.clone(),
                        imports: Vec::new(),
                    }],
                )
                .unwrap(),
            );
            let artifact = context
                .compiler_input_projection()
                .snapshot_roles()
                .get(&owner)
                .unwrap()
                .interface();
            branches.push((root, certificate, owner, context, artifact));
        }
        let (_, first, owner, primary, artifact) = &branches[0];
        let (_, second, _, foreign, foreign_artifact) = &branches[1];
        assert_eq!(
            first.lexical_exports(),
            second.lexical_exports(),
            "both actual products publish the same exported names and types"
        );
        assert_ne!(first.source_sha256(), second.source_sha256());
        assert_ne!(
            artifact, foreign_artifact,
            "different original source certificates exercise distinct canonical products"
        );
        let recovered = crate::artifacts::test_support::with_settlement(|settlement| {
            crate::declaration_join::certify_recovered_declaration_tip_in_context(
                primary.clone(),
                RecoveryDeclarationSelection {
                    origin: RecoveryDeclarationOrigin::Authored {
                        generation: module.gen().0,
                        introduced_exports: first.introduced_exports().to_vec(),
                    },
                    root: owner.clone(),
                    lexical: primary.lexical_graph().to_vec(),
                    exports: first.lexical_exports().to_vec(),
                    instances: first.instances().clone(),
                    family_closure: first.family_closure().to_vec(),
                },
                &[],
                settlement,
            )
        })
        .unwrap();
        let lexical = crate::compiler_world::CompilerWorldLexicalSurface::from_import_lines(
            Vec::new(),
            Some(crate::compiler_world::WorldDeclarationSelection::Recovered(
                Arc::new(recovered),
            )),
        )
        .unwrap();
        let store = MemoryBlockstore::new();
        let mut materializer = Materializer::new(&store);
        lexical.materialize(primary, &mut materializer).unwrap();
        assert!(
            matches!(
                lexical.materialize(foreign, &mut materializer),
                Err(CompileError::ExtractFailed(_))
            ),
            "a distinct actual product must fail the writer's source validation"
        );
    }

    #[test]
    fn lexical_surface_preserves_order_qualification_and_explicit_empty_imports() {
        let primary = ExactDeclarationContext::new(&[], &[], Vec::new()).unwrap();
        let root = |imports: &[&str]| {
            let store = MemoryBlockstore::new();
            let mut materializer = Materializer::new(&store);
            CompilerWorldLexicalSurface::from_import_lines(
                imports.iter().map(|spec| (*spec).to_owned()).collect(),
                None,
            )
            .unwrap()
            .materialize(&primary, &mut materializer)
            .unwrap()
        };
        assert_eq!(
            root(&["import Data.List ()"]),
            root(&["import Data.List ()"])
        );
        assert_ne!(root(&[]), root(&["import Data.List ()"]));
        assert_ne!(
            root(&["import Data.List"]),
            root(&["import qualified Data.List as L"])
        );
        assert_ne!(
            root(&["import Data.List hiding (map)"]),
            root(&["import Data.List"])
        );
        assert_ne!(
            root(&["import Data.List", "import Data.Maybe"]),
            root(&["import Data.Maybe", "import Data.List"])
        );
        for raw_spec in [
            "Data.List",
            "qualified Data.List as L",
            "imported Data.List",
        ] {
            assert!(CompilerWorldLexicalSurface::from_import_lines(
                vec![raw_spec.to_owned()],
                None,
            )
            .is_err());
        }
    }

    #[test]
    fn observation_lexical_fact_binds_profile_fragment_and_ordered_imports() {
        let primary = ExactDeclarationContext::new(&[], &[], Vec::new()).unwrap();
        let root = |observation: Option<Arc<WorldObservationScaffold>>| {
            let store = MemoryBlockstore::new();
            let mut materializer = Materializer::new(&store);
            CompilerWorldLexicalSurface::from_import_lines(Vec::new(), None)
                .unwrap()
                .with_observation(observation)
                .unwrap()
                .materialize(&primary, &mut materializer)
                .unwrap()
        };
        let fact = |profile, fragment: &str, imports: &[&str]| {
            Arc::new(
                WorldObservationScaffold::new(
                    profile,
                    Arc::from(fragment),
                    imports.iter().map(|spec| (*spec).to_owned()).collect(),
                    None,
                    None,
                    Vec::new(),
                )
                .unwrap(),
            )
        };
        let workbench = || {
            fact(
                WorldObservationProfile::Workbench,
                "type ActorEffects = '[]",
                &["Data.List", "Data.Maybe"],
            )
        };
        let original = root(Some(workbench()));
        assert_eq!(original, root(Some(workbench())));
        assert_ne!(original, root(None));
        assert_ne!(
            original,
            root(Some(fact(
                WorldObservationProfile::HostCarrier,
                "type ActorEffects = '[]",
                &["Data.List", "Data.Maybe"]
            )))
        );
        assert_ne!(
            original,
            root(Some(fact(
                WorldObservationProfile::Workbench,
                "type ActorEffects = '[Introspection]",
                &["Data.List", "Data.Maybe"]
            )))
        );
        assert_ne!(
            original,
            root(Some(fact(
                WorldObservationProfile::Workbench,
                "type ActorEffects = '[]",
                &["Data.Maybe", "Data.List"]
            )))
        );
    }

    #[test]
    fn observation_lexical_fact_rejects_malformed_protected_source_fields() {
        for (fragment, imports) in [
            ("", vec![]),
            ("a\0b", vec![]),
            (
                "type R = '[]",
                vec!["Data.List\nimport Data.Maybe".to_owned()],
            ),
        ] {
            assert!(WorldObservationScaffold::new(
                WorldObservationProfile::Workbench,
                Arc::from(fragment),
                imports,
                None,
                None,
                Vec::new(),
            )
            .is_err());
        }
    }

    fn captured_sources(body: &[u8], origin: &[u8]) -> CapturedCompilerSources {
        let root = CapturedCompilerSourceRoot {
            manifest: SourceRootManifest::from_file_digests([(
                PathBuf::from("Module.hs"),
                blake3::hash(body).to_hex().to_string(),
            )])
            .unwrap(),
            bodies: BTreeMap::from([(PathBuf::from("Module.hs"), Arc::<[u8]>::from(body))]),
        };
        CapturedCompilerSources::from_captured_roots(vec![root], Arc::from(origin), Arc::new(()))
            .unwrap()
    }

    fn identity(
        history: &ReachableBlocks,
        sources: &CapturedCompilerSources,
        producer: &[u8; 32],
        profile: &[u8; 32],
        query_scope: u64,
    ) -> [u8; 32] {
        let roots = history.roots();
        assert_eq!(roots.len(), 4);
        world_digest(
            &world_restoration_product(
                &SemanticRoots {
                    primary: roots[0],
                    associations: roots[1],
                    bindings: roots[2],
                    lexical: roots[3],
                },
                sources.seal(),
                producer,
                profile,
                query_scope,
            )
            .unwrap(),
        )
    }

    #[test]
    fn semantic_world_identity_ignores_restoration_work_counters() {
        let store = MemoryBlockstore::new();
        let mut materializer = Materializer::new(&store);
        let leaf = materializer.materialize_value(&("leaf", 0u64)).unwrap();
        let context = materializer
            .materialize_value(&("context", vec![leaf]))
            .unwrap();
        let associations = materializer
            .materialize_value(&("associations", vec![leaf, leaf]))
            .unwrap();
        let bindings = materializer
            .materialize_value(&("bindings", vec![leaf]))
            .unwrap();
        let lexical = materializer
            .materialize_value(&("lexical", vec![leaf]))
            .unwrap();
        let extra = materializer.materialize_value(&("extra", 1u64)).unwrap();
        let transient = materializer
            .materialize_value(&(
                "transient",
                vec![context, associations, bindings, lexical, extra],
            ))
            .unwrap();
        let staged = materializer.finish().unwrap();
        let roots = vec![context, associations, bindings, lexical];

        // Same final semantic roots, three materially different restoration
        // histories.
        let cold = ReachableBlocks::default()
            .replace_roots(&staged, roots.clone())
            .unwrap();
        // Identical roots on a no-op replacement reset the work counters.
        let reused = cold.replace_roots(&staged, roots.clone()).unwrap();
        // Linking a superset closure first means the final replacement verifies
        // no bytes and unlinks the transient root.
        let reordered = ReachableBlocks::default()
            .replace_roots(&staged, vec![transient])
            .unwrap()
            .replace_roots(&staged, roots.clone())
            .unwrap();

        assert_eq!(cold.roots(), roots.as_slice());
        assert_eq!(reused.roots(), roots.as_slice());
        assert_eq!(reordered.roots(), roots.as_slice());

        let cold_work = cold.work();
        let reused_work = reused.work();
        let reordered_work = reordered.work();
        assert_eq!(reused_work, FrontierWork::default());
        assert_eq!((cold_work.link_visits, cold_work.unlink_visits), (9, 0));
        assert_eq!(
            (reordered_work.link_visits, reordered_work.unlink_visits),
            (4, 6)
        );
        assert!(cold_work.verified_bytes > 0 && cold_work.decoded_bytes > 0);
        assert_eq!(
            (reordered_work.verified_bytes, reordered_work.decoded_bytes),
            (0, 0)
        );
        assert_ne!(cold_work, reused_work);
        assert_ne!(cold_work, reordered_work);

        let source = captured_sources(b"module Module where\nx = 1\n", b"origin-a");
        let producer = [7; 32];
        let profile = [8; 32];
        let query_scope = 11;
        let cold_identity = identity(&cold, &source, &producer, &profile, query_scope);
        assert_eq!(
            identity(&reused, &source, &producer, &profile, query_scope),
            cold_identity
        );
        assert_eq!(
            identity(&reordered, &source, &producer, &profile, query_scope),
            cold_identity
        );

        let changed_root = cold
            .replace_roots(&staged, vec![context, associations, extra, lexical])
            .unwrap();
        assert_ne!(
            identity(&changed_root, &source, &producer, &profile, query_scope),
            cold_identity
        );
        let changed_lexical = cold
            .replace_roots(&staged, vec![context, associations, bindings, extra])
            .unwrap();
        assert_ne!(
            identity(&changed_lexical, &source, &producer, &profile, query_scope),
            cold_identity
        );
        let changed_source = captured_sources(b"module Module where\nx = 2\n", b"origin-a");
        assert_ne!(
            identity(&cold, &changed_source, &producer, &profile, query_scope),
            cold_identity
        );
        assert_ne!(
            identity(&cold, &source, &producer, &[9; 32], query_scope),
            cold_identity
        );
        assert_ne!(
            identity(&cold, &source, &[10; 32], &profile, query_scope),
            cold_identity
        );
        assert_ne!(
            identity(&cold, &source, &producer, &profile, query_scope + 1),
            cold_identity
        );
    }
}
