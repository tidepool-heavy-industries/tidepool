use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) const WORKER_REQUEST_FLAG: &str = "--worker-request-v25";
const MAGIC: &[u8; 8] = b"TPREQ025";

/// A probe flag deliberately outside the versioned request grammar above: it
/// asks a worker binary to print the request flag it was built against and
/// exit, so the launcher can compare its own [`WORKER_REQUEST_FLAG`] against
/// a resolved worker's before ever sending it a real request. Mirrored in
/// `bridge/haskell/app/Main.hs`'s argument handling.
pub(crate) const PRINT_WORKER_REQUEST_FLAG: &str = "--print-worker-request-flag";

/// Transport data only: the host and compiler owners authenticate these references.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactInputsRequest {
    pub root: String,
    pub acquisition: ExactInputsAcquisition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExactInputsAcquisition {
    Resident(String),
    /// Strict cold-world descriptor; input descriptors retain their existing owner.
    Complete(PathBuf),
    Successor { predecessor: String, realization: String, descriptor: PathBuf },
}

impl ExactInputsRequest {
    fn valid(&self) -> bool {
        let valid = |value: &str| !value.is_empty() && !value.contains('\0');
        valid_world_identity(&self.root)
            && match &self.acquisition {
                ExactInputsAcquisition::Resident(reference) => valid(reference),
                ExactInputsAcquisition::Complete(path) => path.to_str().is_some_and(valid),
                ExactInputsAcquisition::Successor { predecessor, realization, descriptor } => valid_world_identity(predecessor) && valid(realization) && descriptor.to_str().is_some_and(valid),
            }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExactInputsRejection {
    UnsupportedOperation,
    InvalidRealization,
    InvalidRoot,
    AcquisitionRejected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnsureExactInputsOutcome {
    Ready { world: String, realization: String },
    NeedsAcquisition(String),
    Rejected(ExactInputsRejection),
}

impl EnsureExactInputsOutcome {
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.get(..8) != Some(b"TPEXO002") {
            return Err(ProtocolError::InvalidHeader);
        }
        let mut decoder = Decoder { bytes, cursor: 8 };
        match decoder.byte()? {
            0 => {
                let world = decoder.text()?;
                let realization = decoder.text()?;
                if !valid_world_identity(world) || !valid_reference(realization) {
                    return Err(ProtocolError::InvalidExactInputs);
                }
                decoder.finish()?;
                Ok(Self::Ready {
                    world: world.to_owned(),
                    realization: realization.to_owned(),
                })
            }
            1 => {
                let world = decoder.text()?;
                if !valid_world_identity(world) {
                    return Err(ProtocolError::InvalidExactInputs);
                }
                decoder.finish()?;
                Ok(Self::NeedsAcquisition(world.to_owned()))
            }
            2 => {
                let cause = match decoder.byte()? {
                    0 => ExactInputsRejection::UnsupportedOperation,
                    1 => ExactInputsRejection::InvalidRealization,
                    2 => ExactInputsRejection::InvalidRoot,
                    3 => ExactInputsRejection::AcquisitionRejected,
                    _ => return Err(ProtocolError::InvalidExactInputs),
                };
                decoder.finish()?;
                Ok(Self::Rejected(cause))
            }
            _ => Err(ProtocolError::InvalidExactInputs),
        }
    }
}

fn valid_world_identity(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_reference(value: &str) -> bool {
    !value.is_empty() && !value.contains('\0')
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorldQueryMode {
    Info,
    Type,
}

/// Transport question only; an actual held compiler world authorizes execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldPublicModuleRequest {
    pub world: String,
    pub realization: String,
    pub mode: WorldQueryMode,
    pub query: StructuredInspection,
    pub output: PathBuf,
}

impl WorldPublicModuleRequest {
    fn valid(&self) -> bool {
        valid_world_identity(&self.world)
            && valid_reference(&self.realization)
            && self.output.to_str().is_some_and(valid_reference)
            && matches!(&self.query.scope, InspectionScope::PublicModule(module) if valid_reference(module))
    }
}

/// Source recipe DATA. The existing runtime admission and held-world owner
/// supply authority; these fields never select a second compiler inventory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldCellRecipe {
    pub source: String,
    pub template: String,
    pub turn_templates: Vec<(String, String)>,
}

impl WorldCellRecipe {
    fn encoded_len(&self) -> usize {
        4 + self.source.len()
            + 4
            + self.template.len()
            + 4
            + self
                .turn_templates
                .iter()
                .map(|(kind, body)| 8 + kind.len() + body.len())
                .sum::<usize>()
    }

    fn valid(&self) -> bool {
        self.source.len() <= 2 << 20
            && self.template.len() <= 2 << 20
            && self.turn_templates.len() <= 256
            && self.turn_templates.iter().all(|(kind, source)| {
                valid_reference(kind) && kind.len() <= 65536 && source.len() <= 2 << 20
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldCellPlanRequest {
    pub world: String,
    pub realization: String,
    pub recipe: Arc<WorldCellRecipe>,
    pub output: PathBuf,
}

fn valid_cell_path(path: &Path) -> bool {
    path.to_str()
        .is_some_and(|value| valid_reference(value) && value.len() <= 65536)
}

impl WorldCellPlanRequest {
    fn valid(&self) -> bool {
        self.validate_transport().is_ok()
    }

    /// Check the existing encoded frame budget without copying source bodies.
    pub fn validate_transport(&self) -> Result<(), ProtocolError> {
        if !valid_world_identity(&self.world)
            || !valid_reference(&self.realization)
            || !self.recipe.valid()
            || !valid_cell_path(&self.output)
        {
            return Err(ProtocolError::InvalidWorldCell);
        }
        check_world_cell_bytes(
            21 + 1
                + 4
                + self.world.len()
                + 4
                + self.realization.len()
                + self.recipe.encoded_len()
                + 4
                + self.output.as_os_str().len(),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlannedCellSlot {
    Prologue(u64),
    Declaration(u64),
    Bind(u64),
    Expression {
        generation: u64,
        observation: String,
    },
}

impl PlannedCellSlot {
    fn generation(&self) -> u64 {
        match self {
            Self::Prologue(generation)
            | Self::Declaration(generation)
            | Self::Bind(generation)
            | Self::Expression { generation, .. } => *generation,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldCellPrepareRequest {
    pub world: String,
    pub realization: String,
    pub recipe: Arc<WorldCellRecipe>,
    pub plan: Arc<[u8]>,
    pub admission: [u8; 32],
    pub slots: Vec<(u64, PlannedCellSlot)>,
    pub directory: PathBuf,
    pub output: PathBuf,
}

/// Borrowed reservation fields used before an operation copies observation names.
#[derive(Clone, Copy)]
pub enum PlannedCellSlotRef<'a> {
    Prologue(u64),
    Declaration(u64),
    Bind(u64),
    Expression {
        generation: u64,
        observation: &'a str,
    },
}

impl PlannedCellSlot {
    fn borrowed(&self) -> PlannedCellSlotRef<'_> {
        match self {
            Self::Prologue(generation) => PlannedCellSlotRef::Prologue(*generation),
            Self::Declaration(generation) => PlannedCellSlotRef::Declaration(*generation),
            Self::Bind(generation) => PlannedCellSlotRef::Bind(*generation),
            Self::Expression {
                generation,
                observation,
            } => PlannedCellSlotRef::Expression {
                generation: *generation,
                observation,
            },
        }
    }
}

impl WorldCellPrepareRequest {
    fn valid(&self) -> bool {
        self.validate_transport().is_ok()
    }

    pub fn validate_transport(&self) -> Result<(), ProtocolError> {
        Self::validate_borrowed(
            &self.world,
            &self.realization,
            &self.recipe,
            &self.plan,
            self.slots
                .iter()
                .map(|(ordinal, slot)| (*ordinal, slot.borrowed())),
            &self.directory,
            &self.output,
        )
    }

    /// Bound the complete existing frame before copying caller-owned names.
    /// Only the bounded generation sets allocate; source and receipt are borrowed.
    pub fn validate_borrowed<'a>(
        world: &str,
        realization: &str,
        recipe: &WorldCellRecipe,
        plan: &[u8],
        slots: impl IntoIterator<Item = (u64, PlannedCellSlotRef<'a>)>,
        directory: &Path,
        output: &Path,
    ) -> Result<(), ProtocolError> {
        if !valid_world_identity(world)
            || !valid_reference(realization)
            || !recipe.valid()
            || plan.is_empty()
            || plan.len() > 8 << 20
            || !valid_cell_path(directory)
            || !valid_cell_path(output)
        {
            return Err(ProtocolError::InvalidWorldCell);
        }
        let mut declarations = BTreeSet::new();
        let mut values = BTreeSet::new();
        let mut slot_bytes = 0usize;
        for (index, (ordinal, slot)) in slots.into_iter().enumerate() {
            if index >= 10000 || ordinal != index as u64 {
                return Err(ProtocolError::InvalidWorldCell);
            }
            let (generation, unique, extra) = match slot {
                PlannedCellSlotRef::Prologue(generation)
                | PlannedCellSlotRef::Declaration(generation) => {
                    (generation, declarations.insert(generation), 0)
                }
                PlannedCellSlotRef::Bind(generation) => (generation, values.insert(generation), 0),
                PlannedCellSlotRef::Expression {
                    generation,
                    observation,
                } => {
                    if !valid_reference(observation) || observation.len() > 65536 {
                        return Err(ProtocolError::InvalidWorldCell);
                    }
                    (generation, values.insert(generation), 4 + observation.len())
                }
            };
            if generation == 0 || !unique {
                return Err(ProtocolError::InvalidWorldCell);
            }
            slot_bytes += 17 + extra;
        }
        check_world_cell_bytes(
            21 + 1
                + 4
                + world.len()
                + 4
                + realization.len()
                + recipe.encoded_len()
                + 4
                + plan.len()
                + 32
                + 4
                + slot_bytes
                + 4
                + directory.as_os_str().len()
                + 4
                + output.as_os_str().len(),
        )
    }
}

// read_request charges the cwd and argv PAYLOAD bytes, including this flag
// and the typed request's hex expansion. The actual cwd is known only by the
// physical transport; this borrowed preflight reserves the known argv part
// before cloning/encoding/appending bulk source fields. It does not raise or
// independently copy the daemon's operational receive limit.
fn check_world_cell_bytes(binary_bytes: usize) -> Result<(), ProtocolError> {
    let declared = (binary_bytes as u64)
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(WORKER_REQUEST_FLAG.len() as u64))
        .unwrap_or(u64::MAX);
    let limit = crate::daemon::MAX_REQUEST_FRAME_BYTES;
    if declared > u64::from(limit) {
        Err(ProtocolError::RequestBudgetExceeded { declared, limit })
    } else {
        Ok(())
    }
}

/// Admission class declared before a resident transaction is accepted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CompileWorkload {
    #[default]
    Foreground,
    Preparation,
}

impl CompileWorkload {
    pub(crate) fn wire_tag(self) -> u8 {
        match self {
            Self::Foreground => 0,
            Self::Preparation => 1,
        }
    }
    pub(crate) fn from_wire(tag: u8) -> Result<Self, ProtocolError> {
        match tag {
            0 => Ok(Self::Foreground),
            1 => Ok(Self::Preparation),
            _ => Err(ProtocolError::InvalidExecutionGrant),
        }
    }
}

/// Positive compiler allowance issued by the process owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExecutionGrant {
    pub jobs: u32,
    pub capabilities: u32,
}

impl Default for ExecutionGrant {
    fn default() -> Self {
        Self {
            jobs: 1,
            capabilities: 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InspectionScope {
    Current,
    PublicModule(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InspectionNamespace {
    Any,
    Value,
    Type,
    Constructor,
    Field,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructuredInspection {
    pub scope: InspectionScope,
    pub namespace: InspectionNamespace,
    pub name: String,
    pub generation: u64,
    pub fingerprint: String,
}

/// Mirrors `tidepool_repr::execution_schema::SymbolIdentity` field-for-field.
/// This crate stays a dependency leaf for proc macros (see this crate's
/// `CLAUDE.md`), so the shape is duplicated here rather than the type reused;
/// keep the fields in lockstep with that authoritative definition by hand.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SymbolIdentity {
    pub unit: String,
    pub module: String,
    pub namespace: String,
    pub occurrence: String,
    pub record_parent: Option<String>,
}

#[derive(Clone, Debug)]
enum Field {
    Input(OsString),
    OutputDir(OsString),
    Target(OsString),
    Targets(Vec<String>),
    TargetModuleOnly,
    Include(OsString),
    Turn,
    TurnTemplate {
        kind: String,
        path: PathBuf,
    },
    TurnOut(OsString),
    TurnVerdict(OsString),
    Classify,
    ClassifyOut(OsString),
    Cell,
    CellPlan,
    CheckSource,
    CellTemplate(PathBuf),
    CellOut(OsString),
    BuildProductsDir(OsString),
    ModuleCandidates(PathBuf),
    CertifyHomeProducts,
    SessionArtifacts(PathBuf),
    DeclarationJoin(PathBuf),
    DeclarationJoinOut(PathBuf),
    SessionRoot(OsString),
    /// The session's incarnation identity (the Rust `SessionId`, rendered as
    /// decimal text) -- absent for a one-shot compile or an older caller.
    /// Lets the worker's `GutsMemo` retain a `Tidepool.Session.*` entry
    /// across transactions within one incarnation instead of evicting it
    /// unconditionally: see `bridge/haskell/CLAUDE.md`'s memo-eviction note.
    SessionIncarnation(OsString),
    InjectVal(OsString),
    BindGen(u64),
    HarnessProfile,
    InspectType(String),
    InspectInfo(String),
    InspectBrowse(String),
    InspectBrowseExpanded(String),
    /// `--inspect-browse`/`--inspect-browse-expanded` with an enumeration cap.
    InspectBrowseBounded(String, bool, u32),
    InspectScopeBrowse,
    InspectSearch(String),
    /// `--inspect-search` carrying a compiler-side match cap.
    InspectSearchBounded(String, u32),
    InspectStructuredInfo(StructuredInspection),
    InspectStructuredType(StructuredInspection),
    InspectOut(OsString),
    /// One source containing every indexed `InspectType` probe. Singleton
    /// input sources remain in `Input` fields for typed-error fallback.
    InspectTypeBatch(PathBuf),
    RetainedGeneration(SymbolIdentity, u64),
    /// A `--turn` request that also writes its target's prepared-STG program
    /// (`<target>.prepared.cbor`), compiled against the retained generations.
    ActivationPreview,
    /// Inspection source failures reject the whole request with structured
    /// diagnostics instead of becoming per-query `Rejected` results.
    InspectionStrict,
    PortableOutput,
    EnsureExactInputs(ExactInputsRequest),
    WorldPublicModule(WorldPublicModuleRequest),
    WorldCellPlan(WorldCellPlanRequest),
    WorldCellPrepare(WorldCellPrepareRequest),
}

/// A versioned, typed request for the Haskell compiler worker.
///
/// Fields retain their domain shape until encoding. The CLI rendering exists
/// only for cache keys and diagnostics; worker dispatch consumes
/// [`encode`](Self::encode), not reparsed command-line flags.
#[derive(Clone, Debug, Default)]
pub struct ExtractRequest {
    fields: Vec<Field>,
    workload: CompileWorkload,
    execution_grant: ExecutionGrant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestMode {
    Invalid,
    EnsureExactInputs,
    WorldPublicModule,
    WorldCellPlan,
    WorldCellPrepare,
    ActivationPreview,
    DeclarationInterface,
    CellPlan,
    CheckSource,
    CellProgram,
    Classify,
    Inspection,
    Turn,
    Source,
}

impl std::fmt::Display for RequestMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Invalid => "invalid",
            Self::EnsureExactInputs => "ensure_exact_inputs",
            Self::WorldPublicModule => "world_public_module",
            Self::WorldCellPlan => "world_cell_plan",
            Self::WorldCellPrepare => "world_cell_prepare",
            Self::ActivationPreview => "activation_preview",
            Self::DeclarationInterface => "declaration_interface",
            Self::CellPlan => "cell_plan",
            Self::CheckSource => "check_source",
            Self::CellProgram => "cell_program",
            Self::Classify => "classify",
            Self::Inspection => "inspection",
            Self::Turn => "turn",
            Self::Source => "source",
        })
    }
}

impl ExtractRequest {
    pub fn set_workload(&mut self, workload: CompileWorkload) {
        self.workload = workload;
    }
    pub fn workload(&self) -> CompileWorkload {
        self.workload
    }
    #[cfg(test)]
    pub(crate) fn execution_grant(&self) -> ExecutionGrant {
        self.execution_grant
    }
    pub(crate) fn set_execution_grant(&mut self, grant: ExecutionGrant) {
        self.execution_grant = grant;
    }

    pub(crate) fn is_exact_inputs(&self) -> bool {
        self.fields
            .iter()
            .any(|field| matches!(field, Field::EnsureExactInputs(_)))
    }

    /// Validate only machine framing and the echoed transport reference. Root
    /// authentication and realization issuance belong to the semantic owners.
    pub(crate) fn validate_exact_inputs_response(
        &self,
        output: &std::process::Output,
    ) -> Result<(), ProtocolError> {
        if !self.is_exact_inputs() || !output.status.success() {
            return Ok(());
        }
        let [Field::EnsureExactInputs(request)] = self.fields.as_slice() else {
            return Err(ProtocolError::InvalidExactInputs);
        };
        match EnsureExactInputsOutcome::decode(&output.stdout)? {
            EnsureExactInputsOutcome::NeedsAcquisition(root) if root != request.root => {
                Err(ProtocolError::InvalidExactInputs)
            }
            EnsureExactInputsOutcome::Ready { world, .. } if world != request.root => {
                Err(ProtocolError::InvalidExactInputs)
            }
            _ => Ok(()),
        }
    }

    /// Diagnostic mode comes from the same admission used by the CLI.
    pub(crate) fn mode(&self) -> RequestMode {
        self.admitted_mode().unwrap_or(RequestMode::Invalid)
    }

    fn admitted_mode(&self) -> Result<RequestMode, CliError> {
        if self
            .fields
            .iter()
            .any(|field| matches!(field, Field::EnsureExactInputs(_)))
        {
            return match self.fields.as_slice() {
                [Field::EnsureExactInputs(request)] if request.valid() => {
                    Ok(RequestMode::EnsureExactInputs)
                }
                _ => Err(CliError::new(
                    "exact input acquisition must be source-free and exclusive",
                )),
            };
        }
        if self
            .fields
            .iter()
            .any(|field| matches!(field, Field::WorldPublicModule(_)))
        {
            return match self.fields.as_slice() {
                [Field::WorldPublicModule(request)] if request.valid() => {
                    Ok(RequestMode::WorldPublicModule)
                }
                _ => Err(CliError::new(
                    "world public module query must be source-free, public and exclusive",
                )),
            };
        }
        if self
            .fields
            .iter()
            .any(|field| matches!(field, Field::WorldCellPlan(_) | Field::WorldCellPrepare(_)))
        {
            return match self.fields.as_slice() {
                [Field::WorldCellPlan(request)] if request.valid() => {
                    Ok(RequestMode::WorldCellPlan)
                }
                [Field::WorldCellPrepare(request)] if request.valid() => {
                    Ok(RequestMode::WorldCellPrepare)
                }
                _ => Err(CliError::new(
                    "held world cell request must be bounded and exclusive",
                )),
            };
        }
        let has = |predicate: fn(&Field) -> bool| self.fields.iter().any(predicate);
        let join = has(|field| matches!(field, Field::DeclarationJoin(_)));
        let join_out = has(|field| matches!(field, Field::DeclarationJoinOut(_)));
        if join != join_out {
            return Err(CliError::new(
                "declaration operation requires both --declaration-join and --declaration-join-out",
            ));
        }
        let turn = has(|field| matches!(field, Field::Turn));
        let preview = has(|field| matches!(field, Field::ActivationPreview));
        if preview && !turn {
            return Err(CliError::new("activation preview requires turn mode"));
        }
        let selections = [
            (join, RequestMode::DeclarationInterface),
            (
                turn,
                if preview {
                    RequestMode::ActivationPreview
                } else {
                    RequestMode::Turn
                },
            ),
            (
                has(|field| matches!(field, Field::CheckSource)),
                RequestMode::CheckSource,
            ),
            (
                has(|field| matches!(field, Field::CellPlan)),
                RequestMode::CellPlan,
            ),
            (
                has(|field| matches!(field, Field::Cell)),
                RequestMode::CellProgram,
            ),
            (
                has(|field| matches!(field, Field::Classify)),
                RequestMode::Classify,
            ),
            (
                has(|field| {
                    matches!(
                        field,
                        Field::InspectType(_)
                            | Field::InspectInfo(_)
                            | Field::InspectBrowse(_)
                            | Field::InspectBrowseExpanded(_)
                            | Field::InspectScopeBrowse
                            | Field::InspectBrowseBounded(..)
                            | Field::InspectSearch(_)
                            | Field::InspectSearchBounded(..)
                            | Field::InspectStructuredInfo(_)
                            | Field::InspectStructuredType(_)
                    )
                }),
                RequestMode::Inspection,
            ),
        ];
        let mut selected = selections
            .into_iter()
            .filter_map(|(present, mode)| present.then_some(mode));
        let mode = selected.next().unwrap_or(RequestMode::Source);
        if selected.next().is_some()
            || (has(|field| matches!(field, Field::CertifyHomeProducts))
                && mode != RequestMode::Source)
        {
            return Err(CliError::new(
                "compiler request selects conflicting operations",
            ));
        }
        if has(|field| matches!(field, Field::PortableOutput))
            && (mode != RequestMode::Source
                || self
                    .fields
                    .iter()
                    .filter(|field| matches!(field, Field::Input(_)))
                    .count()
                    != 1
                || !has(|field| matches!(field, Field::OutputDir(_)))
                || {
                    let targets = self.target_names();
                    targets.is_empty() || targets.iter().any(String::is_empty)
                }
                || has(|field| {
                    matches!(
                        field,
                        Field::SessionRoot(_)
                            | Field::SessionIncarnation(_)
                            | Field::SessionArtifacts(_)
                            | Field::InjectVal(_)
                            | Field::RetainedGeneration(_, _)
                            | Field::BindGen(_)
                            | Field::ModuleCandidates(_)
                            | Field::CertifyHomeProducts
                            | Field::TargetModuleOnly
                    )
                }))
        {
            return Err(CliError::new(
                "portable output requires fresh source targets without retained authority",
            ));
        }
        Ok(mode)
    }

    /// Parse the human CLI into the same typed request used by
    /// library callers. Unknown options and missing/invalid values are usage
    /// errors; they are never reinterpreted as source paths.
    pub fn from_cli(args: &[OsString]) -> Result<Self, CliError> {
        let mut request = Self::default();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--workload") => {
                    request.set_workload(match text_value(&mut args, "--workload")? {
                        "foreground" => CompileWorkload::Foreground,
                        "preparation" => CompileWorkload::Preparation,
                        _ => {
                            return Err(CliError::new(
                                "--workload requires foreground or preparation",
                            ))
                        }
                    });
                }
                Some("--output-dir") => request.output_dir(value(&mut args, "--output-dir")?),
                Some("--target") => request.target(value(&mut args, "--target")?),
                Some("--targets") => {
                    let raw = text_value(&mut args, "--targets")?;
                    request
                        .fields
                        .push(Field::Targets(raw.split(',').map(str::to_owned).collect()));
                }
                Some("--portable-output") => request.portable_output(),
                Some("--target-module-only") => request.fields.push(Field::TargetModuleOnly),
                Some("--include") => request.include(value(&mut args, "--include")?),
                Some("--turn") => request.turn(),
                Some("--turn-template") => {
                    let raw = text_value(&mut args, "--turn-template")?;
                    let Some((kind, path)) = raw.split_once('=') else {
                        return Err(CliError::new("--turn-template requires KIND=PATH"));
                    };
                    request.turn_template(kind, Path::new(path));
                }
                Some("--turn-out") => request.turn_out(value(&mut args, "--turn-out")?),
                Some("--turn-verdict") => request.turn_verdict(value(&mut args, "--turn-verdict")?),
                Some("--classify") => request.classify(),
                Some("--classify-out") => request.classify_out(value(&mut args, "--classify-out")?),
                Some("--cell") => request.cell(),
                Some("--cell-plan") => request.cell_plan(),
                Some("--check-source") => request.check_source(),
                Some("--cell-template") => {
                    request.cell_template(Path::new(value(&mut args, "--cell-template")?))
                }
                Some("--cell-out") => request.cell_out(value(&mut args, "--cell-out")?),
                Some(option @ ("--cell-fold-turn" | "--turn-pin")) => {
                    return Err(CliError::new(format!("retired compiler option: {option}")));
                }
                Some("--build-products-dir") => {
                    request.build_products_dir(value(&mut args, "--build-products-dir")?)
                }
                Some("--module-candidates") => {
                    request.module_candidates(Path::new(value(&mut args, "--module-candidates")?))
                }
                Some("--certify-home-products") => request.certify_home_products(),
                Some("--session-artifacts") => {
                    request.session_artifacts(Path::new(value(&mut args, "--session-artifacts")?))
                }
                Some("--declaration-join") => {
                    request.declaration_join(Path::new(value(&mut args, "--declaration-join")?))
                }
                Some("--declaration-join-out") => request
                    .declaration_join_out(Path::new(value(&mut args, "--declaration-join-out")?)),
                Some("--session-root") => request.session_root(value(&mut args, "--session-root")?),
                Some("--session-incarnation") => {
                    request.session_incarnation(value(&mut args, "--session-incarnation")?)
                }
                Some("--inject-val") => request.inject_val(value(&mut args, "--inject-val")?),
                Some("--bind-gen") => {
                    let raw = text_value(&mut args, "--bind-gen")?;
                    let generation = raw
                        .parse()
                        .map_err(|_| CliError::new("--bind-gen requires an unsigned integer"))?;
                    request.bind_gen(generation);
                }
                Some("--harness-profile") => request.fields.push(Field::HarnessProfile),
                Some("--inspect-type") => request.fields.push(Field::InspectType(
                    text_value(&mut args, "--inspect-type")?.into(),
                )),
                Some("--inspect-info") => request.fields.push(Field::InspectInfo(
                    text_value(&mut args, "--inspect-info")?.into(),
                )),
                Some("--inspect-scope-browse") => request.fields.push(Field::InspectScopeBrowse),
                Some("--inspect-browse") => request.fields.push(Field::InspectBrowse(
                    text_value(&mut args, "--inspect-browse")?.into(),
                )),
                Some("--inspect-browse-expanded") => {
                    request.fields.push(Field::InspectBrowseExpanded(
                        text_value(&mut args, "--inspect-browse-expanded")?.into(),
                    ))
                }
                Some("--inspect-search") => request.fields.push(Field::InspectSearch(
                    text_value(&mut args, "--inspect-search")?.into(),
                )),
                Some(
                    option @ ("--inspect-browse-bounded" | "--inspect-browse-expanded-bounded"),
                ) => {
                    let raw = text_value(&mut args, option)?;
                    let (module, budget) = bounded_value(&raw, option)?;
                    request.fields.push(Field::InspectBrowseBounded(
                        module,
                        option == "--inspect-browse-expanded-bounded",
                        budget,
                    ));
                }
                Some("--inspect-search-bounded") => {
                    let raw = text_value(&mut args, "--inspect-search-bounded")?;
                    let (query, budget) = bounded_value(&raw, "--inspect-search-bounded")?;
                    request
                        .fields
                        .push(Field::InspectSearchBounded(query, budget));
                }
                Some("--inspect-out") => request.inspect_out(value(&mut args, "--inspect-out")?),
                Some(option) if option.starts_with('-') => {
                    return Err(CliError::new(format!("unknown option: {option}")));
                }
                _ => request.input(arg),
            }
        }
        let has_input = request
            .fields
            .iter()
            .any(|field| matches!(field, Field::Input(_)));
        if !has_input {
            return Err(CliError::new("an input file is required"));
        }
        request.admitted_mode()?;
        Ok(request)
    }

    /// Decode the versioned worker payload emitted by [`Self::encode`].
    ///
    /// This is the Rust-side protocol boundary for test workers and tooling
    /// that need to inspect a request rather than forward it opaquely. Invalid
    /// headers, truncated fields, unknown tags, and trailing bytes are errors.
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        let mut decoder = Decoder::new(bytes)?;
        let workload = CompileWorkload::from_wire(decoder.byte()?)?;
        let execution_grant = ExecutionGrant {
            jobs: decoder.u32()?,
            capabilities: decoder.u32()?,
        };
        if execution_grant.jobs == 0 || execution_grant.capabilities == 0 {
            return Err(ProtocolError::InvalidExecutionGrant);
        }
        let field_count = decoder.count(Decoder::MIN_FIELD_BYTES)?;
        // A field can be only its one-byte tag, while `Field` is much larger.
        // Do not amplify an untrusted count into a count-sized allocation
        // before the tags themselves have been validated.
        let mut fields = Vec::new();
        for _ in 0..field_count {
            let tag = decoder.byte()?;
            let field = match tag {
                1 => Field::Input(decoder.os_string()?),
                2 => Field::OutputDir(decoder.os_string()?),
                3 => Field::Target(decoder.os_string()?),
                4 => {
                    let count = decoder.count(4)?;
                    let mut values = Vec::with_capacity(count);
                    for _ in 0..count {
                        values.push(decoder.string()?);
                    }
                    Field::Targets(values)
                }
                5 => return Err(ProtocolError::RetiredFieldTag(5)),
                6 | 39 => return Err(ProtocolError::RetiredFieldTag(tag)),
                7 => Field::TargetModuleOnly,
                8 => Field::Include(decoder.os_string()?),
                9 | 10 | 14 => return Err(ProtocolError::RetiredFieldTag(tag)),
                11 => Field::BindGen(decoder.u64()?),
                12 => Field::SessionRoot(decoder.os_string()?),
                44 => Field::SessionIncarnation(decoder.os_string()?),
                13 => Field::InjectVal(decoder.os_string()?),
                16 => Field::Turn,
                17 => Field::TurnTemplate {
                    kind: decoder.string()?,
                    path: PathBuf::from(decoder.os_string()?),
                },
                18 => Field::TurnOut(decoder.os_string()?),
                19 => Field::TurnVerdict(decoder.os_string()?),
                20 => Field::Classify,
                21 => Field::ClassifyOut(decoder.os_string()?),
                24 => Field::HarnessProfile,
                25 => Field::BuildProductsDir(decoder.os_string()?),
                46 => Field::ModuleCandidates(PathBuf::from(decoder.os_string()?)),
                50 => Field::CertifyHomeProducts,
                49 => Field::SessionArtifacts(PathBuf::from(decoder.os_string()?)),
                47 => Field::DeclarationJoin(PathBuf::from(decoder.os_string()?)),
                48 => Field::DeclarationJoinOut(PathBuf::from(decoder.os_string()?)),
                26 => Field::InspectType(decoder.string()?),
                27 => Field::InspectInfo(decoder.string()?),
                28 => Field::InspectOut(decoder.os_string()?),
                29 => Field::InspectBrowse(decoder.string()?),
                30 => Field::InspectBrowseExpanded(decoder.string()?),
                43 => Field::InspectScopeBrowse,
                62 => {
                    let module = decoder.string()?;
                    let expanded = match decoder.byte()? {
                        0 => false,
                        1 => true,
                        other => return Err(ProtocolError::InvalidBrowseExpansion(other)),
                    };
                    Field::InspectBrowseBounded(module, expanded, nonzero_budget(decoder.u32()?)?)
                }
                63 => {
                    let query = decoder.string()?;
                    Field::InspectSearchBounded(query, nonzero_budget(decoder.u32()?)?)
                }
                31 => Field::Cell,
                51 => Field::CellPlan,
                52 => Field::CheckSource,
                32 => Field::CellTemplate(PathBuf::from(decoder.os_string()?)),
                33 => Field::CellOut(decoder.os_string()?),
                34 => return Err(ProtocolError::RetiredFieldTag(34)),
                35 => Field::InspectSearch(decoder.string()?),
                36 => Field::InspectStructuredInfo(decoder.structured_inspection()?),
                37 => Field::InspectStructuredType(decoder.structured_inspection()?),
                38 => {
                    let identity = decoder.symbol_identity()?;
                    Field::RetainedGeneration(identity, decoder.u64()?)
                }
                40 => Field::InspectTypeBatch(PathBuf::from(decoder.os_string()?)),
                41 => Field::ActivationPreview,
                42 => Field::InspectionStrict,
                53 => Field::PortableOutput,
                57 => {
                    let root = decoder.string()?;
                    let acquisition = match decoder.byte()? {
                        0 => ExactInputsAcquisition::Resident(decoder.string()?),
                        1 => ExactInputsAcquisition::Complete(PathBuf::from(decoder.os_string()?)),
                        2 => ExactInputsAcquisition::Successor { predecessor: decoder.string()?, realization: decoder.string()?, descriptor: PathBuf::from(decoder.os_string()?) },
                        _ => return Err(ProtocolError::InvalidExactInputs),
                    };
                    Field::EnsureExactInputs(ExactInputsRequest { root, acquisition })
                }
                58 => {
                    let world = decoder.string()?;
                    let realization = decoder.string()?;
                    let mode = match decoder.byte()? {
                        0 => WorldQueryMode::Info,
                        1 => WorldQueryMode::Type,
                        _ => return Err(ProtocolError::InvalidWorldPublicModule),
                    };
                    let query = decoder.structured_inspection()?;
                    let output = PathBuf::from(decoder.os_string()?);
                    Field::WorldPublicModule(WorldPublicModuleRequest {
                        world,
                        realization,
                        mode,
                        query,
                        output,
                    })
                }
                59 => {
                    check_world_cell_bytes(bytes.len())?;
                    let world = decoder.cell_string(64)?;
                    let realization = decoder.string()?;
                    let recipe = Arc::new(decoder.world_cell_recipe()?);
                    let output = PathBuf::from(decoder.cell_string(65536)?);
                    Field::WorldCellPlan(WorldCellPlanRequest {
                        world,
                        realization,
                        recipe,
                        output,
                    })
                }
                60 => {
                    check_world_cell_bytes(bytes.len())?;
                    let world = decoder.cell_string(64)?;
                    let realization = decoder.string()?;
                    let recipe = Arc::new(decoder.world_cell_recipe()?);
                    let plan = decoder.cell_frame(8 << 20)?.to_vec();
                    let admission = decoder.fixed()?;
                    let count = decoder.count(17)?;
                    if count > 10000 {
                        return Err(ProtocolError::InvalidWorldCell);
                    }
                    let mut slots = Vec::with_capacity(count);
                    for _ in 0..count {
                        let ordinal = decoder.u64()?;
                        let tag = decoder.byte()?;
                        let generation = decoder.u64()?;
                        let slot = match tag {
                            0 => PlannedCellSlot::Prologue(generation),
                            1 => PlannedCellSlot::Declaration(generation),
                            2 => PlannedCellSlot::Bind(generation),
                            3 => PlannedCellSlot::Expression {
                                generation,
                                observation: decoder.cell_string(65536)?,
                            },
                            _ => return Err(ProtocolError::InvalidWorldCell),
                        };
                        slots.push((ordinal, slot));
                    }
                    let directory = PathBuf::from(decoder.cell_string(65536)?);
                    let output = PathBuf::from(decoder.cell_string(65536)?);
                    Field::WorldCellPrepare(WorldCellPrepareRequest {
                        world,
                        realization,
                        recipe,
                        plan: plan.into(),
                        admission,
                        slots,
                        directory,
                        output,
                    })
                }
                45 => return Err(ProtocolError::RetiredFieldTag(45)),
                other => return Err(ProtocolError::UnknownFieldTag(other)),
            };
            fields.push(field);
        }
        decoder.finish()?;
        let request = Self {
            fields,
            workload,
            execution_grant,
        };
        if request
            .fields
            .iter()
            .any(|field| matches!(field, Field::EnsureExactInputs(_)))
        {
            request
                .admitted_mode()
                .map_err(|_| ProtocolError::InvalidExactInputs)?;
        }
        if request
            .fields
            .iter()
            .any(|field| matches!(field, Field::WorldPublicModule(_)))
        {
            request
                .admitted_mode()
                .map_err(|_| ProtocolError::InvalidWorldPublicModule)?;
        }
        if request
            .fields
            .iter()
            .any(|field| matches!(field, Field::WorldCellPlan(_) | Field::WorldCellPrepare(_)))
        {
            request
                .admitted_mode()
                .map_err(|_| ProtocolError::InvalidWorldCell)?;
        }
        Ok(request)
    }

    /// Decode the complete argv accepted by the compiler worker.
    ///
    /// This is the inspection counterpart to the endpoint's private argv
    /// renderer. Wrappers and tests can assert on typed request fields without
    /// depending on the payload's hexadecimal transport representation.
    pub fn decode_worker_argv(args: &[OsString]) -> Result<Self, ProtocolError> {
        if args.len() != 2 || args[0] != WORKER_REQUEST_FLAG {
            return Err(ProtocolError::InvalidWorkerArgv);
        }
        let payload = args[1].to_str().ok_or(ProtocolError::NonUtf8Payload)?;
        Self::decode(&unhex(payload)?)
    }

    /// Exact source search order consumed by the worker.
    pub fn include_paths(&self) -> Vec<&Path> {
        self.fields
            .iter()
            .filter_map(|field| match field {
                Field::Include(path) => Some(Path::new(path)),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn selected_session_values(&self) -> impl Iterator<Item = &OsStr> {
        self.fields.iter().filter_map(|field| match field {
            Field::InjectVal(value) => Some(value.as_os_str()),
            _ => None,
        })
    }

    pub fn is_turn(&self) -> bool {
        self.fields.iter().any(|field| matches!(field, Field::Turn))
    }

    /// Input continuity currently covers plain turns without mutable compiler
    /// context or specialized inspection/projection modes.
    pub fn supports_compile_input_identity(&self) -> bool {
        self.is_turn()
            && self.fields.iter().all(|field| {
                matches!(
                    field,
                    Field::Input(_)
                        | Field::OutputDir(_)
                        | Field::Target(_)
                        | Field::Targets(_)
                        | Field::Include(_)
                        | Field::Turn
                        | Field::TurnTemplate { .. }
                        | Field::TurnOut(_)
                        | Field::TurnVerdict(_)
                        | Field::BuildProductsDir(_)
                        | Field::ModuleCandidates(_)
                        | Field::CertifyHomeProducts
                        | Field::SessionRoot(_)
                        | Field::BindGen(_)
                )
            })
    }

    pub(crate) fn relocate_turn_outputs(&mut self, root: &Path) {
        self.fields
            .retain(|field| !matches!(field, Field::OutputDir(_) | Field::TurnOut(_)));
        self.output_dir(root);
        self.turn_out(root.join("turn.cbor"));
    }

    /// Requested output directory, if this request carries one.
    pub fn output_directory(&self) -> Option<&OsStr> {
        self.fields.iter().find_map(|field| match field {
            Field::OutputDir(value) => Some(value.as_os_str()),
            _ => None,
        })
    }

    /// Logical target names in request order.
    pub fn target_names(&self) -> Vec<String> {
        self.fields
            .iter()
            .flat_map(|field| match field {
                Field::Target(value) => vec![value.to_string_lossy().into_owned()],
                Field::Targets(values) => values.clone(),
                _ => Vec::new(),
            })
            .collect()
    }

    /// Executable imports this request has retained, keyed by identity in
    /// request order (a later entry for the same identity wins, matching the
    /// Haskell decoder's `Map.insert` fold).
    pub fn retained_generations(&self) -> BTreeMap<SymbolIdentity, u64> {
        self.fields
            .iter()
            .filter_map(|field| match field {
                Field::RetainedGeneration(identity, generation) => {
                    Some((identity.clone(), *generation))
                }
                _ => None,
            })
            .collect()
    }

    /// Whether this turn request also asks for its prepared-STG program.
    pub(crate) fn input(&mut self, value: impl AsRef<OsStr>) {
        self.fields.push(Field::Input(value.as_ref().to_owned()));
    }

    pub(crate) fn output_dir(&mut self, value: impl AsRef<OsStr>) {
        self.fields
            .push(Field::OutputDir(value.as_ref().to_owned()));
    }

    pub(crate) fn target(&mut self, value: impl AsRef<OsStr>) {
        self.fields.push(Field::Target(value.as_ref().to_owned()));
    }

    pub(crate) fn targets<I, S>(&mut self, values: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.fields.push(Field::Targets(
            values
                .into_iter()
                .map(|value| value.as_ref().to_owned())
                .collect(),
        ));
    }

    pub(crate) fn include(&mut self, value: impl AsRef<OsStr>) {
        self.fields.push(Field::Include(value.as_ref().to_owned()));
    }

    pub(crate) fn turn(&mut self) {
        self.fields.push(Field::Turn);
    }

    pub(crate) fn turn_template(&mut self, kind: &str, path: &Path) {
        self.fields.push(Field::TurnTemplate {
            kind: kind.to_owned(),
            path: path.to_owned(),
        });
    }

    pub(crate) fn turn_out(&mut self, value: impl AsRef<OsStr>) {
        self.fields.push(Field::TurnOut(value.as_ref().to_owned()));
    }

    pub(crate) fn turn_verdict(&mut self, value: impl AsRef<OsStr>) {
        self.fields
            .push(Field::TurnVerdict(value.as_ref().to_owned()));
    }

    pub(crate) fn classify(&mut self) {
        self.fields.push(Field::Classify);
    }

    pub(crate) fn classify_out(&mut self, value: impl AsRef<OsStr>) {
        self.fields
            .push(Field::ClassifyOut(value.as_ref().to_owned()));
    }

    pub(crate) fn cell(&mut self) {
        self.fields.push(Field::Cell);
    }

    pub(crate) fn check_source(&mut self) {
        self.fields.push(Field::CheckSource);
    }

    pub fn is_check_source(&self) -> bool {
        self.fields
            .iter()
            .any(|field| matches!(field, Field::CheckSource))
    }

    pub(crate) fn cell_plan(&mut self) {
        self.fields.push(Field::CellPlan);
    }

    pub(crate) fn cell_template(&mut self, value: &Path) {
        self.fields.push(Field::CellTemplate(value.to_owned()));
    }

    pub(crate) fn cell_out(&mut self, value: impl AsRef<OsStr>) {
        self.fields.push(Field::CellOut(value.as_ref().to_owned()));
    }

    pub(crate) fn build_products_dir(&mut self, value: impl AsRef<OsStr>) {
        self.fields
            .push(Field::BuildProductsDir(value.as_ref().to_owned()));
    }

    /// Transport-owned placement of mutable GHC outputs. The caller's logical
    /// request remains the recipe and diagnostic identity; this clone is sent
    /// only to the worker that exclusively owns the namespace.
    pub(crate) fn place_build_products(&mut self, namespace: &Path) -> Vec<(PathBuf, PathBuf)> {
        let mut placements = Vec::new();
        for field in &mut self.fields {
            if let Field::BuildProductsDir(root) = field {
                let physical = Path::new(root).join(namespace);
                tracing::info!(
                    logical_build_products_root = %Path::new(root).display(),
                    physical_build_products_dir = %physical.display(),
                    "compiler build products placed"
                );
                placements.push((PathBuf::from(&*root), physical.clone()));
                *root = physical.into_os_string();
            }
        }
        placements
    }

    pub(crate) fn module_candidates(&mut self, value: &Path) {
        self.fields.push(Field::ModuleCandidates(value.to_owned()));
    }

    pub(crate) fn certify_home_products(&mut self) {
        self.fields.push(Field::CertifyHomeProducts);
    }

    pub(crate) fn session_artifacts(&mut self, value: &Path) {
        self.fields.push(Field::SessionArtifacts(value.to_owned()));
    }

    pub(crate) fn declaration_join(&mut self, value: &Path) {
        self.fields.push(Field::DeclarationJoin(value.to_owned()));
    }

    pub(crate) fn declaration_join_out(&mut self, value: &Path) {
        self.fields
            .push(Field::DeclarationJoinOut(value.to_owned()));
    }

    pub(crate) fn session_root(&mut self, value: impl AsRef<OsStr>) {
        self.fields
            .push(Field::SessionRoot(value.as_ref().to_owned()));
    }

    pub(crate) fn session_incarnation(&mut self, value: impl AsRef<OsStr>) {
        self.fields
            .push(Field::SessionIncarnation(value.as_ref().to_owned()));
    }

    pub(crate) fn inject_val(&mut self, value: impl AsRef<OsStr>) {
        self.fields
            .push(Field::InjectVal(value.as_ref().to_owned()));
    }

    pub(crate) fn bind_gen(&mut self, value: u64) {
        self.fields.push(Field::BindGen(value));
    }

    pub(crate) fn retained_generation(&mut self, identity: SymbolIdentity, generation: u64) {
        self.fields
            .push(Field::RetainedGeneration(identity, generation));
    }

    pub(crate) fn activation_preview(&mut self) {
        self.fields.push(Field::ActivationPreview);
    }

    pub(crate) fn inspect_type(&mut self, expression: &str) {
        self.fields.push(Field::InspectType(expression.to_owned()));
    }

    pub(crate) fn inspect_info(&mut self, name: &str) {
        self.fields.push(Field::InspectInfo(name.to_owned()));
    }

    pub(crate) fn inspect_scope_browse(&mut self) {
        self.fields.push(Field::InspectScopeBrowse);
    }

    pub(crate) fn inspect_browse(&mut self, module: &str, expanded: bool) {
        self.fields.push(if expanded {
            Field::InspectBrowseExpanded(module.to_owned())
        } else {
            Field::InspectBrowse(module.to_owned())
        });
    }

    pub(crate) fn inspect_browse_bounded(&mut self, module: &str, expanded: bool, budget: u32) {
        self.fields.push(Field::InspectBrowseBounded(
            module.to_owned(),
            expanded,
            budget,
        ));
    }

    pub(crate) fn inspect_search(&mut self, query: &str) {
        self.fields.push(Field::InspectSearch(query.to_owned()));
    }

    pub(crate) fn inspect_search_bounded(&mut self, query: &str, budget: u32) {
        self.fields
            .push(Field::InspectSearchBounded(query.to_owned(), budget));
    }

    pub fn ensure_exact_inputs(&mut self, request: ExactInputsRequest) -> &mut Self {
        self.fields.push(Field::EnsureExactInputs(request));
        self
    }

    pub fn world_cell_plan(
        &mut self,
        request: WorldCellPlanRequest,
    ) -> Result<&mut Self, ProtocolError> {
        if !self.fields.is_empty() {
            return Err(ProtocolError::InvalidWorldCell);
        }
        request.validate_transport()?;
        self.fields.push(Field::WorldCellPlan(request));
        Ok(self)
    }

    pub fn world_cell_prepare(
        &mut self,
        request: WorldCellPrepareRequest,
    ) -> Result<&mut Self, ProtocolError> {
        if !self.fields.is_empty() {
            return Err(ProtocolError::InvalidWorldCell);
        }
        request.validate_transport()?;
        self.fields.push(Field::WorldCellPrepare(request));
        Ok(self)
    }

    pub fn world_public_module(&mut self, request: WorldPublicModuleRequest) -> &mut Self {
        self.fields.push(Field::WorldPublicModule(request));
        self
    }

    pub(crate) fn inspect_structured_info(&mut self, query: StructuredInspection) {
        self.fields.push(Field::InspectStructuredInfo(query));
    }

    pub(crate) fn inspect_structured_type(&mut self, query: StructuredInspection) {
        self.fields.push(Field::InspectStructuredType(query));
    }

    pub(crate) fn inspect_out(&mut self, value: impl AsRef<OsStr>) {
        self.fields
            .push(Field::InspectOut(value.as_ref().to_owned()));
    }

    pub(crate) fn inspect_type_batch(&mut self, value: &Path) {
        self.fields.push(Field::InspectTypeBatch(value.to_owned()));
    }

    pub(crate) fn portable_output(&mut self) {
        self.fields.push(Field::PortableOutput);
    }

    pub(crate) fn inspection_strict(&mut self) {
        self.fields.push(Field::InspectionStrict);
    }

    pub(crate) fn cli_argv(&self) -> Vec<OsString> {
        let mut inputs = Vec::new();
        let mut flags = Vec::new();
        for field in &self.fields {
            match field {
                Field::Input(value) => inputs.push(value.clone()),
                Field::OutputDir(value) => flag(&mut flags, "--output-dir", value),
                Field::Target(value) => flag(&mut flags, "--target", value),
                Field::Targets(values) => {
                    flag(&mut flags, "--targets", OsStr::new(&values.join(",")))
                }
                Field::TargetModuleOnly => flags.push("--target-module-only".into()),
                Field::Include(value) => flag(&mut flags, "--include", value),
                Field::Turn => flags.push("--turn".into()),
                Field::TurnTemplate { kind, path } => flag(
                    &mut flags,
                    "--turn-template",
                    OsStr::new(&format!("{kind}={}", path.display())),
                ),
                Field::TurnOut(value) => flag(&mut flags, "--turn-out", value),
                Field::TurnVerdict(value) => flag(&mut flags, "--turn-verdict", value),
                Field::Classify => flags.push("--classify".into()),
                Field::ClassifyOut(value) => flag(&mut flags, "--classify-out", value),
                Field::Cell => flags.push("--cell".into()),
                Field::CellPlan => flags.push("--cell-plan".into()),
                Field::CheckSource => flags.push("--check-source".into()),
                Field::CellTemplate(value) => {
                    flag(&mut flags, "--cell-template", value.as_os_str())
                }
                Field::CellOut(value) => flag(&mut flags, "--cell-out", value),
                Field::BuildProductsDir(value) => flag(&mut flags, "--build-products-dir", value),
                Field::ModuleCandidates(value) => {
                    flag(&mut flags, "--module-candidates", value.as_os_str())
                }
                Field::CertifyHomeProducts => flags.push("--certify-home-products".into()),
                Field::SessionArtifacts(value) => {
                    flag(&mut flags, "--session-artifacts", value.as_os_str())
                }
                Field::DeclarationJoin(value) => {
                    flag(&mut flags, "--declaration-join", value.as_os_str())
                }
                Field::DeclarationJoinOut(value) => {
                    flag(&mut flags, "--declaration-join-out", value.as_os_str())
                }
                Field::SessionRoot(value) => flag(&mut flags, "--session-root", value),
                Field::SessionIncarnation(value) => {
                    flag(&mut flags, "--session-incarnation", value)
                }
                Field::InjectVal(value) => flag(&mut flags, "--inject-val", value),
                Field::BindGen(value) => {
                    flag(&mut flags, "--bind-gen", OsStr::new(&value.to_string()))
                }
                Field::HarnessProfile => flags.push("--harness-profile".into()),
                Field::InspectType(value) => flag(&mut flags, "--inspect-type", OsStr::new(value)),
                Field::InspectInfo(value) => flag(&mut flags, "--inspect-info", OsStr::new(value)),
                Field::InspectBrowse(value) => {
                    flag(&mut flags, "--inspect-browse", OsStr::new(value))
                }
                Field::InspectBrowseExpanded(value) => {
                    flag(&mut flags, "--inspect-browse-expanded", OsStr::new(value))
                }
                Field::InspectScopeBrowse => flags.push("--inspect-scope-browse".into()),
                Field::InspectBrowseBounded(value, expanded, budget) => flag(
                    &mut flags,
                    if *expanded {
                        "--inspect-browse-expanded-bounded"
                    } else {
                        "--inspect-browse-bounded"
                    },
                    OsStr::new(&format!("{value}={budget}")),
                ),
                Field::InspectSearch(value) => {
                    flag(&mut flags, "--inspect-search", OsStr::new(value))
                }
                Field::InspectSearchBounded(value, budget) => flag(
                    &mut flags,
                    "--inspect-search-bounded",
                    OsStr::new(&format!("{value}={budget}")),
                ),
                Field::InspectStructuredInfo(query) => {
                    structured_flag(&mut flags, "--inspect-structured-info", query)
                }
                Field::InspectStructuredType(query) => {
                    structured_flag(&mut flags, "--inspect-structured-type", query)
                }
                Field::InspectOut(value) => flag(&mut flags, "--inspect-out", value),
                Field::InspectTypeBatch(value) => {
                    flag(&mut flags, "--inspect-type-batch", value.as_os_str())
                }
                Field::RetainedGeneration(identity, generation) => flag(
                    &mut flags,
                    "--retained-generation",
                    OsStr::new(&format!(
                        "{}:{}:{}:{}:{}={generation}",
                        identity.unit,
                        identity.module,
                        identity.namespace,
                        identity.occurrence,
                        identity.record_parent.as_deref().unwrap_or(""),
                    )),
                ),
                Field::ActivationPreview => flags.push("--activation-preview".into()),
                Field::InspectionStrict => flags.push("--inspection-strict".into()),
                Field::PortableOutput => flags.push("--portable-output".into()),
                Field::WorldCellPlan(request) => {
                    flag(&mut flags, "<world-cell-plan>", OsStr::new(&request.world))
                }
                Field::WorldCellPrepare(request) => flag(
                    &mut flags,
                    "<world-cell-prepare>",
                    OsStr::new(&request.world),
                ),
                Field::WorldPublicModule(request) => flag(
                    &mut flags,
                    "<world-public-module>",
                    OsStr::new(&format!("{request:?}")),
                ),
                Field::EnsureExactInputs(request) => flag(
                    &mut flags,
                    "<ensure-exact-inputs>",
                    OsStr::new(&format!("{request:?}")),
                ),
            }
        }
        inputs.extend(flags);
        inputs
    }

    pub fn encode(&self) -> Vec<u8> {
        self.encode_with_grant(self.execution_grant)
    }

    fn encode_with_grant(&self, grant: ExecutionGrant) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.push(self.workload.wire_tag());
        out.extend_from_slice(&grant.jobs.to_le_bytes());
        out.extend_from_slice(&grant.capabilities.to_le_bytes());
        push_u32(&mut out, self.fields.len());
        for field in &self.fields {
            encode_field(&mut out, field);
        }
        out
    }

    pub(crate) fn worker_argv(&self) -> Vec<OsString> {
        self.worker_argv_with_grant(self.execution_grant)
    }

    pub(crate) fn worker_argv_with_grant(&self, grant: ExecutionGrant) -> Vec<OsString> {
        vec![
            WORKER_REQUEST_FLAG.into(),
            hex(&self.encode_with_grant(grant)).into(),
        ]
    }
}

struct Decoder<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> Decoder<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self, ProtocolError> {
        if bytes.get(..MAGIC.len()) != Some(MAGIC) {
            return Err(ProtocolError::InvalidHeader);
        }
        Ok(Self {
            bytes,
            cursor: MAGIC.len(),
        })
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], ProtocolError> {
        let end = self
            .cursor
            .checked_add(len)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(ProtocolError::Truncated)?;
        let value = &self.bytes[self.cursor..end];
        self.cursor = end;
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, ProtocolError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, ProtocolError> {
        Ok(u32::from_le_bytes(self.fixed()?))
    }

    const MIN_FIELD_BYTES: usize = 1;

    // Bound collection allocations by the minimum encoded size of each item.
    fn count(&mut self, minimum_bytes: usize) -> Result<usize, ProtocolError> {
        let count = self.u32()? as usize;
        if count > (self.bytes.len() - self.cursor) / minimum_bytes {
            return Err(ProtocolError::Truncated);
        }
        Ok(count)
    }

    fn u64(&mut self) -> Result<u64, ProtocolError> {
        Ok(u64::from_le_bytes(self.fixed()?))
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N], ProtocolError> {
        let mut value = [0; N];
        value.copy_from_slice(self.take(N)?);
        Ok(value)
    }

    fn frame(&mut self) -> Result<&'a [u8], ProtocolError> {
        let len = self.u32()? as usize;
        self.take(len)
    }

    fn text(&mut self) -> Result<&'a str, ProtocolError> {
        std::str::from_utf8(self.frame()?).map_err(|_| ProtocolError::NonUtf8Text)
    }

    fn string(&mut self) -> Result<String, ProtocolError> {
        self.text().map(str::to_owned)
    }

    fn cell_frame(&mut self, limit: usize) -> Result<&'a [u8], ProtocolError> {
        let bytes = self.frame()?;
        if bytes.len() > limit {
            return Err(ProtocolError::InvalidWorldCell);
        }
        Ok(bytes)
    }

    fn cell_string(&mut self, limit: usize) -> Result<String, ProtocolError> {
        std::str::from_utf8(self.cell_frame(limit)?)
            .map(str::to_owned)
            .map_err(|_| ProtocolError::NonUtf8Text)
    }

    fn world_cell_recipe(&mut self) -> Result<WorldCellRecipe, ProtocolError> {
        let source = self.cell_string(2 << 20)?;
        let template = self.cell_string(2 << 20)?;
        let count = self.count(8)?;
        if count > 256 {
            return Err(ProtocolError::InvalidWorldCell);
        }
        let mut turn_templates = Vec::with_capacity(count);
        for _ in 0..count {
            turn_templates.push((self.cell_string(65536)?, self.cell_string(2 << 20)?));
        }
        Ok(WorldCellRecipe {
            source,
            template,
            turn_templates,
        })
    }

    fn os_string(&mut self) -> Result<OsString, ProtocolError> {
        Ok(self.string()?.into())
    }

    fn maybe_string(&mut self) -> Result<Option<String>, ProtocolError> {
        match self.byte()? {
            0 => Ok(None),
            1 => Ok(Some(self.string()?)),
            tag => Err(ProtocolError::UnknownOptionalTextTag(tag)),
        }
    }

    fn symbol_identity(&mut self) -> Result<SymbolIdentity, ProtocolError> {
        Ok(SymbolIdentity {
            unit: self.string()?,
            module: self.string()?,
            namespace: self.string()?,
            occurrence: self.string()?,
            record_parent: self.maybe_string()?,
        })
    }

    fn structured_inspection(&mut self) -> Result<StructuredInspection, ProtocolError> {
        let scope = match self.byte()? {
            0 => InspectionScope::Current,
            1 => InspectionScope::PublicModule(self.string()?),
            tag => return Err(ProtocolError::UnknownInspectionScope(tag)),
        };
        let namespace = match self.byte()? {
            0 => InspectionNamespace::Any,
            1 => InspectionNamespace::Value,
            2 => InspectionNamespace::Type,
            3 => InspectionNamespace::Constructor,
            4 => InspectionNamespace::Field,
            tag => return Err(ProtocolError::UnknownInspectionNamespace(tag)),
        };
        Ok(StructuredInspection {
            scope,
            namespace,
            name: self.string()?,
            generation: self.u64()?,
            fingerprint: self.string()?,
        })
    }

    fn finish(self) -> Result<(), ProtocolError> {
        if self.cursor == self.bytes.len() {
            Ok(())
        } else {
            Err(ProtocolError::TrailingBytes)
        }
    }
}

/// A malformed versioned worker request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    InvalidExecutionGrant,
    InvalidWorkerArgv,
    NonUtf8Payload,
    InvalidHeader,
    Truncated,
    NonUtf8Text,
    TrailingBytes,
    OddHexLength,
    NonHexData,
    RetiredFieldTag(u8),
    UnknownFieldTag(u8),
    UnknownInspectionScope(u8),
    InvalidExactInputs,
    InvalidWorldPublicModule,
    InvalidWorldCell,
    RequestBudgetExceeded { declared: u64, limit: u32 },
    UnknownInspectionNamespace(u8),
    InvalidBrowseExpansion(u8),
    InvalidInspectionBudget,
    UnknownOptionalTextTag(u8),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidExecutionGrant => {
                write!(f, "invalid compiler workload or execution grant")
            }
            Self::InvalidWorkerArgv => {
                write!(
                    f,
                    "worker argv must be exactly {WORKER_REQUEST_FLAG} PAYLOAD"
                )
            }
            Self::NonUtf8Payload => f.write_str("worker request payload is not UTF-8"),
            Self::InvalidExactInputs => f.write_str("invalid source-free exact input acquisition"),
            Self::InvalidWorldPublicModule => {
                f.write_str("invalid source-free world public module query")
            }
            Self::InvalidWorldCell => f.write_str("invalid held world cell request"),
            Self::RequestBudgetExceeded { declared, limit } => write!(f, "compiler request needs {declared} known argv payload bytes; limit is {limit} before the actual cwd"),
            Self::InvalidHeader => f.write_str("invalid worker request header"),
            Self::Truncated => f.write_str("truncated worker request"),
            Self::NonUtf8Text => f.write_str("worker request text is not UTF-8"),
            Self::TrailingBytes => f.write_str("trailing worker request bytes"),
            Self::OddHexLength => f.write_str("worker request payload has odd length"),
            Self::NonHexData => f.write_str("worker request payload contains non-hexadecimal data"),
            Self::RetiredFieldTag(tag) => write!(f, "retired field tag {tag}"),
            Self::UnknownFieldTag(tag) => write!(f, "unknown field tag {tag}"),
            Self::UnknownInspectionScope(tag) => {
                write!(f, "unknown structured inspection scope tag {tag}")
            }
            Self::UnknownInspectionNamespace(tag) => {
                write!(f, "unknown structured inspection namespace tag {tag}")
            }
            Self::InvalidBrowseExpansion(tag) => {
                write!(f, "invalid bounded-browse expansion flag {tag}")
            }
            Self::InvalidInspectionBudget => {
                f.write_str("inspection budget must be positive")
            }
            Self::UnknownOptionalTextTag(tag) => {
                write!(f, "unknown optional-text tag {tag}")
            }
        }
    }
}

impl std::error::Error for ProtocolError {}

fn flag(out: &mut Vec<OsString>, name: &str, value: &OsStr) {
    out.push(name.into());
    out.push(value.to_owned());
}

fn push_u32(out: &mut Vec<u8>, value: usize) {
    assert!(
        u32::try_from(value).is_ok(),
        "extract request exceeds u32 field limit"
    );
    let value = value as u32;
    out.extend_from_slice(&value.to_le_bytes());
}

/// Zero would be indistinguishable from an omitted budget; a bounded request
/// always states a positive cap.
fn bounded_value(raw: &str, flag: &str) -> Result<(String, u32), CliError> {
    let (value, budget) = raw
        .rsplit_once('=')
        .ok_or_else(|| CliError::new(format!("{flag} requires VALUE=BUDGET")))?;
    Ok((value.to_owned(), bounded_budget(budget, flag)?))
}

fn bounded_budget(raw: &str, flag: &str) -> Result<u32, CliError> {
    match raw.parse::<u32>() {
        Ok(budget) if budget > 0 => Ok(budget),
        Ok(_) => Err(CliError::new(format!("{flag} requires a positive budget"))),
        Err(_) => Err(CliError::new(format!(
            "{flag} requires an unsigned integer budget"
        ))),
    }
}

fn nonzero_budget(budget: u32) -> Result<u32, ProtocolError> {
    if budget == 0 {
        Err(ProtocolError::InvalidInspectionBudget)
    } else {
        Ok(budget)
    }
}

fn push_frame(out: &mut Vec<u8>, value: &OsStr) {
    let value = value.to_string_lossy();
    push_u32(out, value.len());
    out.extend_from_slice(value.as_bytes());
}

fn encode_field(out: &mut Vec<u8>, field: &Field) {
    match field {
        Field::Input(value) => tagged_frame(out, 1, value),
        Field::OutputDir(value) => tagged_frame(out, 2, value),
        Field::Target(value) => tagged_frame(out, 3, value),
        Field::Targets(values) => {
            out.push(4);
            push_u32(out, values.len());
            for value in values {
                push_frame(out, OsStr::new(value));
            }
        }
        // Tag 5 was the retired Core dump request.
        Field::TargetModuleOnly => out.push(7),
        Field::Include(value) => tagged_frame(out, 8, value),
        Field::Turn => out.push(16),
        Field::TurnTemplate { kind, path } => {
            out.push(17);
            push_frame(out, OsStr::new(kind));
            push_frame(out, path.as_os_str());
        }
        Field::TurnOut(value) => tagged_frame(out, 18, value),
        Field::TurnVerdict(value) => tagged_frame(out, 19, value),
        Field::Classify => out.push(20),
        Field::ClassifyOut(value) => tagged_frame(out, 21, value),
        Field::Cell => out.push(31),
        Field::CellPlan => out.push(51),
        Field::CheckSource => out.push(52),
        Field::CellTemplate(value) => tagged_frame(out, 32, value.as_os_str()),
        Field::CellOut(value) => tagged_frame(out, 33, value),
        Field::BuildProductsDir(value) => tagged_frame(out, 25, value),
        Field::ModuleCandidates(value) => tagged_frame(out, 46, value.as_os_str()),
        Field::CertifyHomeProducts => out.push(50),
        Field::SessionArtifacts(value) => tagged_frame(out, 49, value.as_os_str()),
        Field::DeclarationJoin(value) => tagged_frame(out, 47, value.as_os_str()),
        Field::DeclarationJoinOut(value) => tagged_frame(out, 48, value.as_os_str()),
        Field::SessionRoot(value) => tagged_frame(out, 12, value),
        Field::SessionIncarnation(value) => tagged_frame(out, 44, value),
        Field::InjectVal(value) => tagged_frame(out, 13, value),
        Field::BindGen(value) => {
            out.push(11);
            out.extend_from_slice(&value.to_le_bytes());
        }
        Field::HarnessProfile => out.push(24),
        Field::InspectType(value) => tagged_frame(out, 26, OsStr::new(value)),
        Field::InspectInfo(value) => tagged_frame(out, 27, OsStr::new(value)),
        Field::InspectOut(value) => tagged_frame(out, 28, value),
        Field::InspectBrowse(value) => tagged_frame(out, 29, OsStr::new(value)),
        Field::InspectBrowseExpanded(value) => tagged_frame(out, 30, OsStr::new(value)),
        Field::InspectScopeBrowse => out.push(43),
        Field::InspectBrowseBounded(module, expanded, budget) => {
            out.push(62);
            push_frame(out, OsStr::new(module));
            out.push(u8::from(*expanded));
            out.extend_from_slice(&budget.to_le_bytes());
        }
        Field::InspectSearch(value) => tagged_frame(out, 35, OsStr::new(value)),
        Field::InspectSearchBounded(value, budget) => {
            out.push(63);
            push_frame(out, OsStr::new(value));
            out.extend_from_slice(&budget.to_le_bytes());
        }
        Field::InspectStructuredInfo(query) => encode_structured(out, 36, query),
        Field::InspectStructuredType(query) => encode_structured(out, 37, query),
        Field::RetainedGeneration(identity, generation) => {
            out.push(38);
            encode_symbol_identity(out, identity);
            out.extend_from_slice(&generation.to_le_bytes());
        }
        Field::InspectTypeBatch(value) => {
            out.push(40);
            push_frame(out, value.as_os_str());
        }
        Field::ActivationPreview => out.push(41),
        Field::InspectionStrict => out.push(42),
        Field::PortableOutput => out.push(53),
        Field::WorldCellPlan(request) => {
            out.push(59);
            push_frame(out, OsStr::new(&request.world));
            push_frame(out, OsStr::new(&request.realization));
            encode_world_cell_recipe(out, &request.recipe);
            push_frame(out, request.output.as_os_str());
        }
        Field::WorldCellPrepare(request) => {
            out.push(60);
            push_frame(out, OsStr::new(&request.world));
            push_frame(out, OsStr::new(&request.realization));
            encode_world_cell_recipe(out, &request.recipe);
            push_u32(out, request.plan.len());
            out.extend_from_slice(&request.plan);
            out.extend_from_slice(&request.admission);
            push_u32(out, request.slots.len());
            for (ordinal, slot) in &request.slots {
                out.extend_from_slice(&ordinal.to_le_bytes());
                out.push(match slot {
                    PlannedCellSlot::Prologue(_) => 0,
                    PlannedCellSlot::Declaration(_) => 1,
                    PlannedCellSlot::Bind(_) => 2,
                    PlannedCellSlot::Expression { .. } => 3,
                });
                out.extend_from_slice(&slot.generation().to_le_bytes());
                if let PlannedCellSlot::Expression { observation, .. } = slot {
                    push_frame(out, OsStr::new(observation));
                }
            }
            push_frame(out, request.directory.as_os_str());
            push_frame(out, request.output.as_os_str());
        }
        Field::WorldPublicModule(request) => {
            out.push(58);
            push_frame(out, OsStr::new(&request.world));
            push_frame(out, OsStr::new(&request.realization));
            out.push(match request.mode {
                WorldQueryMode::Info => 0,
                WorldQueryMode::Type => 1,
            });
            encode_name_selection(
                out,
                &request.query.scope,
                request.query.namespace,
                &request.query.name,
            );
            out.extend_from_slice(&request.query.generation.to_le_bytes());
            push_frame(out, OsStr::new(&request.query.fingerprint));
            push_frame(out, request.output.as_os_str());
        }
        Field::EnsureExactInputs(request) => {
            out.push(57);
            push_frame(out, OsStr::new(&request.root));
            match &request.acquisition {
                ExactInputsAcquisition::Resident(reference) => {
                    out.push(0);
                    push_frame(out, OsStr::new(reference));
                }
                ExactInputsAcquisition::Complete(path) => {
                    out.push(1);
                    push_frame(out, path.as_os_str());
                }
                ExactInputsAcquisition::Successor { predecessor, realization, descriptor } => {
                    out.push(2);
                    push_frame(out, OsStr::new(predecessor));
                    push_frame(out, OsStr::new(realization));
                    push_frame(out, descriptor.as_os_str());
                }
            }
        }
    }
}

fn encode_world_cell_recipe(out: &mut Vec<u8>, recipe: &WorldCellRecipe) {
    push_frame(out, OsStr::new(&recipe.source));
    push_frame(out, OsStr::new(&recipe.template));
    push_u32(out, recipe.turn_templates.len());
    for (kind, source) in &recipe.turn_templates {
        push_frame(out, OsStr::new(kind));
        push_frame(out, OsStr::new(source));
    }
}

fn encode_symbol_identity(out: &mut Vec<u8>, identity: &SymbolIdentity) {
    push_frame(out, OsStr::new(&identity.unit));
    push_frame(out, OsStr::new(&identity.module));
    push_frame(out, OsStr::new(&identity.namespace));
    push_frame(out, OsStr::new(&identity.occurrence));
    match &identity.record_parent {
        None => out.push(0),
        Some(parent) => {
            out.push(1);
            push_frame(out, OsStr::new(parent));
        }
    }
}

fn encode_name_selection(
    out: &mut Vec<u8>,
    scope: &InspectionScope,
    namespace: InspectionNamespace,
    name: &str,
) {
    match scope {
        InspectionScope::Current => out.push(0),
        InspectionScope::PublicModule(module) => {
            out.push(1);
            push_frame(out, OsStr::new(module));
        }
    }
    out.push(match namespace {
        InspectionNamespace::Any => 0,
        InspectionNamespace::Value => 1,
        InspectionNamespace::Type => 2,
        InspectionNamespace::Constructor => 3,
        InspectionNamespace::Field => 4,
    });
    push_frame(out, OsStr::new(name));
}

fn encode_structured(out: &mut Vec<u8>, tag: u8, query: &StructuredInspection) {
    out.push(tag);
    encode_name_selection(out, &query.scope, query.namespace, &query.name);
    out.extend_from_slice(&query.generation.to_le_bytes());
    push_frame(out, OsStr::new(&query.fingerprint));
}

fn structured_flag(flags: &mut Vec<OsString>, name: &str, query: &StructuredInspection) {
    let scope = match &query.scope {
        InspectionScope::Current => "current".to_owned(),
        InspectionScope::PublicModule(module) => format!("module:{module}"),
    };
    flag(
        flags,
        name,
        OsStr::new(&format!(
            "scope={scope};namespace={:?};name={};generation={};fingerprint={}",
            query.namespace, query.name, query.generation, query.fingerprint
        )),
    );
}

fn tagged_frame(out: &mut Vec<u8>, tag: u8, value: &OsStr) {
    out.push(tag);
    push_frame(out, value);
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

fn unhex(text: &str) -> Result<Vec<u8>, ProtocolError> {
    if !text.len().is_multiple_of(2) {
        return Err(ProtocolError::OddHexLength);
    }
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_digit(pair[0])?;
            let low = hex_digit(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn hex_digit(byte: u8) -> Result<u8, ProtocolError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(ProtocolError::NonHexData),
    }
}

fn value<'a>(
    args: &mut impl Iterator<Item = &'a OsString>,
    option: &str,
) -> Result<&'a OsStr, CliError> {
    args.next()
        .map(OsString::as_os_str)
        .ok_or_else(|| CliError::new(format!("{option} requires a value")))
}

