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
use crate::exact_context::{
    ContextRoot, MaterializedMap, Materializer, ReachableBlocks, SemanticRoots,
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

#[derive(Clone)]
struct WorldBindingFact {
    symbol: tidepool_repr::execution_schema::SymbolIdentity,
    session_var: tidepool_repr::SessionVarId,
    defining_scope: u64,
    row: Arc<CheckedValueArtifact>,
}

/// The current occurrence table, coupled to genuine checked rows by its
/// publication owner. Aliases keep independent names and share their row.
#[derive(Clone)]
pub struct CompilerWorldBindings {
    query_scope: u64,
    entries: imbl::OrdMap<String, WorldBindingFact>,
    metadata_bytes: u64,
    row_users: imbl::OrdMap<String, usize>,
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
        let mut row_users = imbl::OrdMap::new();
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
            let owner = input.row.owner().module_name();
            let count = row_users.get(&owner).copied().unwrap_or(0);
            row_users.insert(owner, count + 1);
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
            row_users,
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

/// Source syntax selected by the runtime's captured publication. Runtime grants
/// remain in the admission that owns this description.
#[derive(Clone, Debug)]
pub struct CompilerWorldLexicalSurface {
    import_specs: Arc<[String]>,
    declaration: Option<WorldDeclarationSelection>,
    metadata_bytes: u64,
}

impl CompilerWorldLexicalSurface {
    pub fn from_publication(
        import_specs: Vec<String>,
        declaration: Option<WorldDeclarationSelection>,
    ) -> Result<Self, CompileError> {
        let mut bytes = import_specs.iter().try_fold(128u64, |bytes, spec| {
            if spec.is_empty() || spec.contains('\0') || spec.contains('\n') {
                return Err(source_failure(
                    "invalid published compiler import specification",
                ));
            }
            bytes
                .checked_add(spec.len() as u64 + 32)
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
            import_specs: import_specs.into(),
            declaration,
            metadata_bytes: bytes,
        })
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
                    WorldDeclarationSelection::Recovered(tip) => (
                        tip.root().clone(),
                        None,
                        tip.interface_sha256().to_owned(),
                        tip.exports(),
                    ),
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
            world_text("TPCWLEX1"),
            world_array(self.import_specs.iter().map(world_text).collect()),
            declaration,
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
    visible_rows: imbl::OrdMap<String, Arc<CheckedValueArtifact>>,
    bindings: CompilerWorldBindings,
    sources: Arc<CapturedCompilerSources>,
    lexical: CompilerWorldLexicalSurface,
    directory: Arc<tempfile::TempDir>,
}

impl CompilerWorldOpenInputs {
    pub(crate) fn from_primary_selection(
        primary_context: Arc<ExactDeclarationContext>,
        visible_rows: Vec<Arc<CheckedValueArtifact>>,
        bindings: CompilerWorldBindings,
        sources: Arc<CapturedCompilerSources>,
        lexical: CompilerWorldLexicalSurface,
        directory: Arc<tempfile::TempDir>,
    ) -> Result<Self, CompileError> {
        Ok(Self {
            primary_context,
            visible_rows: visible_rows.into_iter().map(|row| (row.owner().module_name(), row)).collect(),
            bindings,
            sources,
            lexical,
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
    primary_compiler_inputs: Arc<crate::declaration_context::RequestCompilerInputs>,
    bindings: MaterializedMap<String, WorldBindingSnapshotFact>,
    binding_rows: imbl::OrdMap<String, cid::Cid>,
    lexical_root: cid::Cid,
    producer: [u8; 32],
    profile: [u8; 32],
    descriptor: PathBuf,
    input_files: imbl::Vector<Arc<std::fs::File>>,
    successor: Option<WorldSuccessorAcquisition>,
}

struct WorldSuccessorAcquisition {
    predecessor: [u8; 32],
    realization: Arc<Mutex<Option<String>>>,
    files: Vec<Arc<std::fs::File>>,
    cold: Mutex<Option<Arc<CompilerWorldRestoration>>>,
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

#[derive(Clone, Debug)]
pub struct CompilerWorld {
    restoration: Arc<CompilerWorldRestoration>,
    realization: Arc<Mutex<Option<String>>>,
    position: Option<WorldCellPosition>,
}

#[derive(Clone, Debug)]
struct WorldCellPosition {
    original_base: [u8; 32],
    certificate: [u8; 32],
    next_item: usize,
    total_items: usize,
}

impl CompilerWorld {
    pub fn logical_id(&self) -> &[u8; 32] {
        &self.restoration.logical_id
    }
    pub fn restoration(&self) -> &Arc<CompilerWorldRestoration> {
        &self.restoration
    }
}

impl crate::checked_cell::CellProgram {
    /// Issue the compiler facts of one successful runtime prefix. The caller
    /// supplies descriptions of the actual installed delta; this owner checks
    /// them against the same captured item and retains its type/native custody.
    /// Runtime source admission and atomic installation remain runtime-owned.
    pub fn item_successor<'a>(
        self: &Arc<Self>,
        predecessor: &Arc<CompilerWorld>,
        item: &crate::checked_cell::ExactCheckedItem,
        bindings: impl IntoIterator<Item = CompilerWorldBinding<'a>>,
    ) -> Result<Arc<CompilerWorld>, CompileError> {
        let crate::checked_cell::CellProgramBasis::World(program) = self.basis() else {
            return Err(source_failure("finite cell has no compiler-world successor"));
        };
        let ordinal = item.index();
        let issued = self.items().get(ordinal)
            .filter(|issued| issued.checked_item() == item)
            .ok_or_else(|| source_failure("successor item belongs to another checked output"))?;
        let observed = program.item(ordinal)
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
        if !ordered || predecessor.restoration.producer != *program.producer_sha256()
            || predecessor.restoration.profile != program.logical.profile_sha256
        {
            return Err(source_failure("successor predecessor is not this item's checked prefix"));
        }
        let native = issued.native().ok_or_else(|| {
            source_failure("declaration successor requires its checked lexical product")
        })?;
        let row = native.value_interface_certificate();
        let additions = CompilerWorldBindings::from_published_bindings(
            predecessor.restoration.inputs.bindings.query_scope, bindings,
        )?;
        if additions.entries.len() != observed.binders().len() {
            return Err(source_failure("runtime delta does not cover this item's captured binders"));
        }
        for binder in observed.binders() {
            let fact = additions.entries.get(binder.name()).ok_or_else(|| {
                source_failure("runtime delta omits a captured binder")
            })?;
            if fact.session_var.raw() != binder.varid()
                || fact.symbol.module != binder.module()
                || fact.symbol.occurrence != binder.name()
                || fact.symbol.namespace != "value"
                || fact.symbol.record_parent.is_some()
                || fact.row.owner().gen.0 != observed.generation()
                || row.as_ref().is_none_or(|row| !row.same_world_type_row(&fact.row))
            {
                return Err(source_failure("runtime delta has another captured binder or row"));
            }
        }
        predecessor.project_bindings(
            predecessor.restoration.inputs.bindings.query_scope,
            additions.entries.into_iter().map(|(name, fact)| (name, Some(fact))),
            Some(WorldCellPosition {
                original_base: *program.logical().logical_id(),
                certificate: *program.certificate_sha256(),
                next_item: ordinal + 1,
                total_items: program.items().len(),
            }),
        )
    }
}

impl CompilerWorld {
    /// Project the actual prepared runtime promotion onto the current public
    /// compiler product. Runtime still owns source gates, binding publication
    /// and the atomic installation of this prospective immutable product.
    pub fn promote_private_bindings<'a>(
        self: &Arc<Self>,
        original_public: &Arc<Self>,
        private: &Arc<Self>,
        target_scope: u64,
        visible_changes: impl IntoIterator<Item = (&'a str, Option<(tidepool_repr::SessionVarId, u64)>)>,
    ) -> Result<Arc<Self>, CompileError> {
        let position = private.position.as_ref().ok_or_else(|| {
            source_failure("private publication lacks a checked cell prefix")
        })?;
        if position.original_base != *original_public.logical_id()
            || position.next_item != position.total_items
            || private.restoration.producer != original_public.restoration.producer
            || private.restoration.profile != original_public.restoration.profile
            || self.restoration.producer != private.restoration.producer
            || self.restoration.profile != private.restoration.profile
            || original_public.restoration.inputs.sources.seal() != private.restoration.inputs.sources.seal()
            || self.restoration.inputs.sources.seal() != private.restoration.inputs.sources.seal()
        {
            return Err(source_failure("private publication has an incomplete or foreign checked lineage"));
        }
        if self.restoration.inputs.bindings.query_scope != target_scope {
            return Err(source_failure("public compiler projection belongs to another scope"));
        }
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
                    let fact = private.restoration.inputs.bindings.entries.get(name)
                        .filter(matches)
                        .or_else(|| self.restoration.inputs.bindings.entries.get(name).filter(matches))
                        .ok_or_else(|| source_failure("runtime promotion has no same issued compiler binding"))?;
                    Some(fact.clone())
                }
            };
            changes.insert(name.to_owned(), fact);
        }
        self.project_bindings(target_scope, changes, None)
    }

    fn project_bindings(
        self: &Arc<Self>,
        query_scope: u64,
        changes: impl IntoIterator<Item = (String, Option<WorldBindingFact>)>,
        position: Option<WorldCellPosition>,
    ) -> Result<Arc<Self>, CompileError> {
        let prior = &self.restoration;
        let mut inputs = prior.inputs.clone();
        inputs.bindings.query_scope = query_scope;
        let directory = Arc::new(crate::artifacts::compiler_scratch_directory()?);
        inputs.directory = directory.clone();
        let mut facts = prior.bindings.facts().clone();
        let mut row_roots = prior.binding_rows.clone();
        let mut visible_changes = BTreeMap::new();
        let mut rows = WorldColdRows {
            producer: prior.producer, files: BTreeMap::new(), images: BTreeMap::new(),
            rows: BTreeMap::new(), arenas: Default::default(), diagnostics: None,
        };
        let mut materializer = Materializer::new(&prior.primary_blocks);
        // Remove all touched heads first, then add their resulting actual
        // selections. A row shared by several changed names is issued once.
        let changes: Vec<_> = changes.into_iter().collect();
        for (occurrence, _) in &changes {
            if let Some(old) = inputs.bindings.entries.remove(occurrence) {
                inputs.bindings.metadata_bytes = inputs.bindings.metadata_bytes.checked_sub(
                    binding_metadata_bound([occurrence.len(), old.symbol.unit.len(), old.symbol.module.len(),
                        old.symbol.namespace.len(), old.symbol.occurrence.len()],
                        old.symbol.record_parent.as_ref().map_or(0, String::len))?)
                    .ok_or_else(|| source_failure("predecessor binding metadata charge underflow"))?;
                let owner = old.row.owner().module_name();
                let users = inputs.bindings.row_users.get(&owner).copied()
                    .ok_or_else(|| source_failure("predecessor binding has no row custody count"))?;
                if users == 1 {
                    inputs.bindings.row_users.remove(&owner);
                    inputs.visible_rows.remove(&owner);
                    row_roots.remove(&owner);
                    let interface = old.row.certified_interface().interface();
                    visible_changes.insert((interface.unit().to_owned(), interface.module().to_owned()),
                        ciborium::value::Value::Null);
                } else {
                    inputs.bindings.row_users.insert(owner, users - 1);
                }
                facts.remove(occurrence);
            }
        }
        for (occurrence, fact) in changes {
            let Some(fact) = fact else { continue; };
            let owner = fact.row.owner().module_name();
            if let Some(selected) = inputs.visible_rows.get(&owner) {
                if !selected.same_world_type_row(&fact.row) {
                    return Err(source_failure("promotion selects competing checked rows for one owner"));
                }
            } else if let Some(selected) = prior.inputs.visible_rows.get(&owner)
                .filter(|selected| selected.same_world_type_row(&fact.row))
            {
                let root = prior.binding_rows.get(&owner).ok_or_else(|| {
                    source_failure("predecessor checked row lacks its semantic root")
                })?;
                inputs.visible_rows.insert(owner.clone(), selected.clone());
                row_roots.insert(owner.clone(), *root);
                let interface = fact.row.certified_interface().interface();
                visible_changes.remove(&(interface.unit().to_owned(), interface.module().to_owned()));
            } else {
                let (entry, parts) = fact.row.world_type_parts()?;
                let transport = rows.row(&entry, parts)?.0;
                let semantic = world_semantic_row(&transport, &prior.producer)?;
                let (_, receipt) = fact.row.world_issuer();
                let root = materializer.materialize_value(&world_array(vec![
                    semantic, world_blob(fact.row.certified_interface().artifact_id().0), world_blob(receipt),
                ])).map_err(|error| source_failure(error.to_string()))?;
                row_roots.insert(owner.clone(), root);
                inputs.visible_rows.insert(owner.clone(), fact.row.clone());
                visible_changes.insert((entry.descriptor.owner.unit.clone(), entry.descriptor.owner.module.clone()), transport);
            }
            let users = inputs.bindings.row_users.get(&owner).copied().unwrap_or(0);
            inputs.bindings.row_users.insert(owner.clone(), users.checked_add(1).ok_or_else(source_size_failure)?);
            let symbol = &fact.symbol;
            inputs.bindings.metadata_bytes = inputs.bindings.metadata_bytes.checked_add(
                binding_metadata_bound([occurrence.len(), symbol.unit.len(), symbol.module.len(),
                    symbol.namespace.len(), symbol.occurrence.len()],
                    symbol.record_parent.as_ref().map_or(0, String::len))?
            ).ok_or_else(source_size_failure)?;
            let allowance = capture_allowance()?;
            if inputs.bindings.metadata_bytes > allowance {
                return Err(binding_capture_refusal(inputs.bindings.metadata_bytes, allowance));
            }
            facts.insert(occurrence.clone(), (
                (symbol.unit.clone(), symbol.module.clone(), symbol.namespace.clone(),
                 symbol.occurrence.clone(), symbol.record_parent.clone()),
                fact.session_var.raw(), fact.defining_scope, fact.row.owner().gen.0,
                *row_roots.get(&owner).ok_or_else(|| source_failure("captured binder lacks its row root"))?,
            ));
            inputs.bindings.entries.insert(occurrence, fact);
        }
        let bindings = materializer.materialize_map(Some(&prior.bindings), &facts)
            .map_err(|error| source_failure(error.to_string()))?;
        let staged = materializer.finish().map_err(|error| source_failure(error.to_string()))?;
        let changed_blocks = staged.blocks().iter().map(|(cid, bytes)| {
            world_array(vec![world_blob(cid.to_bytes()), world_blob(bytes)])
        }).collect();
        let (roots, primary_blocks) = prior.primary_blocks.replace_semantic_roots(&staged,
            SemanticRoots { primary: *prior.primary_root.cid(),
                associations: *prior.primary_associations.root().cid(),
                bindings: *bindings.root().cid(), lexical: prior.lexical_root })
            .map_err(|error| source_failure(error.to_string()))?;
        let product = world_restoration_product(&roots, inputs.sources.seal(), &prior.producer,
            &prior.profile, query_scope)?;
        let logical_id = world_digest(&product);
        let files: Vec<_> = rows.files.into_values().collect();
        let acquisition = rows.arenas.acquisition(rows.images.into_values().collect())?;
        let descriptor = directory.path().join("successor.cbor");
        std::fs::write(&descriptor, world_bytes(&world_array(vec![
            world_text("TPCWSUCC4"), world_blob(&product), world_array(changed_blocks),
            world_array(Vec::new()), world_array(visible_changes.into_iter().map(|((unit,module), row)| {
                world_array(vec![world_text(unit), world_text(module), row])
            }).collect()), acquisition,
        ]))?)?;
        // Only the changed transport is retained. Each current immutable row
        // owns the bytes needed for cold recovery after every ancestor drops.
        let input_files = files.iter().cloned().collect();
        let restoration = Arc::new(CompilerWorldRestoration {
            logical_id, inputs, product: product.into(), primary_root: prior.primary_root.clone(),
            primary_blocks, primary_associations: prior.primary_associations.clone(),
            primary_compiler_inputs: prior.primary_compiler_inputs.clone(), bindings,
            binding_rows: row_roots, lexical_root: prior.lexical_root,
            producer: prior.producer, profile: prior.profile, descriptor, input_files,
            successor: Some(WorldSuccessorAcquisition { predecessor: *self.logical_id(),
                realization: self.realization.clone(), files, cold: Mutex::new(None) }),
        });
        Ok(Arc::new(CompilerWorld { restoration, realization: Arc::new(Mutex::new(None)), position }))
    }
}

pub use tidepool_extract_report::world_query::{
    ClosedModuleAbsence, WorldAnswer, WorldModuleOrigin, WorldModuleReceipt, WorldModuleResolution,
    WorldQueryRejection,
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
        4u64.into(),
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
    let lexical = CompilerWorldLexicalSurface::from_publication(Vec::new(), None)?
        .materialize(context, &mut materializer)?;
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
        world_blob(lexical.to_bytes()),
    ]))
}