fn text_value<'a>(
    args: &mut impl Iterator<Item = &'a OsString>,
    option: &str,
) -> Result<&'a str, CliError> {
    value(args, option)?
        .to_str()
        .ok_or_else(|| CliError::new(format!("{option} requires UTF-8 text")))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliError {
    message: String,
}

impl CliError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

#[cfg(test)]
mod tests {
    #[test]
    fn diagnostic_mode_preserves_preview_refinement_and_wire_roundtrip() {
        use super::{ExtractRequest, RequestMode};
        let mut command = crate::ExtractCmd::with_bin(crate::ResolvedExtractBin::assume_resolved(
            "selected-frontend",
        ));
        command.input("input.hs").turn().activation_preview();
        assert_eq!(command.request.mode(), RequestMode::ActivationPreview);
        assert_eq!(
            ExtractRequest::decode_worker_argv(&command.request.worker_argv())
                .unwrap()
                .mode(),
            RequestMode::ActivationPreview
        );
        let mut planned = crate::ExtractCmd::with_bin(crate::ResolvedExtractBin::assume_resolved(
            "selected-frontend",
        ));
        planned.input("input.hs").cell_plan();
        assert_eq!(planned.request.mode(), RequestMode::CellPlan);
        assert_eq!(
            ExtractRequest::decode_worker_argv(&planned.request.worker_argv())
                .unwrap()
                .mode(),
            RequestMode::CellPlan
        );
    }