impl CompilerWorldOpenInputs {
    fn materialize_primary(
        &self,
        producer: [u8; 32],
    ) -> Result<(Arc<ExactDeclarationContext>, ContextRoot, ReachableBlocks), CompileError> {
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

    fn prepare_for(mut self, producer: [u8; 32], profile: [u8; 32]) -> Result<Arc<CompilerWorldRestoration>, CompileError> {
        let (primary_context, primary_root, primary_blocks) =
            self.materialize_primary(producer)?;
        self.primary_context = primary_context;
        // This cold issuer preserves the actual primary selection and native
        // lineage. Visible checked rows keep their independent issuing support;
        // ordinary operations select demanded closures from these owners.
        let primary_compiler_inputs = self
            .primary_context
            .capture_world_primary_compiler_inputs(producer)?;
        crate::host_work::record_host_work(
            crate::host_work::HostWorkCounter::ColdWorldCompilerBasisCapture,
            1,
        );
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
            &self.primary_context,
            &producer,
            self.directory.path(),
            &mut rows,
            &mut binding_materializer,
            &mut remaining_raw,
        )?;
        let mut binding_rows = imbl::OrdMap::new();
        let mut visible = Vec::with_capacity(self.visible_rows.len());
        for artifact in self.visible_rows.values() {
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
        let lexical_root = self.lexical.materialize(&self.primary_context, &mut binding_materializer)?;
        let staged = binding_materializer
            .finish()
            .map_err(|error| source_failure(error.to_string()))?;
        let (semantic_roots, primary_blocks) = primary_blocks
            .replace_semantic_roots(
                &staged,
                SemanticRoots {
                    primary: *primary_root.cid(),
                    associations: *primary_associations.root().cid(),
                    bindings: *bindings.root().cid(),
                    lexical: lexical_root,
                },
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
        let source_path = self.directory.path().join("sources.bin");
        std::fs::write(&source_path, self.sources.encoded())?;
        let files = rows.files.into_values().collect();
        let acquisition = rows
            .arenas
            .acquisition(rows.images.into_values().collect())?;
        let descriptor = self.directory.path().join("world.cbor");
        let encoded = world_bytes(&world_array(vec![
            world_text("TPCWORLD"),
            4u64.into(),
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
            primary_compiler_inputs,
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
            let (acquisition, files) = match &restoration.successor {
                Some(successor) => (A::Successor {
                    predecessor: world_hex(&successor.predecessor),
                    realization: successor.realization.lock()
                        .map_err(|_| source_failure("predecessor realization lock poisoned"))?
                        .clone().unwrap_or_else(|| "not-resident".into()),
                    descriptor: restoration.descriptor.clone(),
                }, successor.files.clone()),
                None => (A::Complete(restoration.descriptor.clone()),
                    restoration.input_files.iter().cloned().collect()),
            };
            let (mut cmd, endpoint) = world_command(identity)?;
            cmd.ensure_exact_inputs(ExactInputsRequest { root: root.clone(), acquisition });
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

            let response = O::decode(&run.output.stdout).map_err(|_| world_response_error(false))?;
            let response = match (response, &restoration.successor) {
                (O::NeedsAcquisition(world), Some(successor)) if world == root => {
                    // Missing exact predecessor backing is ordinary eviction.
                    // Rehydrate the SAME retained immutable product; no cell
                    // compilation or effect execution is retried here.
                    let mut cached = successor.cold.lock()
                        .map_err(|_| source_failure("successor recovery lock poisoned"))?;
                    if cached.is_none() {
                        let cold = restoration.inputs.clone().prepare_for(restoration.producer, restoration.profile)?;
                        if cold.logical_id != restoration.logical_id {
                            return Err(source_failure("successor cold restoration changed logical product"));
                        }
                        *cached = Some(cold);
                    }
                    let cold = cached.as_ref().expect("checked successor cold restoration");
                    let (mut cmd, endpoint) = world_command(identity)?;
                    cmd.ensure_exact_inputs(ExactInputsRequest {
                        root: root.clone(), acquisition: A::Complete(cold.descriptor.clone()),
                    });
                    O::decode(&world_execute(&cmd, endpoint, cold.input_files.iter().cloned().collect())?.output.stdout)
                        .map_err(|_| world_response_error(false))?
                }
                (response, _) => response,
            };
            match response {
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

/// Physical completion of one world-bound preparation. Only the checked
/// program issuer in this module can turn these observations into authority.
struct WorldCellOutputAdmission {
    pub(crate) origin: tidepool_extract_cmd::CompilerTransactionOrigin,
    pub(crate) reservation: Arc<crate::cell_plan::CompilerWorldCellReservation>,
    pub(crate) directory: Arc<tempfile::TempDir>,
    pub(crate) observations: Arc<[u8]>,
    pub(crate) items: Vec<ciborium::value::Value>,
    pub(crate) common_files: BTreeMap<PathBuf, [u8; 32]>,
    pub(crate) demanded_rows: Vec<Arc<CheckedValueArtifact>>,
    pub(crate) captured_files: BTreeMap<PathBuf, WorldOutputFile>,
    pub(crate) validation: crate::recovery_artifacts::PackageInterfaceValidation,
    segments: Vec<Arc<crate::checked_cell::CheckedTypedSegmentPlan>>,
    item_proofs: Vec<WorldItemProof>,
}

impl WorldCellOutputAdmission {
    pub(crate) fn reservation(&self) -> &Arc<crate::cell_plan::CompilerWorldCellReservation> {
        &self.reservation
    }

    pub(crate) fn primary_context(&self) -> &Arc<ExactDeclarationContext> {
        &self.reservation.plan().world().restoration.inputs.primary_context
    }

    pub(crate) fn primary_compiler_inputs(
        &self,
    ) -> &Arc<crate::declaration_context::RequestCompilerInputs> {
        &self.reservation.plan().world().restoration.primary_compiler_inputs
    }
    pub(crate) fn segments(&self) -> &[Arc<crate::checked_cell::CheckedTypedSegmentPlan>] {
        &self.segments
    }

    /// One decoded item proof per reservation slot, correlated with the same
    /// physical output that produced `items` and `captured_files`.
    pub(crate) fn item_proofs(&self) -> &[WorldItemProof] {
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

/// Compiler-issued identity decoded from one item's own `TPCWITEM1` output.
/// Admission retains this proof so a later constructor does not re-decode
/// transport bytes, and so the item's binder, signature, provenance and
/// native group ordinal stay correlated with the physical output that carried
/// them. It grants no runtime value or execution permission by itself.
// boundary: see CELL-PREPARATION-BOUNDARY.md
pub(crate) struct WorldItemProof {
    index: usize,
    ordinal: u64,
    generation: u64,
    entry: String,
    native_ordinal: u32,
    segment_digest: String,
    symbol: tidepool_repr::execution_schema::SymbolIdentity,
    binders: Vec<crate::checked_cell::WorldItemBinder>,
    signatures: Vec<crate::checked_cell::ExactCheckedSignature>,
    manifest: BTreeMap<PathBuf, [u8; 32]>,
}

impl WorldItemProof {
    pub(crate) fn index(&self) -> usize {
        self.index
    }
    pub(crate) fn ordinal(&self) -> u64 {
        self.ordinal
    }
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
    pub(crate) fn entry(&self) -> &str {
        &self.entry
    }
    /// Native original-group ordinal the compiler issued for this item entry.
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
    pub(crate) fn signatures(&self) -> &[crate::checked_cell::ExactCheckedSignature] {
        &self.signatures
    }
    pub(crate) fn manifest(&self) -> &BTreeMap<PathBuf, [u8; 32]> {
        &self.manifest
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
    pub fn new(
        sha256: [u8; 32],
        revision: impl Into<String>,
    ) -> Result<Self, CompileError> {
        let revision = revision.into();
        if sha256 == [0; 32]
            || revision.is_empty()
            || revision.len() > 65536
        {
            return Err(source_failure("world source selection is incomplete"));
        }
        Ok(Self {
            sha256,
            revision,
        })
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
    ordinal: usize,
    generation: u64,
    entry: String,
    native_ordinal: u32,
    segment_digest: String,
    symbol: tidepool_repr::execution_schema::SymbolIdentity,
    manifest: BTreeMap<PathBuf, [u8; 32]>,
    binders: Vec<crate::checked_cell::WorldItemBinder>,
    checked: crate::checked_cell::CheckedItem,
}

impl CheckedWorldCellItem {
    pub(crate) fn checked_item_observation(&self) -> &crate::checked_cell::CheckedItem {
        &self.checked
    }

    pub fn ordinal(&self) -> usize {
        self.ordinal
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn entry(&self) -> &str {
        &self.entry
    }

    pub fn native_ordinal(&self) -> u32 {
        self.native_ordinal
    }

    pub fn segment_digest(&self) -> &str {
        &self.segment_digest
    }

    pub fn symbol(&self) -> &tidepool_repr::execution_schema::SymbolIdentity {
        &self.symbol
    }

    pub(crate) fn binders(&self) -> &[crate::checked_cell::WorldItemBinder] {
        &self.binders
    }

    pub(crate) fn signatures(&self) -> &[crate::checked_cell::ExactCheckedSignature] {
        self.checked.signatures()
    }

    pub(crate) fn manifest(&self) -> &BTreeMap<PathBuf, [u8; 32]> {
        &self.manifest
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
    certificate_sha256: [u8; 32],
    producer_identity: [u8; 32],
}

impl CheckedWorldCellProgram {
    pub(crate) fn observations(&self) -> &Arc<[u8]> {
        &self.output.observations
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
        world_digest(self.output.reservation.plan().receipt())
    }

    pub fn certificate_sha256(&self) -> &[u8; 32] {
        &self.certificate_sha256
    }

    pub fn items(&self) -> &[CheckedWorldCellItem] {
        &self.items
    }

    pub fn item(&self, ordinal: usize) -> Option<&CheckedWorldCellItem> {
        self.items.get(ordinal)
    }

    pub(crate) fn primary_context(&self) -> &Arc<ExactDeclarationContext> {
        &self
            .output
            .reservation
            .plan()
            .world()
            .restoration
            .inputs
            .primary_context
    }

    pub(crate) fn primary_compiler_inputs(
        &self,
    ) -> &Arc<crate::declaration_context::RequestCompilerInputs> {
        &self
            .output
            .reservation
            .plan()
            .world()
            .restoration
            .primary_compiler_inputs
    }

    pub(crate) fn segments(&self) -> &[Arc<crate::checked_cell::CheckedTypedSegmentPlan>] {
        &self.output.segments
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

    pub(crate) fn item_proofs(&self) -> &[WorldItemProof] {
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
        if output.item_proofs.len() != reservation.slots().len()
            || output.items.len() != reservation.slots().len()
        {
            return Err(source_failure(
                "world cell preparation does not cover every reserved slot",
            ));
        }
        let checked_items = crate::checked_cell::admit_world_checked_items(
            &output.observations,
            reservation,
            &output.segments,
            &output.item_proofs,
            &output.validation.inventory,
        )?;
        // TODO(engine V): carry these same checked items and captured products
        // through World original-execution/typed-entry assembly into
        // PreparedCheckedItemOutput. SegmentCursor's finite request continuation
        // and CellProgram's finite ParsedCellPlan are not World evidence. Keep
        // refusal until those owners receive the World reservation; executable
        // sealing still requires finish_cell_program_admitted's matching END
        // and HostAdmissionCancellation. No reread or finite request substitute.
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
                || proof.generation() != world_cell_generation(slot)
                || proof.entry().is_empty()
                || !output
                    .segments
                    .iter()
                    .any(|segment| segment.digest() == proof.segment_digest())
            {
                return Err(source_failure(
                    "world cell item proof differs from its reservation",
                ));
            }
            items.push(CheckedWorldCellItem {
                ordinal,
                generation: proof.generation(),
                entry: proof.entry().to_owned(),
                native_ordinal: proof.native_ordinal(),
                segment_digest: proof.segment_digest().to_owned(),
                symbol: proof.symbol().clone(),
                manifest: proof.manifest().clone(),
                binders: proof.binders().to_vec(),
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
        Ok(Self {
            output,
            original,
            target,
            logical,
            items,
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
    frame(b"TidepoolCheckedWorldCellProgram3");
    frame(reservation.admission_digest());
    frame(&world_digest(reservation.plan().receipt()));
    frame(logical.logical_id());
    frame(logical.producer_sha256());
    frame(logical.profile_sha256());
    for selection in [original, target] {
        frame(selection.sha256());
        frame(selection.revision().as_bytes());
    }
    for item in items {
        frame(&(item.ordinal as u64).to_le_bytes());
        frame(&item.generation.to_le_bytes());
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
        for signature in item.signatures() {
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
            "not-resident",
            plan.recipe(),
            plan.receipt(),
            reservation
                .slots()
                .iter()
                .enumerate()
                .map(|(ordinal, slot)| {
                    use crate::checked_cell::CheckedPlannedCellSlot as S;
                    use tidepool_extract_cmd::PlannedCellSlotRef as P;
                    (
                        ordinal as u64,
                        match slot {
                            S::Prologue { declaration } => P::Prologue(*declaration),
                            S::Declaration { declaration } => P::Declaration(*declaration),
                            S::Bind { value } => P::Bind(*value),
                            S::Expression {
                                capture,
                                observation_name,
                            } => P::Expression {
                                generation: *capture,
                                observation: observation_name,
                            },
                        },
                    )
                }),
            directory.path(),
            &output,
        )
        .map_err(|error| source_failure(error.to_string()))?;
        let mut request = tidepool_extract_cmd::WorldCellPrepareRequest {
            world: world_id,
            realization: "not-resident".into(),
            recipe: plan.recipe_owned(),
            plan: plan.receipt_owned(),
            admission: *reservation.admission_digest(),
            slots: world_cell_slots(&reservation),
            directory: directory.path().to_path_buf(),
            output: output.clone(),
        };
        // Source and parser receipt remain shared. Bound the actual frame
        // before Ensure or transport encoding performs bulk allocation.
        request
            .validate_transport()
            .map_err(|error| source_failure(error.to_string()))?;
        if !tidepool_extract_cmd::retain_compiler_input_directory(Arc::clone(&directory)) {
            return Err(source_failure(
                "world cell preparation requires its compiler scope",
            ));
        }
        request.realization = ensure_world(plan.world(), endpoint.identity())?;
        let (mut command, bound) = world_command(endpoint.identity())?;
        command
            .world_cell_prepare(request)
            .map_err(|error| source_failure(error.to_string()))?;
        let diagnostics =
            crate::artifacts::CompilerDiagnosticCapture::start(directory.path(), &command);
        let run = world_execute(&command, bound, Vec::new())?;
        diagnostics.completed(
            directory.path(),
            &command,
            run.success(),
            &run.output.stderr,
        );
        crate::diag::decode_extract_result(run.success(), &run.output.stdout, &run.output.stderr)?;
        let validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
        let bytes = crate::certified_products::read_bounded_with_operation(
            &output,
            8 << 20,
            &validation.inventory,
        )
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
        admit_world_preparation_response(origin, reservation, directory, &bytes, validation)
    })
}

fn world_cell_slots(
    reservation: &crate::cell_plan::CompilerWorldCellReservation,
) -> Vec<(u64, tidepool_extract_cmd::PlannedCellSlot)> {
    use crate::checked_cell::CheckedPlannedCellSlot as S;
    use tidepool_extract_cmd::PlannedCellSlot as P;
    reservation
        .slots()
        .iter()
        .enumerate()
        .map(|(index, slot)| {
            (
                index as u64,
                match slot {
                    S::Prologue { declaration } => P::Prologue(*declaration),
                    S::Declaration { declaration } => P::Declaration(*declaration),
                    S::Bind { value } => P::Bind(*value),
                    S::Expression {
                        capture,
                        observation_name,
                    } => P::Expression {
                        generation: *capture,
                        observation: observation_name.clone(),
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
        return Err(world_response_error(false));
    };
    inventory
        .charge_value_copies(value, 1)
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
    let mut files = BTreeMap::new();
    for entry in entries {
        crate::host_work::checkpoint()?;
        let entry = answer_array(entry, 2)?;
        let path = PathBuf::from(answer_text(&entry[0])?);
        relative_source_path(&path).map_err(|_| world_response_error(false))?;
        if files.insert(path, answer_hash(&entry[1])?).is_some() {
            return Err(world_response_error(false));
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
        _ => return Err(world_response_error(false)),
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
        return Err(world_response_error(true));
    }
    Ok(())
}

fn admit_world_preparation_response(
    origin: tidepool_extract_cmd::CompilerTransactionOrigin,
    reservation: Arc<crate::cell_plan::CompilerWorldCellReservation>,
    directory: Arc<tempfile::TempDir>,
    bytes: &[u8],
    mut validation: crate::recovery_artifacts::PackageInterfaceValidation,
) -> Result<Result<WorldCellOutputAdmission, WorldQueryRejection>, CompileError> {
    let packet = validation
        .inventory
        .decode_value(bytes, 8 << 20)
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
    let fields = answer_array(&packet, 9)?;
    let plan = reservation.plan();
    let base = plan.world().logical_id();
    let plan_digest = world_digest(plan.receipt());
    if answer_text(&fields[0])? != "TPCWPREP3"
        || answer_hash(&fields[1])? != *base
        || answer_hash(&fields[2])? != plan_digest
        || answer_hash(&fields[3])? != *reservation.admission_digest()
    {
        return Err(world_response_error(true));
    }
    let outcome = answer_array(&fields[4], 2)?;
    if answer_text(&outcome[0])? == "rejected" {
        if !answer_blob(&fields[5])?.is_empty()
            || !answer_array(&fields[6], 0)?.is_empty()
            || !answer_array(&fields[7], 0)?.is_empty()
            || !answer_array(&fields[8], 0)?.is_empty()
        {
            return Err(world_response_error(false));
        }
        return Ok(Err(match answer_u64(&outcome[1])? {
            0 => WorldQueryRejection::Unsupported,
            1 => WorldQueryRejection::Unavailable,
            2 => WorldQueryRejection::Corrupt,
            _ => return Err(world_response_error(false)),
        }));
    }
    if answer_text(&outcome[0])? != "prepared" {
        return Err(world_response_error(false));
    }
    validation
        .inventory
        .charge_value_copies(&fields[8], 1)
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
    let segments = crate::checked_cell::decode_world_typed_segment_plans(&fields[8], &reservation)?;
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
    let mut items = Vec::with_capacity(completed.len());
    let mut item_proofs = Vec::with_capacity(completed.len());
    let mut issued_native_ordinals = BTreeMap::<String, u32>::new();
    let mut captured_files = BTreeMap::new();
    for (index, (completed, slot)) in completed.iter().zip(reservation.slots()).enumerate() {
        crate::host_work::checkpoint()?;
        let completed = answer_array(completed, 3)?;
        if answer_u64(&completed[0])? != index as u64
            || answer_u64(&completed[1])? != world_cell_generation(slot)
        {
            return Err(world_response_error(true));
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
            return Err(world_response_error(true));
        }
        let item = validation
            .inventory
            .decode_value(&item_bytes, 4 << 20)
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let observed = answer_array(&item, 13)?;
        let &(segment_index, item_index) = by_ordinal
            .get(&index)
            .ok_or_else(|| world_response_error(true))?;
        let segment = &segments[segment_index];
        let typed_item = &segment.items()[item_index];
        if answer_text(&observed[0])? != "TPCWITEM1"
            || answer_hash(&observed[1])? != *base
            || answer_hash(&observed[2])? != plan_digest
            || answer_hash(&observed[3])? != *reservation.admission_digest()
            || answer_u64(&observed[4])? != index as u64
            || answer_u64(&observed[5])? != world_cell_generation(slot)
            || answer_hash(&observed[7])? != world_digest(plan.items()[index].source().as_bytes())
            || answer_text(&observed[6])? != segment.digest()
            || answer_text(&observed[8])? != typed_item.entry()
        {
            return Err(world_response_error(true));
        }
        validation
            .inventory
            .charge_value_copies(&observed[9], 1)
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let symbol = world_output_symbol(&observed[9])?;
        validate_world_native_entry_observation(&symbol, typed_item)?;
        // Field 10 is the native original-group ordinal the compiler issued for
        // this item entry. Retain it, and refuse a preparation whose two items
        // claim different ordinals for one entry.
        validation
            .inventory
            .charge_value_copies(&observed[10], 1)
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let native_ordinal = u32::try_from(answer_u64(&observed[10])?)
            .map_err(|_| world_response_error(false))?;
        match issued_native_ordinals.get(typed_item.entry()) {
            Some(issued) if *issued != native_ordinal => {
                return Err(world_response_error(true));
            }
            Some(_) => {}
            None => {
                issued_native_ordinals.insert(typed_item.entry().to_owned(), native_ordinal);
            }
        }
        // Field 11 is the two-list `[binders, signatures]`. Binder order and
        // identity must match the reservation, and each signature must name
        // this typed item's entry and ordered capture.
        validation
            .inventory
            .charge_value_copies(&observed[11], 1)
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let evidence = answer_array(&observed[11], 2)?;
        let expected_bindings = reservation
            .expected_bindings()
            .get(index)
            .ok_or_else(|| world_response_error(true))?;
        let binders =
            crate::checked_cell::decode_world_item_binders(&evidence[0], expected_bindings)?;
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
            return Err(world_response_error(false));
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
        item_proofs.push(WorldItemProof {
            index,
            ordinal: index as u64,
            generation: world_cell_generation(slot),
            entry: typed_item.entry().to_owned(),
            native_ordinal,
            segment_digest: segment.digest().to_owned(),
            symbol,
            binders,
            signatures,
            manifest,
        });
        items.push(item);
    }
    let ciborium::value::Value::Array(input_refs) = &fields[6] else {
        return Err(world_response_error(false));
    };
    let world = plan.world();
    validation
        .inventory
        .charge_value_copies(&fields[6], 1)
        .and_then(|_| {
            validation
                .inventory
                .reserve::<(String, Arc<CheckedValueArtifact>)>(input_refs.len())
        })
        .and_then(|_| validation.inventory.reserve::<&str>(input_refs.len()))
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
    let mut demanded = BTreeMap::<String, Arc<CheckedValueArtifact>>::new();
    let mut occurrences = std::collections::BTreeSet::new();
    for input in input_refs {
        crate::host_work::checkpoint()?;
        let input = answer_array(input, 6)?;
        let occurrence = answer_text(&input[0])?;
        if !occurrences.insert(occurrence) {
            return Err(world_response_error(true));
        }
        let actual = world
            .restoration
            .inputs
            .bindings
            .entries
            .get(occurrence)
            .ok_or_else(|| world_response_error(true))?;
        if world_output_symbol(&input[1])? != actual.symbol
            || answer_u64(&input[2])? != actual.session_var.raw()
            || answer_u64(&input[3])? != actual.defining_scope
            || answer_u64(&input[4])? != actual.row.owner().gen.0
        {
            return Err(world_response_error(true));
        }
        // The service returns the exact root-bound row fact, not a caller's
        // module/image guess or a fresh certificate.
        let row =
            cid::Cid::try_from(answer_blob(&input[5])?).map_err(|_| world_response_error(false))?;
        let expected = world
            .restoration
            .bindings
            .facts()
            .get(occurrence)
            .ok_or_else(|| world_response_error(true))?;
        if row != expected.4 {
            return Err(world_response_error(true));
        }
        // The canonical session module renderer uses a fixed prefix and at
        // most twenty decimal generation digits.
        validation
            .inventory
            .charge(64)
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        match demanded.entry(actual.row.owner().module_name()) {
            std::collections::btree_map::Entry::Occupied(old)
                if !old.get().same_world_type_row(&actual.row) =>
            {
                return Err(world_response_error(true))
            }
            std::collections::btree_map::Entry::Occupied(_) => {}
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Arc::clone(&actual.row));
            }
        }
    }
    let common_files = world_output_manifest(&fields[7], &validation.inventory)?;
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
        origin,
        reservation,
        directory,
        observations: Arc::from(answer_blob(&fields[5])?),
        items,
        common_files,
        captured_files,
        validation,
        segments,
        item_proofs,
        demanded_rows: demanded.into_values().collect(),
    }))
}

fn capture_world_output(
    root: &tempfile::TempDir,
    relative: PathBuf,
    digest: [u8; 32],
    files: &mut BTreeMap<PathBuf, WorldOutputFile>,
    validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
) -> Result<(), CompileError> {
    relative_source_path(&relative).map_err(|_| world_response_error(false))?;
    if files.contains_key(&relative) {
        return Err(world_response_error(false));
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
        return Err(world_response_error(true));
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
                return Err(world_response_error(true));
            }
        }
        origin @ WorldModuleOrigin::Home { path, .. } => {
            if relative_source_path(Path::new(path)).is_err()
                || sources.selected_sources.get(&receipt.module) != Some(origin)
            {
                return Err(world_response_error(true));
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
    use tidepool_extract_report::world_query::{
        self, WorldAnswerDecodeError, WorldQueryExpectation, WorldQueryKind,
    };
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
    .map_err(|error| match error {
        WorldAnswerDecodeError::Malformed(_) => world_response_error(false),
        WorldAnswerDecodeError::ProvenanceMismatch(_) => world_response_error(true),
    })?;
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
                return Err(world_response_error(true));
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
    let world = CompilerWorld {
        restoration: Arc::clone(restoration),
        realization: Arc::new(Mutex::new(None)),
        position: None,
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
            CompilerWorldBindings::from_published_bindings(0, std::iter::empty()).unwrap(),
            Arc::clone(&sources),
            CompilerWorldLexicalSurface::from_publication(Vec::new(), None).unwrap(),
        )
        .unwrap();
        assert_eq!(cold.primary_context.snapshot().producer_sha256(), [0; 32]);
        let (bound, root, blocks) = cold
            .materialize_primary(
                CanonicalProducerIdentity::from_compiler(endpoint.identity()).sha256(),
            )
            .unwrap();
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
            CompilerWorldLexicalSurface::from_publication(Vec::new(), None).unwrap(),
        )
        .unwrap();
        assert_eq!(
            issued
                .materialize_primary(
                    CanonicalProducerIdentity::from_compiler(endpoint.identity()).sha256()
                )
                .unwrap()
                .1,
            root
        );
        let wrong = crate::artifacts::ModuleCandidateOffer::compiler_world_inputs(
            Some(Arc::new(
                (*cold.primary_context).clone().admit_producer(raw).unwrap(),
            )),
            CompilerWorldBindings::from_published_bindings(0, std::iter::empty()).unwrap(),
            sources,
            CompilerWorldLexicalSurface::from_publication(Vec::new(), None).unwrap(),
        )
        .unwrap();
        assert!(wrong
            .materialize_primary(
                CanonicalProducerIdentity::from_compiler(endpoint.identity()).sha256()
            )
            .is_err());
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
        assert_eq!(fields.len(), 6);
        assert!(fields[2].as_array().unwrap().is_empty());
        let blocks = fields[4].as_array().unwrap();
        for root in [&fields[0], &fields[1], &fields[5]] {
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
    fn source_selection_names_the_captured_namespace_and_refuses_missing_identity() {
        let original = WorldSourceSelection::new([7; 32], "captured-source").unwrap();
        let target = WorldSourceSelection::new([7; 32], "captured-source").unwrap();
        assert_eq!(original, target);
        assert!(WorldSourceSelection::new([0; 32], "captured-source").is_err());
        assert!(WorldSourceSelection::new([7; 32], "").is_err());
        assert!(WorldSourceSelection::new([7; 32], "x".repeat(65537)).is_err());
    }

    #[test]
    fn lexical_surface_preserves_order_qualification_and_explicit_empty_imports() {
        let primary = ExactDeclarationContext::new(&[], &[], Vec::new()).unwrap();
        let root = |imports: &[&str]| {
            let store = MemoryBlockstore::new();
            let mut materializer = Materializer::new(&store);
            CompilerWorldLexicalSurface::from_publication(
                imports.iter().map(|spec| (*spec).to_owned()).collect(), None,
            ).unwrap().materialize(&primary, &mut materializer).unwrap()
        };
        assert_eq!(root(&["Data.List ()"]), root(&["Data.List ()"]));
        assert_ne!(root(&[]), root(&["Data.List ()"]));
        assert_ne!(root(&["Data.List"]), root(&["qualified Data.List as L"]));
        assert_ne!(root(&["Data.List hiding (map)"]), root(&["Data.List"]));
        assert_ne!(root(&["Data.List", "Data.Maybe"]), root(&["Data.Maybe", "Data.List"]));
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
        let lexical = materializer.materialize_value(&("lexical", vec![leaf])).unwrap();
        let extra = materializer.materialize_value(&("extra", 1u64)).unwrap();
        let transient = materializer
            .materialize_value(&("transient", vec![context, associations, bindings, lexical, extra]))
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