    use super::*;

    #[test]
    fn execution_header_round_trips_and_refuses_nonpositive_grants() {
        let mut request = ExtractRequest::default();
        request.set_workload(CompileWorkload::Preparation);
        request.set_execution_grant(ExecutionGrant {
            jobs: 4,
            capabilities: 2,
        });
        request.input("Source.hs");
        let decoded = ExtractRequest::decode(&request.encode()).unwrap();
        assert_eq!(decoded.workload(), CompileWorkload::Preparation);
        assert_eq!(
            decoded.execution_grant(),
            ExecutionGrant {
                jobs: 4,
                capabilities: 2
            }
        );
        assert_eq!(decoded.cli_argv(), request.cli_argv());
        for offset in [9, 13] {
            let mut bytes = request.encode();
            bytes[offset..offset + 4].fill(0);
            assert_eq!(
                ExtractRequest::decode(&bytes).unwrap_err(),
                ProtocolError::InvalidExecutionGrant
            );
        }
        let mut invalid_class = request.encode();
        invalid_class[8] = 2;
        assert_eq!(
            ExtractRequest::decode(&invalid_class).unwrap_err(),
            ProtocolError::InvalidExecutionGrant
        );
        assert_eq!(
            ExtractRequest::from_cli(&[
                "Source.hs".into(),
                "--workload".into(),
                "preparation".into()
            ])
            .unwrap()
            .workload(),
            CompileWorkload::Preparation
        );
    }

    #[test]
    fn impossible_collection_counts_reject_before_allocating() {
        let mut fields = ExtractRequest::default().encode()[..17].to_vec();
        fields.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            ExtractRequest::decode(&fields).unwrap_err(),
            ProtocolError::Truncated
        );

        let mut targets = ExtractRequest::default().encode()[..17].to_vec();
        targets.extend_from_slice(&1u32.to_le_bytes());
        targets.push(4);
        targets.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            ExtractRequest::decode(&targets).unwrap_err(),
            ProtocolError::Truncated
        );
    }

    #[test]
    fn payload_free_tags_are_one_byte_fields() {
        let mut request = ExtractRequest::default().encode()[..17].to_vec();
        request.extend_from_slice(&2u32.to_le_bytes());
        request.extend_from_slice(&[7, 16]);

        let decoded = ExtractRequest::decode(&request).unwrap();
        assert!(matches!(
            decoded.fields.as_slice(),
            [Field::TargetModuleOnly, Field::Turn]
        ));
    }

    #[test]
    fn bounded_inspection_fields_round_trip_their_budget() {
        let mut request = ExtractRequest::default();
        request.inspect_browse_bounded("Data.Thing", true, 256);
        request.inspect_search_bounded("a -> a", 64);
        let decoded = ExtractRequest::decode(&request.encode()).unwrap();
        assert!(matches!(
            decoded.fields.as_slice(),
            [
                Field::InspectBrowseBounded(module, true, 256),
                Field::InspectSearchBounded(query, 64),
            ] if module == "Data.Thing" && query == "a -> a"
        ));
    }

    #[test]
    fn a_zero_inspection_budget_is_rejected_not_treated_as_unbounded() {
        let mut request = ExtractRequest::default().encode()[..17].to_vec();
        request.extend_from_slice(&1u32.to_le_bytes());
        request.push(61);
        request.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(
            ExtractRequest::decode(&request).unwrap_err(),
            ProtocolError::InvalidInspectionBudget
        );
    }

    #[test]
    fn request_has_versioned_header_and_typed_integer() {
        let mut request = ExtractRequest::default();
        request.input("Expr.hs");
        request.bind_gen(0x0102_0304_0506_0708);
        let bytes = request.encode();
        assert_eq!(&bytes[..8], MAGIC);
        assert_eq!(&bytes[17..21], &2u32.to_le_bytes());
        assert_eq!(bytes[21], 1);
        assert_eq!(bytes[33], 11);
        assert_eq!(&bytes[34..42], &0x0102_0304_0506_0708u64.to_le_bytes());
    }

    #[test]
    fn retained_generation_round_trips_through_the_typed_protocol() {
        let mut request = ExtractRequest::default();
        request.input("ImportConsumer.hs");
        let identity = SymbolIdentity {
            unit: "main".to_owned(),
            module: "ImportProducer".to_owned(),
            namespace: "value".to_owned(),
            occurrence: "producerValue".to_owned(),
            record_parent: None,
        };
        request.retained_generation(identity.clone(), 7);
        let decoded = ExtractRequest::decode(&request.encode()).unwrap();
        assert_eq!(
            decoded.retained_generations(),
            request.retained_generations()
        );
        assert_eq!(decoded.retained_generations().get(&identity), Some(&7u64));
    }

    #[test]
    fn retained_generation_with_a_record_parent_round_trips() {
        let mut request = ExtractRequest::default();
        request.input("Expr.hs");
        let identity = SymbolIdentity {
            unit: "main".to_owned(),
            module: "Records".to_owned(),
            namespace: "value".to_owned(),
            occurrence: "field".to_owned(),
            record_parent: Some("Parent".to_owned()),
        };
        request.retained_generation(identity.clone(), 3);
        let decoded = ExtractRequest::decode(&request.encode()).unwrap();
        assert_eq!(decoded.retained_generations().get(&identity), Some(&3u64));
    }

    #[test]
    fn activation_preview_round_trips_through_the_typed_protocol() {
        let mut request = ExtractRequest::default();
        request.activation_preview();
        let decoded = ExtractRequest::decode(&request.encode()).unwrap();
        assert!(matches!(
            decoded.fields.as_slice(),
            [Field::ActivationPreview]
        ));
    }

    #[test]
    fn portable_output_round_trips_and_refuses_retained_modes() {
        let mut request = ExtractRequest::default();
        request.input("Fixture.hs");
        request.output_dir("out");
        request.targets(["first", "second"]);
        request.portable_output();
        assert_eq!(request.admitted_mode().unwrap(), RequestMode::Source);
        let decoded = ExtractRequest::decode(&request.encode()).unwrap();
        assert_eq!(decoded.cli_argv(), request.cli_argv());
        assert_eq!(decoded.admitted_mode().unwrap(), RequestMode::Source);
        for conflicting in [
            Field::Turn,
            Field::Cell,
            Field::CellPlan,
            Field::CheckSource,
            Field::Classify,
            Field::ModuleCandidates("scope".into()),
            Field::SessionArtifacts("scope".into()),
            Field::CertifyHomeProducts,
            Field::TargetModuleOnly,
            Field::InjectVal("Binding".into()),
            Field::BindGen(1),
            Field::SessionRoot("session".into()),
            Field::SessionIncarnation("1".into()),
        ] {
            let mut invalid = request.clone();
            invalid.fields.push(conflicting);
            assert!(invalid.admitted_mode().is_err());
            assert!(ExtractRequest::decode(&invalid.encode())
                .unwrap()
                .admitted_mode()
                .is_err());
        }
        for missing in [
            Field::OutputDir("out".into()),
            Field::Targets(vec!["first".into(), "second".into()]),
        ] {
            let mut invalid = request.clone();
            invalid
                .fields
                .retain(|field| std::mem::discriminant(field) != std::mem::discriminant(&missing));
            assert!(invalid.admitted_mode().is_err());
        }
        for roots in [
            Field::Targets(Vec::new()),
            Field::Targets(vec![String::new()]),
            Field::Targets(vec!["first".into(), String::new()]),
            Field::Target("".into()),
        ] {
            let mut invalid = request.clone();
            invalid
                .fields
                .retain(|field| !matches!(field, Field::Target(_) | Field::Targets(_)));
            invalid.fields.push(roots);
            assert!(invalid.admitted_mode().is_err());
            assert!(ExtractRequest::decode(&invalid.encode())
                .unwrap()
                .admitted_mode()
                .is_err());
        }
        assert_eq!(
            ExtractRequest::from_cli(&request.cli_argv())
                .unwrap()
                .cli_argv(),
            request.cli_argv()
        );
    }

    #[test]
    fn module_candidates_round_trip() {
        let mut request = ExtractRequest::default();
        request.input("Expr.hs");
        request.module_candidates(Path::new("/tmp/candidates.cbor"));
        let decoded = ExtractRequest::decode(&request.encode()).unwrap();
        assert_eq!(decoded.cli_argv(), request.cli_argv());
        assert!(matches!(
            decoded.fields.as_slice(),
            [Field::Input(_), Field::ModuleCandidates(_)]
        ));
    }

    #[test]
    fn certify_home_products_round_trips() {
        let mut request = ExtractRequest::default();
        request.input("Probe.hs");
        request.certify_home_products();
        let decoded = ExtractRequest::decode(&request.encode()).unwrap();
        assert_eq!(decoded.cli_argv(), request.cli_argv());
        assert!(matches!(
            decoded.fields.as_slice(),
            [Field::Input(_), Field::CertifyHomeProducts]
        ));
    }

    #[test]
    fn exact_session_artifacts_round_trip() {
        let mut request = ExtractRequest::default();
        request.input("Expr.hs");
        request.session_artifacts(Path::new("/tmp/session-artifacts.cbor"));
        let decoded = ExtractRequest::decode(&request.encode()).unwrap();
        assert_eq!(decoded.cli_argv(), request.cli_argv());
        assert!(matches!(
            decoded.fields.as_slice(),
            [Field::Input(_), Field::SessionArtifacts(_)]
        ));
    }

    #[test]
    fn relocated_turn_outputs_replace_old_sinks_and_preserve_recipe() {
        let mut request = ExtractRequest::default();
        request.input("authored-turn.txt");
        request.turn();
        request.include("/source/first");
        request.include("/source/second");
        request.output_dir("/caller/output");
        request.turn_out("/caller/result.cbor");
        request.relocate_turn_outputs(Path::new("/owned/output"));
        let decoded = ExtractRequest::decode(&request.encode()).unwrap();
        assert!(decoded.is_turn());
        assert!(decoded.supports_compile_input_identity());
        let mut mutable = decoded.clone();
        mutable.session_incarnation("resident");
        assert!(!mutable.supports_compile_input_identity());
        assert_eq!(
            decoded.include_paths(),
            vec![Path::new("/source/first"), Path::new("/source/second")]
        );
        assert_eq!(
            decoded.output_directory(),
            Some(OsStr::new("/owned/output"))
        );
        assert_eq!(
            decoded
                .fields
                .iter()
                .filter(|field| matches!(field, Field::OutputDir(_)))
                .count(),
            1
        );
        assert_eq!(
            decoded
                .fields
                .iter()
                .filter(|field| matches!(field, Field::TurnOut(_)))
                .count(),
            1
        );
        assert!(decoded.fields.iter().any(|field| matches!(field, Field::TurnOut(path) if path == OsStr::new("/owned/output/turn.cbor"))));
        assert!(!decoded
            .cli_argv()
            .iter()
            .any(|argument| argument == "/caller/output" || argument == "/caller/result.cbor"));
    }

    #[test]
    fn declaration_join_paths_round_trip() {
        let mut request = ExtractRequest::default();
        request.input("Candidate.hs");
        request.declaration_join(Path::new("/tmp/join-input.cbor"));
        request.declaration_join_out(Path::new("/tmp/join-output.json"));
        let decoded = ExtractRequest::decode(&request.encode()).unwrap();
        assert_eq!(decoded.cli_argv(), request.cli_argv());
        assert!(matches!(
            decoded.fields.as_slice(),
            [
                Field::Input(_),
                Field::DeclarationJoin(_),
                Field::DeclarationJoinOut(_)
            ]
        ));
    }

    #[test]
    fn strict_inspection_round_trips_through_the_typed_protocol() {
        let mut request = ExtractRequest::default();
        request.inspection_strict();
        let decoded = ExtractRequest::decode(&request.encode()).unwrap();
        assert!(matches!(
            decoded.fields.as_slice(),
            [Field::InspectionStrict]
        ));
    }

    #[test]
    fn inspection_type_batch_round_trips_through_the_typed_protocol() {
        let mut request = ExtractRequest::default();
        request.input("query-0/Expr.hs");
        request.input("query-1/Expr.hs");
        request.inspect_type("id");
        request.inspect_type("1");
        request.inspect_type_batch(Path::new("type-batch/Expr.hs"));

        let decoded = ExtractRequest::decode(&request.encode()).unwrap();
        assert_eq!(decoded.cli_argv(), request.cli_argv());
        assert!(decoded
            .cli_argv()
            .windows(2)
            .any(|args| args == ["--inspect-type-batch", "type-batch/Expr.hs"]));
    }

    #[test]
    fn prepared_turn_is_absent_from_the_cli_flag_parser() {
        let error = ExtractRequest::from_cli(&["--prepared-turn".into()]).unwrap_err();
        assert_eq!(error.to_string(), "unknown option: --prepared-turn");
    }

    #[test]
    fn retained_generation_is_absent_from_the_cli_flag_parser() {
        // "extend, do not add a flag parser": the field exists only in the
        // typed builder/encoder, never as CLI text a human or a caller could
        // pass to `from_cli`. `cli_argv()` still RENDERS it (for cache-key
        // and diagnostic purposes), but nothing parses that rendering back.
        let mut request = ExtractRequest::default();
        request.input("Expr.hs");
        request.retained_generation(
            SymbolIdentity {
                unit: "main".to_owned(),
                module: "M".to_owned(),
                namespace: "value".to_owned(),
                occurrence: "x".to_owned(),
                record_parent: None,
            },
            1,
        );
        let argv = request.cli_argv();
        assert!(argv.iter().any(|arg| arg == "--retained-generation"));
        // The allowlisted CLI flags in `from_cli` do not include it.
        let error =
            ExtractRequest::from_cli(&["--retained-generation".into(), "x".into()]).unwrap_err();
        assert_eq!(error.to_string(), "unknown option: --retained-generation");
    }

    #[test]
    fn worker_argv_contains_only_the_typed_protocol() {
        let request =
            ExtractRequest::from_cli(&["Expr.hs".into(), "--target".into(), "answer".into()])
                .unwrap();
        let argv = request.worker_argv();
        assert_eq!(argv.len(), 2);
        assert_eq!(argv[0], WORKER_REQUEST_FLAG);
        let decoded = ExtractRequest::decode_worker_argv(&argv).unwrap();
        assert_eq!(decoded.target_names(), ["answer"]);
    }

    #[test]
    fn worker_argv_rejects_retired_version_markers() {
        let request = ExtractRequest::from_cli(&["Expr.hs".into()]).unwrap();
        let payload = OsString::from(hex(&request.encode()));
        for flag in [
            "--worker-request-v1",
            "--worker-request-v2",
            "--worker-request-v3",
            "--worker-request-v4",
            "--worker-request-v5",
            "--worker-request-v6",
            "--worker-request-v7",
            "--worker-request-v8",
            "--worker-request-v9",
            "--worker-request-v10",
            "--worker-request-v11",
            "--worker-request-v12",
            "--worker-request-v18",
            "--worker-request-v21",
        ] {
            assert_eq!(
                ExtractRequest::decode_worker_argv(&[flag.into(), payload.clone()]).unwrap_err(),
                ProtocolError::InvalidWorkerArgv
            );
        }
    }

    #[test]
    fn typed_protocol_rejects_retired_magic_versions() {
        for magic in [
            b"TPREQ001",
            b"TPREQ002",
            b"TPREQ003",
            b"TPREQ004",
            b"TPREQ005",
            b"TPREQ006",
            b"TPREQ007",
            b"TPREQ008",
            b"TPREQ009",
            b"TPREQ010",
            b"TPREQ011",
            b"TPREQ012",
            b"TPREQ013",
            b"TPREQ014",
            b"TPREQ015",
            b"TPREQ016",
            b"TPREQ017",
            b"TPREQ018",
            b"TPREQ020",
            b"TPREQ021",
            b"TPREQ022",
            b"TPREQ023",
        ] {
            let mut request = magic.to_vec();
            request.extend_from_slice(&0u32.to_le_bytes());
            assert_eq!(
                ExtractRequest::decode(&request).unwrap_err(),
                ProtocolError::InvalidHeader
            );
        }
    }

    #[test]
    fn typed_protocol_round_trips_common_turn_fields() {
        let args = vec![
            "Expr.hs".into(),
            "--output-dir".into(),
            "/tmp/out".into(),
            "--targets".into(),
            "a,b".into(),
            "--include".into(),
            "/tmp/include".into(),
            "--turn".into(),
            "--turn-template".into(),
            "expr=/tmp/template.hs".into(),
            "--turn-out".into(),
            "/tmp/turn.cbor".into(),
            "--turn-verdict".into(),
            "expr".into(),
            "--build-products-dir".into(),
            "/tmp/build".into(),
            "--module-candidates".into(),
            "/tmp/module-candidates.cbor".into(),
            "--session-root".into(),
            "/tmp/session".into(),
            "--session-incarnation".into(),
            "12345".into(),
            "--inject-val".into(),
            "M".into(),
            "--bind-gen".into(),
            "7".into(),
            "--harness-profile".into(),
        ];
        let request = ExtractRequest::from_cli(&args).unwrap();
        let decoded = ExtractRequest::decode(&request.encode()).unwrap();
        assert_eq!(decoded.cli_argv(), request.cli_argv());
    }

    #[test]
    fn cli_admits_one_operation_and_rejects_near_valid_conflicts() {
        // Operation recipes are independent of dispatch ordering. Reordering,
        // repeated selection and compile-context additions retain the operation.
        let recipes: &[(&[&str], RequestMode)] = &[
            (&[], RequestMode::Source),
            (&["--turn"], RequestMode::Turn),
            (&["--classify"], RequestMode::Classify),
            (&["--cell"], RequestMode::CellProgram),
            (&["--cell-plan"], RequestMode::CellPlan),
            (&["--check-source"], RequestMode::CheckSource),
            (
                &[
                    "--inspect-type",
                    "Int",
                    "--inspect-info",
                    "Maybe",
                    "--inspect-browse",
                    "Tidepool.Prelude",
                    "--inspect-browse-expanded",
                    "Tidepool.Actors.Exomonad",
                    "--inspect-scope-browse",
                    "--inspect-search",
                    "Response result -> Await result",
                    "--inspect-out",
                    "inspection.cbor",
                ],
                RequestMode::Inspection,
            ),
            (
                &[
                    "--declaration-join",
                    "join",
                    "--declaration-join-out",
                    "receipt",
                ],
                RequestMode::DeclarationInterface,
            ),
        ];
        let parse = |args: Vec<&str>| {
            ExtractRequest::from_cli(&args.into_iter().map(OsString::from).collect::<Vec<_>>())
        };
        for (index, (recipe, expected)) in recipes.iter().enumerate() {
            for context_first in [false, true] {
                let mut args = vec!["input.hs"];
                let context = ["--include", "lib", "--build-products-dir", "products"];
                if context_first {
                    args.extend(context);
                }
                args.extend_from_slice(recipe);
                args.extend_from_slice(recipe);
                if !context_first {
                    args.extend(context);
                }
                let request = parse(args).unwrap();
                assert_eq!(request.mode(), *expected);
                let decoded = ExtractRequest::decode(&request.encode()).unwrap();
                assert_eq!(decoded.mode(), *expected);
                assert_eq!(decoded.cli_argv(), request.cli_argv());
            }
            if index == 0 {
                continue;
            }
            for (other, _) in recipes.iter().skip(index + 1) {
                for reverse in [false, true] {
                    let mut args = vec!["input.hs"];
                    let (first, second) = if reverse {
                        (*other, *recipe)
                    } else {
                        (*recipe, *other)
                    };
                    args.extend_from_slice(first);
                    args.extend_from_slice(second);
                    assert!(
                        parse(args).is_err(),
                        "conflicting operation recipes accepted"
                    );
                }
            }
        }
        for orphan in [
            vec!["--declaration-join", "join"],
            vec!["--declaration-join-out", "receipt"],
        ] {
            for (recipe, _) in recipes.iter().take(7) {
                let mut args = vec!["input.hs"];
                args.extend_from_slice(recipe);
                args.extend_from_slice(&orphan);
                assert!(
                    parse(args).is_err(),
                    "incomplete declaration operation accepted"
                );
            }
        }
        for mode in [
            "--turn",
            "--classify",
            "--cell",
            "--cell-plan",
            "--check-source",
        ] {
            assert!(parse(vec!["input.hs", "--certify-home-products", mode]).is_err());
        }
        assert_eq!(
            parse(vec![
                "input.hs",
                "--certify-home-products",
                "--session-artifacts",
                "scope"
            ])
            .unwrap()
            .mode(),
            RequestMode::Source
        );
    }

    #[test]
    fn checking_only_source_request_round_trips_without_output_authority() {
        let args = [
            "WholeModule.hs".into(),
            "--check-source".into(),
            "--include".into(),
            "/tmp/source-graph".into(),
            "--module-candidates".into(),
            "/tmp/candidates.cbor".into(),
            "--build-products-dir".into(),
            "/tmp/interfaces".into(),
        ];
        let request = ExtractRequest::from_cli(&args).unwrap();
        let decoded = ExtractRequest::decode(&request.encode()).unwrap();
        assert!(decoded.is_check_source());
        assert_eq!(decoded.cli_argv(), request.cli_argv());
        assert!(!decoded.is_turn());
        assert!(decoded.output_directory().is_none());
    }

    #[test]
    fn parser_only_cell_mode_round_trips() {
        let args = [
            "cell.txt".into(),
            "--cell-plan".into(),
            "--cell-template".into(),
            "/tmp/template.hs".into(),
            "--cell-out".into(),
            "/tmp/plan.cbor".into(),
        ];
        let request = ExtractRequest::from_cli(&args).unwrap();
        let decoded = ExtractRequest::decode(&request.encode()).unwrap();
        assert_eq!(decoded.cli_argv(), request.cli_argv());
        assert!(decoded.cli_argv().contains(&OsString::from("--cell-plan")));
    }

    #[test]
    fn typed_protocol_rejects_unknown_and_truncated_fields() {
        let mut unknown = ExtractRequest::default().encode()[..17].to_vec();
        unknown.extend_from_slice(&1u32.to_le_bytes());
        unknown.push(255);
        assert_eq!(
            ExtractRequest::decode(&unknown).unwrap_err(),
            ProtocolError::UnknownFieldTag(255)
        );

        let request = ExtractRequest::from_cli(&["Expr.hs".into()]).unwrap();
        let mut truncated = request.encode();
        truncated.pop();
        assert_eq!(
            ExtractRequest::decode(&truncated).unwrap_err(),
            ProtocolError::Truncated
        );
    }

    #[test]
    fn typed_protocol_rejects_retired_fields_explicitly() {
        for tag in [6, 9, 10, 14, 34, 39, 45] {
            let mut request = ExtractRequest::default().encode()[..17].to_vec();
            request.extend_from_slice(&1u32.to_le_bytes());
            request.push(tag);
            assert_eq!(
                ExtractRequest::decode(&request).unwrap_err(),
                ProtocolError::RetiredFieldTag(tag)
            );
        }
    }

    #[test]
    fn cli_rejects_retired_cell_fold_and_type_pin_options() {
        for option in ["--cell-fold-turn", "--turn-pin"] {
            let error = ExtractRequest::from_cli(&[option.into()]).unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("retired compiler option: {option}")
            );
        }
    }

    #[test]
    fn cli_rejects_unknown_options() {
        let error = ExtractRequest::from_cli(&["--traget".into(), "answer".into()]).unwrap_err();
        assert_eq!(error.to_string(), "unknown option: --traget");
    }

    #[test]
    fn cli_rejects_invalid_typed_values() {
        let error = ExtractRequest::from_cli(&["--bind-gen".into(), "nope".into()]).unwrap_err();
        assert_eq!(error.to_string(), "--bind-gen requires an unsigned integer");
    }

    #[test]
    fn cli_requires_an_input() {
        let error = ExtractRequest::from_cli(&["--target".into(), "answer".into()]).unwrap_err();
        assert_eq!(error.to_string(), "an input file is required");
    }

    #[test]
    fn world_public_module_wire_is_exclusive_and_matches_haskell() {
        let query = WorldPublicModuleRequest {
            world: "a".repeat(64),
            realization: "realized".into(),
            mode: WorldQueryMode::Info,
            query: StructuredInspection {
                scope: InspectionScope::PublicModule("Data.List".into()),
                namespace: InspectionNamespace::Value,
                name: "map".into(),
                generation: 1,
                fingerprint: "display-only".into(),
            },
            output: "answer.bin".into(),
        };
        let mut request = ExtractRequest::default();
        request.world_public_module(query.clone());
        let bytes = unhex("5450524551303235000100000001000000010000003a4000000061616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161080000007265616c697a6564000109000000446174612e4c69737401030000006d617001000000000000000c000000646973706c61792d6f6e6c790a000000616e737765722e62696e").unwrap();
        assert_eq!(request.encode(), bytes);
        for mode in [WorldQueryMode::Info, WorldQueryMode::Type] {
            let mut request = ExtractRequest::default();
            request.world_public_module(WorldPublicModuleRequest {
                mode,
                ..query.clone()
            });
            assert_eq!(
                request.admitted_mode().unwrap(),
                RequestMode::WorldPublicModule
            );
            assert_eq!(
                ExtractRequest::decode(&request.encode()).unwrap().encode(),
                request.encode()
            );
        }
        for extra in [
            Field::Input("missing.hs".into()),
            Field::OutputDir("missing".into()),
            Field::Include("ambient".into()),
            Field::SessionArtifacts("missing".into()),
            Field::InspectOut("other".into()),
            Field::WorldPublicModule(query.clone()),
            Field::EnsureExactInputs(ExactInputsRequest {
                root: query.world.clone(),
                acquisition: ExactInputsAcquisition::Resident("realized".into()),
            }),
        ] {
            for reverse in [false, true] {
                let mut mixed = request.clone();
                if reverse {
                    mixed.fields.insert(0, extra.clone());
                } else {
                    mixed.fields.push(extra.clone());
                }
                assert!(mixed.admitted_mode().is_err());
                assert!(ExtractRequest::decode(&mixed.encode()).is_err());
            }
        }
        for invalid in [
            WorldPublicModuleRequest {
                world: "caller-label".into(),
                ..query.clone()
            },
            WorldPublicModuleRequest {
                realization: String::new(),
                ..query.clone()
            },
            WorldPublicModuleRequest {
                output: PathBuf::new(),
                ..query.clone()
            },
            WorldPublicModuleRequest {
                query: StructuredInspection {
                    scope: InspectionScope::Current,
                    ..query.query.clone()
                },
                ..query.clone()
            },
        ] {
            let mut invalid_request = ExtractRequest::default();
            invalid_request.world_public_module(invalid);
            assert_eq!(
                ExtractRequest::decode(&invalid_request.encode()).unwrap_err(),
                ProtocolError::InvalidWorldPublicModule
            );
        }
        for end in 0..bytes.len() {
            assert!(ExtractRequest::decode(&bytes[..end]).is_err());
        }
        let mut bad_mode = bytes;
        bad_mode[102] = 2;
        assert_eq!(
            ExtractRequest::decode(&bad_mode).unwrap_err(),
            ProtocolError::InvalidWorldPublicModule
        );
    }

    #[test]
    fn world_cell_wire_is_exclusive_bounded_and_matches_haskell() {
        let recipe = Arc::new(WorldCellRecipe {
            source: "x <- pure 1".into(),
            template: "{{CELL_IMPORTS}}\n{{CELL_BODY}}".into(),
            turn_templates: vec![("bind".into(), "module Bind where".into())],
        });
        let plan = WorldCellPlanRequest {
            world: "a".repeat(64),
            realization: "held".into(),
            recipe: recipe.clone(),
            output: "plan.cbor".into(),
        };
        let prepare = WorldCellPrepareRequest {
            world: plan.world.clone(),
            realization: plan.realization.clone(),
            recipe,
            plan: Arc::from(&b"plan-receipt"[..]),
            admission: [b'b'; 32],
            slots: vec![
                (0, PlannedCellSlot::Bind(7)),
                (
                    1,
                    PlannedCellSlot::Expression {
                        generation: 8,
                        observation: "it".into(),
                    },
                ),
            ],
            directory: "operation".into(),
            output: "prepared.cbor".into(),
        };
        assert!(Arc::ptr_eq(&plan.recipe, &prepare.recipe));
        assert!(Arc::ptr_eq(&plan.recipe, &plan.clone().recipe));
        let plan_bytes = unhex("5450524551303235000100000001000000010000003b40000000616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161610400000068656c640b00000078203c2d207075726520311e0000007b7b43454c4c5f494d504f5254537d7d0a7b7b43454c4c5f424f44597d7d010000000400000062696e64110000006d6f64756c652042696e6420776865726509000000706c616e2e63626f72").unwrap();
        let prepare_bytes = unhex("5450524551303235000100000001000000010000003c40000000616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161610400000068656c640b00000078203c2d207075726520311e0000007b7b43454c4c5f494d504f5254537d7d0a7b7b43454c4c5f424f44597d7d010000000400000062696e64110000006d6f64756c652042696e642077686572650c000000706c616e2d7265636569707462626262626262626262626262626262626262626262626262626262626262620200000000000000000000000207000000000000000100000000000000030800000000000000020000006974090000006f7065726174696f6e0d00000070726570617265642e63626f72").unwrap();
        let mut plan_request = ExtractRequest::default();
        plan_request.world_cell_plan(plan.clone()).unwrap();
        let mut prepare_request = ExtractRequest::default();
        prepare_request.world_cell_prepare(prepare.clone()).unwrap();
        for (request, bytes, mode) in [
            (plan_request, plan_bytes, RequestMode::WorldCellPlan),
            (
                prepare_request,
                prepare_bytes,
                RequestMode::WorldCellPrepare,
            ),
        ] {
            assert_eq!(request.encode(), bytes);
            assert_eq!(ExtractRequest::decode(&bytes).unwrap().encode(), bytes);
            assert_eq!(request.admitted_mode().unwrap(), mode);
            for end in 0..bytes.len() {
                assert!(ExtractRequest::decode(&bytes[..end]).is_err());
            }
            for extra in [
                Field::Input("ambient.hs".into()),
                Field::Include("ambient".into()),
                Field::WorldCellPlan(plan.clone()),
                Field::WorldCellPrepare(prepare.clone()),
            ] {
                for first in [false, true] {
                    let mut mixed = request.clone();
                    if first {
                        mixed.fields.insert(0, extra.clone());
                    } else {
                        mixed.fields.push(extra.clone());
                    }
                    assert_eq!(
                        ExtractRequest::decode(&mixed.encode()).unwrap_err(),
                        ProtocolError::InvalidWorldCell
                    );
                }
            }
        }
        let mut bad_recipes = Vec::new();
        let mut value = plan.clone();
        Arc::make_mut(&mut value.recipe).source = "λ".repeat((1 << 20) + 1);
        bad_recipes.push(value);
        let mut value = plan.clone();
        Arc::make_mut(&mut value.recipe).turn_templates[0].0.clear();
        bad_recipes.push(value);
        let mut value = plan.clone();
        value.output = "x".repeat(65537).into();
        bad_recipes.push(value);
        for value in bad_recipes {
            let mut request = ExtractRequest::default();
            assert!(request.world_cell_plan(value.clone()).is_err());
            assert!(request.fields.is_empty());
            request.fields.push(Field::WorldCellPlan(value));
            assert!(ExtractRequest::decode(&request.encode()).is_err());
        }
        let mut wrong_slots = Vec::new();
        let mut value = prepare.clone();
        value.slots[1].0 = 0;
        wrong_slots.push(value);
        let mut value = prepare.clone();
        value.slots.reverse();
        wrong_slots.push(value);
        let mut value = prepare.clone();
        value.slots[1].1 = PlannedCellSlot::Bind(7);
        wrong_slots.push(value);
        for value in wrong_slots {
            let mut request = ExtractRequest::default();
            assert!(request.world_cell_prepare(value.clone()).is_err());
            assert!(request.fields.is_empty());
            request.fields.push(Field::WorldCellPrepare(value));
            assert_eq!(
                ExtractRequest::decode(&request.encode()).unwrap_err(),
                ProtocolError::InvalidWorldCell
            );
        }
        let mut overlapping = prepare.clone();
        overlapping.slots = vec![
            (0, PlannedCellSlot::Declaration(1)),
            (1, PlannedCellSlot::Bind(1)),
        ];
        let mut request = ExtractRequest::default();
        request.world_cell_prepare(overlapping.clone()).unwrap();
        assert_eq!(
            ExtractRequest::decode(&request.encode()).unwrap().encode(),
            request.encode()
        );
        overlapping.slots[1].1 = PlannedCellSlot::Prologue(1);
        assert_eq!(
            overlapping.validate_transport(),
            Err(ProtocolError::InvalidWorldCell)
        );
        overlapping.slots[0].1 = PlannedCellSlot::Expression {
            generation: 1,
            observation: "it".into(),
        };
        overlapping.slots[1].1 = PlannedCellSlot::Bind(1);
        assert_eq!(
            overlapping.validate_transport(),
            Err(ProtocolError::InvalidWorldCell)
        );

        let mut empty = prepare;
        empty.slots.clear();
        let mut request = ExtractRequest::default();
        request.world_cell_prepare(empty).unwrap();
        assert!(ExtractRequest::decode(&request.encode()).is_ok()); // Semantic empty-plan proof is owned by the compiler.

        let mut ordered = plan;
        Arc::make_mut(&mut ordered.recipe).turn_templates = vec![
            ("expr".into(), "effectful expression template".into()),
            ("expr".into(), "pure expression template".into()),
        ];
        let mut request = ExtractRequest::default();
        request.world_cell_plan(ordered.clone()).unwrap();
        let decoded = ExtractRequest::decode(&request.encode()).unwrap();
        let [Field::WorldCellPlan(actual)] = decoded.fields.as_slice() else {
            panic!("expected exactly one decoded World cell plan");
        };
        assert_eq!(actual, &ordered);
    }

    #[test]
    fn world_prepare_bounds_borrowed_names_before_owned_slot_copies() {
        let recipe = WorldCellRecipe {
            source: "pure ()".into(),
            template: "{{CELL_BODY}}".into(),
            turn_templates: vec![],
        };
        let world = "a".repeat(64);
        let oversized = "x".repeat(65537);
        assert_eq!(
            WorldCellPrepareRequest::validate_borrowed(
                &world,
                "held",
                &recipe,
                b"plan",
                [(
                    0,
                    PlannedCellSlotRef::Expression {
                        generation: 1,
                        observation: &oversized
                    }
                )],
                Path::new("operation"),
                Path::new("prepared.cbor"),
            ),
            Err(ProtocolError::InvalidWorldCell)
        );
        let within_field_limit = "x".repeat(65536);
        let aggregate = (0..200).map(|index| {
            (
                index,
                PlannedCellSlotRef::Expression {
                    generation: index + 1,
                    observation: within_field_limit.as_str(),
                },
            )
        });
        assert!(matches!(
            WorldCellPrepareRequest::validate_borrowed(
                &world,
                "held",
                &recipe,
                b"plan",
                aggregate,
                Path::new("operation"),
                Path::new("prepared.cbor"),
            ),
            Err(ProtocolError::RequestBudgetExceeded { .. })
        ));
    }

    #[test]
    fn world_cell_builders_bound_hex_expansion_before_appending_source() {
        // Four existing 2MiB fields already exceed the SAME daemon ceiling
        // after typed framing and hex. No 256-field/512MiB fixture is needed.
        let body = "x".repeat(2 << 20);
        let recipe = Arc::new(WorldCellRecipe {
            source: body.clone(),
            template: body.clone(),
            turn_templates: vec![("one".into(), body.clone()), ("two".into(), body)],
        });
        let mut request = ExtractRequest::default();
        let refused = match request.world_cell_plan(WorldCellPlanRequest {
            world: "a".repeat(64),
            realization: "held".into(),
            recipe,
            output: "plan.cbor".into(),
        }) {
            Err(failure) => failure,
            Ok(_) => panic!("over-budget bulk recipe was appended"),
        };
        assert!(
            matches!(refused,ProtocolError::RequestBudgetExceeded {declared,limit}
            if declared > u64::from(limit) && limit==crate::daemon::MAX_REQUEST_FRAME_BYTES)
        );
        assert!(request.fields.is_empty());
        // The existing receipt field alone can fill 8MiB. Hex and the real
        // prepare envelope must still be charged before it is appended.
        let refused = match request.world_cell_prepare(WorldCellPrepareRequest {
            world: "a".repeat(64),
            realization: "held".into(),
            recipe: Arc::new(WorldCellRecipe {
                source: String::new(),
                template: String::new(),
                turn_templates: vec![],
            }),
            plan: vec![0; 8 << 20].into(),
            admission: [1; 32],
            slots: vec![],
            directory: "operation".into(),
            output: "prepared.cbor".into(),
        }) {
            Err(failure) => failure,
            Ok(_) => panic!("over-budget plan receipt was appended"),
        };
        assert!(
            matches!(refused,ProtocolError::RequestBudgetExceeded {declared,limit}
            if declared > u64::from(limit) && limit==crate::daemon::MAX_REQUEST_FRAME_BYTES)
        );
        assert!(request.fields.is_empty());
    }

    #[test]
    fn exact_inputs_requests_are_source_free_and_exclusive() {
        let resident = ExactInputsRequest {
            root: "a".repeat(64),
            acquisition: ExactInputsAcquisition::Resident("realized".into()),
        };
        let make = |inputs| ExtractRequest {
            fields: vec![Field::EnsureExactInputs(inputs)],
            ..ExtractRequest::default()
        };
        let request = make(resident.clone());
        // Independent fixed bytes also checked by the maintained Haskell codec case.
        let bytes = unhex("54505245513032350001000000010000000100000039400000006161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616100080000007265616c697a6564").unwrap();
        assert_eq!(request.encode(), bytes);
        assert_eq!(
            ExtractRequest::decode(&bytes).unwrap().mode(),
            RequestMode::EnsureExactInputs
        );
        assert_eq!(
            ExtractRequest::decode(&bytes).unwrap().encode(),
            request.encode()
        );
        let complete = make(ExactInputsRequest {
            root: "a".repeat(64),
            acquisition: ExactInputsAcquisition::Complete("/scope.cbor".into()),
        });
        assert_eq!(
            ExtractRequest::decode(&complete.encode()).unwrap().encode(),
            complete.encode()
        );
        // Envelope admission metadata does not become source or selection authority.
        let mut admitted = request.clone();
        admitted.set_workload(CompileWorkload::Preparation);
        admitted.set_execution_grant(ExecutionGrant {
            jobs: 2,
            capabilities: 3,
        });
        assert_eq!(
            ExtractRequest::decode(&admitted.encode()).unwrap().mode(),
            RequestMode::EnsureExactInputs
        );
        for extra in [
            Field::Input("missing.hs".into()),
            Field::OutputDir("missing".into()),
            Field::Include("missing".into()),
            Field::SessionArtifacts("missing".into()),
            Field::ModuleCandidates("missing".into()),
            Field::HarnessProfile,
            Field::EnsureExactInputs(resident.clone()),
            Field::Turn,
            Field::PortableOutput,
        ] {
            for reverse in [false, true] {
                let mut bad = request.clone();
                if reverse {
                    bad.fields.insert(0, extra.clone());
                } else {
                    bad.fields.push(extra.clone());
                }
                assert_eq!(
                    ExtractRequest::decode(&bad.encode()).unwrap_err(),
                    ProtocolError::InvalidExactInputs
                );
                assert!(bad.admitted_mode().is_err());
            }
        }
        for bad in [
            ExactInputsRequest {
                root: "".into(),
                acquisition: resident.acquisition.clone(),
            },
            ExactInputsRequest {
                root: "root\0other".into(),
                acquisition: resident.acquisition.clone(),
            },
            ExactInputsRequest {
                root: "a".repeat(64),
                acquisition: ExactInputsAcquisition::Resident("".into()),
            },
            ExactInputsRequest {
                root: "a".repeat(64),
                acquisition: ExactInputsAcquisition::Complete("".into()),
            },
        ] {
            assert_eq!(
                ExtractRequest::decode(&make(bad).encode()).unwrap_err(),
                ProtocolError::InvalidExactInputs
            );
        }
        for end in 0..bytes.len() {
            assert!(ExtractRequest::decode(&bytes[..end]).is_err());
        }
        let mut unknown = bytes.clone();
        unknown[90] = 3;
        assert_eq!(
            ExtractRequest::decode(&unknown).unwrap_err(),
            ProtocolError::InvalidExactInputs
        );
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(
            ExtractRequest::decode(&trailing).unwrap_err(),
            ProtocolError::TrailingBytes
        );
        assert!(ExtractRequest::from_cli(&["--ensure-exact-inputs".into()]).is_err());
    }

    #[test]
    fn exact_inputs_response_validation_keeps_transport_correlation_separate() {
        use std::os::unix::process::ExitStatusExt;
        let request = ExtractRequest {
            fields: vec![Field::EnsureExactInputs(ExactInputsRequest {
                root: "a".repeat(64),
                acquisition: ExactInputsAcquisition::Resident("realized".into()),
            })],
            ..ExtractRequest::default()
        };
        let frame = |tag: u8, root: &str, reference: Option<&str>| {
            let mut bytes = b"TPEXO002".to_vec();
            bytes.push(tag);
            push_frame(&mut bytes, OsStr::new(root));
            if let Some(reference) = reference {
                push_frame(&mut bytes, OsStr::new(reference));
            }
            bytes
        };
        let mut output = std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: frame(1, &"a".repeat(64), None),
            stderr: Vec::new(),
        };
        assert!(request.validate_exact_inputs_response(&output).is_ok());
        for bytes in [
            frame(1, &"b".repeat(64), None),
            frame(0, &"b".repeat(64), Some("realized")),
        ] {
            output.stdout = bytes;
            assert_eq!(
                request.validate_exact_inputs_response(&output),
                Err(ProtocolError::InvalidExactInputs)
            );
        }
        output.stdout = frame(0, &"a".repeat(64), Some("realized"));
        assert!(request.validate_exact_inputs_response(&output).is_ok());
        output.stdout = b"human captured output".to_vec();
        assert!(request.validate_exact_inputs_response(&output).is_err());
        // Ordinary reports and unsuccessful diagnostic responses keep their
        // existing machine formats; validation never parses a human channel.
        assert!(ExtractRequest::default()
            .validate_exact_inputs_response(&output)
            .is_ok());
        output.status = std::process::ExitStatus::from_raw(1 << 8);
        assert!(request.validate_exact_inputs_response(&output).is_ok());
    }

    #[test]
    fn exact_inputs_outcomes_match_closed_wire_and_reject_uncertain_frames() {
        let cases = [
            ("545045584f303032004000000061616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161080000007265616c697a6564", EnsureExactInputsOutcome::Ready { world: "a".repeat(64), realization: "realized".into() }),
            ("545045584f303032014000000061616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161", EnsureExactInputsOutcome::NeedsAcquisition("a".repeat(64))),
            ("545045584f3030320200", EnsureExactInputsOutcome::Rejected(ExactInputsRejection::UnsupportedOperation)),
            ("545045584f3030320201", EnsureExactInputsOutcome::Rejected(ExactInputsRejection::InvalidRealization)),
            ("545045584f3030320202", EnsureExactInputsOutcome::Rejected(ExactInputsRejection::InvalidRoot)),
            ("545045584f3030320203", EnsureExactInputsOutcome::Rejected(ExactInputsRejection::AcquisitionRejected)),
        ];
        for (hex, expected) in cases {
            let bytes = unhex(hex).unwrap();
            assert_eq!(EnsureExactInputsOutcome::decode(&bytes).unwrap(), expected);
            for end in 0..bytes.len() {
                assert!(EnsureExactInputsOutcome::decode(&bytes[..end]).is_err());
            }
            let mut trailing = bytes;
            trailing.push(0);
            assert_eq!(
                EnsureExactInputsOutcome::decode(&trailing).unwrap_err(),
                ProtocolError::TrailingBytes
            );
        }
        for hex in [
            "545045584f30303203",
            "545045584f3030320204",
            "545045584f3030320000000000",
            "545045584f303032010100000000",
            "545045584f3030310200",
        ] {
            assert!(EnsureExactInputsOutcome::decode(&unhex(hex).unwrap()).is_err());
        }
        for reference in ["", "realized\0other"] {
            let mut bytes = b"TPEXO002\0".to_vec();
            push_frame(&mut bytes, OsStr::new(&"a".repeat(64)));
            push_frame(&mut bytes, OsStr::new(reference));
            assert_eq!(
                EnsureExactInputsOutcome::decode(&bytes).unwrap_err(),
                ProtocolError::InvalidExactInputs
            );
        }
    }

    /// The Rust worker request flag must match the exact Haskell encoder
    /// literal — a version bump on either side without the other would make
    /// `decode_worker_argv` reject every request from a compiled worker.
    #[test]
    fn worker_request_flag_matches_the_haskell_encoder() {
        let haskell = include_str!("../../../bridge/haskell/src/Tidepool/ExtractRequest.hs");
        let version: u32 = haskell.lines().find_map(|line| line.strip_prefix("workerRequestVersion = ")).expect("owning worker grammar version").parse().unwrap();
        assert_eq!(format!("--worker-request-v{version}"), WORKER_REQUEST_FLAG);
        assert_eq!(format!("TPREQ{version:03}").as_bytes(), MAGIC);
    }
}
