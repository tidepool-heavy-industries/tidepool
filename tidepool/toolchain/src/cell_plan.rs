//! Parser-owned ordered notebook plans. These capabilities identify source
//! items and compiler inputs; they confer no checked types or live values.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ciborium::value::Value;
use sha2::{Digest, Sha256};

use crate::checked_cell::CellRecipe;
use crate::CompileError;

const RECEIPT_LIMIT: u64 = 8 << 20;
const ITEM_LIMIT: usize = 10_000;

#[derive(Debug)]
enum CellPlanInputRejection {
    InputBound,
    SearchRoot,
    ReservedOwners,
    InjectedInventory,
}

impl CellPlanInputRejection {
    fn error(self) -> CompileError {
        CompileError::InputRejected(vec![crate::diag::ExtractDiag {
            span: None,
            severity: crate::diag::DiagnosticSeverity::Error,
            message: format!("ordered parser input rejected: {self:?}"),
        }])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParsedCellPlanKind {
    Prologue,
    Declaration,
    Bind,
    Expression,
}

/// The owning GHC parser's native bind statement form. This observation
/// comes from the same AST as item classification, never source text matching.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParsedCellBindingForm {
    Action,
    Let,
    Recursive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParsedCellPlanSpan {
    start_line: usize,
    start_column: usize,
    end_line: usize,
    end_column: usize,
}

impl ParsedCellPlanSpan {
    pub fn start_line(&self) -> usize {
        self.start_line
    }
    pub fn start_column(&self) -> usize {
        self.start_column
    }
    pub fn end_line(&self) -> usize {
        self.end_line
    }
    pub fn end_column(&self) -> usize {
        self.end_column
    }
}

#[derive(Debug)]
pub struct ParsedCellPlanItem {
    index: usize,
    kind: ParsedCellPlanKind,
    binding_form: Option<ParsedCellBindingForm>,
    binders: Vec<String>,
    source: String,
    span: ParsedCellPlanSpan,
    source_ordinals: Vec<usize>,
}

impl ParsedCellPlanItem {
    pub fn index(&self) -> usize {
        self.index
    }
    pub fn kind(&self) -> ParsedCellPlanKind {
        self.kind
    }
    pub fn binding_form(&self) -> Option<ParsedCellBindingForm> {
        self.binding_form
    }
    pub fn binders(&self) -> &[String] {
        &self.binders
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn span(&self) -> &ParsedCellPlanSpan {
        &self.span
    }
    pub fn source_ordinals(&self) -> &[usize] {
        &self.source_ordinals
    }
}

/// Issued only by the owning parser endpoint after its bounded receipt is
/// bound to the exact source, templates, search roots and injected inventory.
/// Runtime reservations retain this plan before authoritative checking.
#[derive(Debug)]
pub struct ParsedCellPlan {
    specification: Arc<CellRecipe>,
    include_paths: Vec<PathBuf>,
    producer_sha256: [u8; 32],
    items: Vec<ParsedCellPlanItem>,
    receipt: Arc<[u8]>,
    observations: Arc<[u8]>,
    digest: [u8; 32],
}

impl ParsedCellPlan {
    /// Immutable source recipe bound by the parser receipt and retained by
    /// runtime admission. Consumers derive compiler requests from this owner.
    pub fn specification(&self) -> &Arc<CellRecipe> {
        &self.specification
    }

    pub fn specification_digest(&self) -> [u8; 32] {
        self.specification.specification_digest()
    }
    pub fn include_paths(&self) -> &[PathBuf] {
        &self.include_paths
    }
    pub fn injected_modules(&self) -> &[String] {
        self.specification.injected_modules()
    }
    pub fn items(&self) -> &[ParsedCellPlanItem] {
        &self.items
    }
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
    pub fn producer_sha256(&self) -> [u8; 32] {
        self.producer_sha256
    }
    pub fn receipt(&self) -> &[u8] {
        &self.receipt
    }
    pub fn observations(&self) -> &[u8] {
        &self.observations
    }
}

/// A service parser receipt bound to a held logical world and authored recipe.
/// It carries no runtime publication or checked value permission.
#[derive(Debug)]
pub struct CompilerWorldCellPlan {
    world: Arc<crate::compiler_world::CompilerWorld>,
    recipe: Arc<tidepool_extract_cmd::WorldCellRecipe>,
    operand: Option<Arc<crate::compiler_world::CompilerWorldCellOperand>>,
    producer_sha256: [u8; 32],
    items: Vec<ParsedCellPlanItem>,
    expression_names: WorldExpressionNames,
    receipt: Arc<[u8]>,
    observations: Arc<[u8]>,
    digest: [u8; 32],
    allocation: Arc<()>,
    issued: AdmissionIssueGate,
}

/// Names observed fresh by the held compiler's actual lexical TYPE scope.
/// This immutable projection reserves no generation or runtime value.
#[derive(Debug)]
struct WorldExpressionNames(std::collections::BTreeMap<usize, String>);

/// One-shot issuance for a live parser plan. A plan validates its requested
/// slots first, then claims this gate exactly once, so one plan authorises one
/// execution and one generation range.
#[derive(Debug, Default)]
struct AdmissionIssueGate(AtomicBool);

impl AdmissionIssueGate {
    fn claim(&self) -> Result<(), CompileError> {
        self.0
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| failure("world cell plan already issued its admission token"))
    }
}

impl CompilerWorldCellPlan {
    pub fn operand(&self) -> Option<&Arc<crate::compiler_world::CompilerWorldCellOperand>> {
        self.operand.as_ref()
    }
    pub(crate) fn recipe_owned(&self) -> Arc<tidepool_extract_cmd::WorldCellRecipe> {
        Arc::clone(&self.recipe)
    }
    pub(crate) fn receipt_owned(&self) -> Arc<[u8]> {
        Arc::clone(&self.receipt)
    }
    pub fn world(&self) -> &crate::compiler_world::CompilerWorld {
        &self.world
    }
    pub fn world_owned(&self) -> &Arc<crate::compiler_world::CompilerWorld> {
        &self.world
    }
    pub fn recipe(&self) -> &tidepool_extract_cmd::WorldCellRecipe {
        &self.recipe
    }
    pub fn items(&self) -> &[ParsedCellPlanItem] {
        &self.items
    }
    /// Read the compiler-issued name; allocation and checked-value authority
    /// remain with this plan's one-shot runtime reservation.
    pub fn expression_observation_name(&self, ordinal: usize) -> Option<&str> {
        self.expression_names.0.get(&ordinal).map(String::as_str)
    }
    pub fn receipt(&self) -> &[u8] {
        &self.receipt
    }
    pub fn observations(&self) -> &[u8] {
        &self.observations
    }
    pub fn producer_sha256(&self) -> &[u8; 32] {
        &self.producer_sha256
    }
    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }
}

/// Runtime reservation data paired with one actual world parser plan. This
/// carries expected identities only; checked output and runtime settlement
/// remain separate issuers.
#[derive(Debug)]
pub struct CompilerWorldCellReservation {
    defining_scope: u64,
    plan: Arc<CompilerWorldCellPlan>,
    admission_digest: [u8; 32],
    slots: Arc<[crate::checked_cell::CheckedPlannedCellSlot]>,
    expected_bindings: Arc<[Vec<(String, tidepool_repr::SessionVarId)>]>,
}

/// The exact runtime facts one source-admission owner binds to a world parser
/// plan. Callers supply facts; the plan owner derives the admission digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldCellAdmissionSubject {
    owner_identity: [u8; 16],
    owner_epoch: u64,
    session: u64,
    scope: u64,
    authorization: Arc<[Arc<[u8]>]>,
}

impl WorldCellAdmissionSubject {
    /// Frame the source-admission owner's identity, epoch and authorization
    /// facts inside the compiler transaction that owner actually issued.
    /// Holding that transaction's origin is the issuing capability: code
    /// outside the owner's active transaction cannot construct a subject, and
    /// a subject never carries a digest.
    pub fn for_source_transaction(
        origin: &tidepool_extract_cmd::CompilerTransactionOrigin,
        owner_identity: [u8; 16],
        owner_epoch: u64,
        session: u64,
        scope: u64,
        authorization: impl IntoIterator<Item = Vec<u8>>,
    ) -> Result<Self, CompileError> {
        if tidepool_extract_cmd::CompilerTransactionOrigin::current().as_ref() != Some(origin) {
            return Err(failure(
                "world cell admission requires its issuing source transaction",
            ));
        }
        Self::new(owner_identity, owner_epoch, session, scope, authorization)
    }

    pub(crate) fn new(
        owner_identity: [u8; 16],
        owner_epoch: u64,
        session: u64,
        scope: u64,
        authorization: impl IntoIterator<Item = Vec<u8>>,
    ) -> Result<Self, CompileError> {
        if owner_identity == [0; 16] {
            return Err(failure("world cell admission has no issuing source owner"));
        }
        Ok(Self {
            owner_identity,
            owner_epoch,
            session,
            scope,
            authorization: authorization
                .into_iter()
                .map(Arc::from)
                .collect::<Vec<_>>()
                .into(),
        })
    }
}

/// Issued only by the parser plan's owner after it validates the exact slots
/// and binders. A token is bound to the one parser-plan allocation that
/// admitted the receipt and is consumed by `CompilerWorldCellPlan::reserve`;
/// a digest, a foreign allocation's token or a caller-built value is not
/// admission.
#[derive(Debug)]
pub struct WorldCellAdmissionToken {
    defining_scope: u64,
    allocation: Arc<()>,
    admission_digest: [u8; 32],
    slots: Arc<[crate::checked_cell::CheckedPlannedCellSlot]>,
    expected_bindings: Arc<[Vec<(String, tidepool_repr::SessionVarId)>]>,
}

/// Derive one admission token from an already admitted parser plan. The
/// allocation witness names the plan that will accept it, and slot kinds,
/// generations and binders are validated before the token exists.
fn issue_world_cell_admission(
    world: &[u8; 32],
    producer_sha256: &[u8; 32],
    plan_digest: &[u8; 32],
    allocation: &Arc<()>,
    items: &[ParsedCellPlanItem],
    expression_names: &WorldExpressionNames,
    subject: WorldCellAdmissionSubject,
    slots: Vec<crate::checked_cell::CheckedPlannedCellSlot>,
    expected_bindings: Vec<Vec<(String, tidepool_repr::SessionVarId)>>,
) -> Result<WorldCellAdmissionToken, CompileError> {
    let admission_digest = world_cell_admission_digest(
        world,
        producer_sha256,
        plan_digest,
        &subject,
        &slots,
        &expected_bindings,
    );
    if admission_digest == [0; 32] {
        return Err(failure("issued world cell admission is empty"));
    }
    validate_world_cell_reservations(items, expression_names, &slots, &expected_bindings)?;
    Ok(WorldCellAdmissionToken {
        defining_scope: subject.scope,
        allocation: Arc::clone(allocation),
        admission_digest,
        slots: slots.into(),
        expected_bindings: expected_bindings.into(),
    })
}

/// Consume a token against the plan allocation named by its issuer. This is
/// the only path that yields reservation expectations; an unissued or foreign
/// token has no matching allocation witness.
fn consume_world_cell_admission(
    allocation: &Arc<()>,
    token: WorldCellAdmissionToken,
) -> Result<
    (
        Arc<[crate::checked_cell::CheckedPlannedCellSlot]>,
        Arc<[Vec<(String, tidepool_repr::SessionVarId)>]>,
        [u8; 32],
    ),
    CompileError,
> {
    if !Arc::ptr_eq(allocation, &token.allocation) {
        return Err(failure(
            "world cell admission token belongs to another plan",
        ));
    }
    Ok((token.slots, token.expected_bindings, token.admission_digest))
}

/// Validate the requested reservations, then claim the plan's single issue
/// and derive the token. Validation runs before the claim so a refused issue
/// leaves the plan unburned; `CompilerWorldCellPlan::issue_admission_token`
/// is the only production caller.
fn claim_and_issue_world_cell_admission(
    issued: &AdmissionIssueGate,
    world_logical_id: &[u8; 32],
    producer_sha256: &[u8; 32],
    plan_digest: &[u8; 32],
    allocation: &Arc<()>,
    items: &[ParsedCellPlanItem],
    expression_names: &WorldExpressionNames,
    subject: WorldCellAdmissionSubject,
    slots: Vec<crate::checked_cell::CheckedPlannedCellSlot>,
    expected_bindings: Vec<Vec<(String, tidepool_repr::SessionVarId)>>,
) -> Result<WorldCellAdmissionToken, CompileError> {
    validate_world_cell_reservations(items, expression_names, &slots, &expected_bindings)?;
    issued.claim()?;
    issue_world_cell_admission(
        world_logical_id,
        producer_sha256,
        plan_digest,
        allocation,
        items,
        expression_names,
        subject,
        slots,
        expected_bindings,
    )
}

impl CompilerWorldCellPlan {
    /// Issue the one admission token for this exact plan. Slot kinds,
    /// generations and binders are validated first, so a refused issue does
    /// not burn the plan; only a valid request can claim the plan's single
    /// issue. A second issue on the same live plan is refused, so one plan
    /// authorises at most one execution and one generation range.
    pub fn issue_admission_token(
        self: &Arc<Self>,
        subject: WorldCellAdmissionSubject,
        slots: Vec<crate::checked_cell::CheckedPlannedCellSlot>,
        expected_bindings: Vec<Vec<(String, tidepool_repr::SessionVarId)>>,
    ) -> Result<WorldCellAdmissionToken, CompileError> {
        claim_and_issue_world_cell_admission(
            &self.issued,
            self.world().logical_id(),
            self.producer_sha256(),
            self.digest(),
            &self.allocation,
            &self.items,
            &self.expression_names,
            subject,
            slots,
            expected_bindings,
        )
    }

    /// Consume one token issued for this exact plan allocation.
    pub fn reserve(
        self: &Arc<Self>,
        token: WorldCellAdmissionToken,
    ) -> Result<Arc<CompilerWorldCellReservation>, CompileError> {
        let defining_scope = token.defining_scope;
        let (slots, expected_bindings, admission_digest) =
            consume_world_cell_admission(&self.allocation, token)?;
        Ok(Arc::new(CompilerWorldCellReservation {
            defining_scope,
            plan: Arc::clone(self),
            admission_digest,
            slots,
            expected_bindings,
        }))
    }
}

fn world_cell_admission_digest(
    world_logical_id: &[u8; 32],
    producer_sha256: &[u8; 32],
    plan_digest: &[u8; 32],
    subject: &WorldCellAdmissionSubject,
    slots: &[crate::checked_cell::CheckedPlannedCellSlot],
    expected_bindings: &[Vec<(String, tidepool_repr::SessionVarId)>],
) -> [u8; 32] {
    use crate::checked_cell::CheckedPlannedCellSlot as Slot;
    let mut digest = blake3::Hasher::new();
    let mut frame = |bytes: &[u8]| {
        digest.update(&(bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    };
    frame(b"TidepoolRuntimeWorldCellAdmission1");
    frame(&subject.owner_identity);
    frame(&subject.owner_epoch.to_le_bytes());
    frame(&subject.session.to_le_bytes());
    frame(&subject.scope.to_le_bytes());
    frame(world_logical_id);
    frame(producer_sha256);
    frame(plan_digest);
    for (slot, bindings) in slots.iter().zip(expected_bindings) {
        let (kind, generation) = match slot {
            Slot::Prologue { declaration } => (0, *declaration),
            Slot::Declaration { declaration } => (1, *declaration),
            Slot::Bind { value } => (2, *value),
            Slot::Expression { capture, .. } => (3, *capture),
        };
        frame(&[kind]);
        frame(&generation.to_le_bytes());
        frame(&(bindings.len() as u64).to_le_bytes());
        for (name, id) in bindings {
            frame(name.as_bytes());
            frame(&id.raw().to_le_bytes());
        }
    }
    for authorization in subject.authorization.iter() {
        frame(authorization);
    }
    *digest.finalize().as_bytes()
}

fn validate_world_cell_reservations(
    items: &[ParsedCellPlanItem],
    expression_names: &WorldExpressionNames,
    slots: &[crate::checked_cell::CheckedPlannedCellSlot],
    expected_bindings: &[Vec<(String, tidepool_repr::SessionVarId)>],
) -> Result<(), CompileError> {
    use crate::checked_cell::CheckedPlannedCellSlot as Slot;
    if slots.len() != items.len() || expected_bindings.len() != items.len() {
        return Err(failure("world cell reservation differs from parser plan"));
    }
    let mut generations = std::collections::HashSet::new();
    let mut identifiers = std::collections::BTreeSet::new();
    for ((item, slot), bindings) in items.iter().zip(slots).zip(expected_bindings) {
        let (generation, names): (u64, Vec<&str>) = match (item.kind(), slot) {
            (ParsedCellPlanKind::Prologue, Slot::Prologue { declaration })
            | (ParsedCellPlanKind::Declaration, Slot::Declaration { declaration }) => {
                if !bindings.is_empty() {
                    return Err(failure("declaration has value reservations"));
                }
                (*declaration, Vec::new())
            }
            (ParsedCellPlanKind::Bind, Slot::Bind { value }) => {
                (*value, item.binders().iter().map(String::as_str).collect())
            }
            (
                ParsedCellPlanKind::Expression,
                Slot::Expression {
                    capture,
                    observation_name,
                },
            ) if expression_names.0.get(&item.index()) == Some(observation_name) => {
                (*capture, vec![observation_name.as_str()])
            }
            _ => return Err(failure("world cell slot differs from parsed item kind")),
        };
        let module = match slot {
            Slot::Prologue { .. } | Slot::Declaration { .. } => {
                tidepool_repr::SessionModule::lib(tidepool_repr::Generation(generation))
            }
            Slot::Bind { .. } | Slot::Expression { .. } => {
                tidepool_repr::SessionModule::val(tidepool_repr::Generation(generation))
            }
        };
        if generation == 0
            || !generations.insert(module)
            || names.len() != bindings.len()
            || names
                .iter()
                .zip(bindings)
                .any(|(name, (actual, id))| *name != actual || !identifiers.insert(id.raw()))
        {
            return Err(failure(
                "world cell binder or generation reservation differs",
            ));
        }
    }
    Ok(())
}

impl CompilerWorldCellReservation {
    /// Runtime-value placement retained from the same subject hashed into this
    /// reservation. The held World's query scope may be its public ancestor.
    pub fn defining_scope(&self) -> u64 {
        self.defining_scope
    }
    pub fn plan(&self) -> &Arc<CompilerWorldCellPlan> {
        &self.plan
    }
    pub fn admission_digest(&self) -> &[u8; 32] {
        &self.admission_digest
    }
    pub fn slots(&self) -> &[crate::checked_cell::CheckedPlannedCellSlot] {
        &self.slots
    }
    pub fn expected_bindings(&self) -> &[Vec<(String, tidepool_repr::SessionVarId)>] {
        &self.expected_bindings
    }
}

pub(crate) fn admit_world_plan(
    world: Arc<crate::compiler_world::CompilerWorld>,
    recipe: Arc<tidepool_extract_cmd::WorldCellRecipe>,
    operand: Option<Arc<crate::compiler_world::CompilerWorldCellOperand>>,
    producer: [u8; 32],
    realization: &str,
    receipt: Vec<u8>,
) -> Result<Arc<CompilerWorldCellPlan>, CompileError> {
    let facts = admit_world_parser_receipt(
        world.logical_id(),
        &recipe,
        producer,
        operand.as_ref().map(|operand| operand.request().sha256),
        &receipt,
    )?;
    if facts.realization != realization {
        return Err(failure(
            "world parser receipt belongs to another realization",
        ));
    }
    Ok(Arc::new(CompilerWorldCellPlan {
        world,
        recipe,
        operand,
        producer_sha256: producer,
        items: facts.items,
        expression_names: facts.expression_names,
        observations: facts.observations,
        receipt: receipt.into(),
        digest: facts.digest,
        allocation: facts.allocation,
        issued: AdmissionIssueGate::default(),
    }))
}

struct WorldParserFacts {
    items: Vec<ParsedCellPlanItem>,
    expression_names: WorldExpressionNames,
    realization: String,
    observations: Arc<[u8]>,
    digest: [u8; 32],
    allocation: Arc<()>,
}

fn admit_world_parser_receipt(
    expected_world: &[u8; 32],
    recipe: &tidepool_extract_cmd::WorldCellRecipe,
    producer: [u8; 32],
    operand_sha256: Option<[u8; 32]>,
    receipt: &[u8],
) -> Result<WorldParserFacts, CompileError> {
    let value = decode(&receipt)?;
    let fields = crate::checked_cell::row(&value, 11)?;
    let hash = |value: &Value| -> Result<[u8; 32], CompileError> {
        match value {
            Value::Bytes(bytes) => bytes.as_slice().try_into().map_err(failure),
            _ => Err(failure("world parser hash")),
        }
    };
    if text(&fields[0])? != "TPCELLPLAN6"
        || hash(&fields[1])? != *expected_world
        || hash(&fields[2])? != producer
        || hash(&fields[3])? != <[u8; 32]>::from(Sha256::digest(recipe.source.as_bytes()))
        || hash(&fields[4])? != <[u8; 32]>::from(Sha256::digest(recipe.template.as_bytes()))
    {
        return Err(failure(
            "world parser source, template, world or producer differs",
        ));
    }
    match (operand_sha256, &fields[8]) {
        (None, Value::Null) => {}
        (Some(expected), value) if hash(value)? == expected => {}
        _ => return Err(failure("world parser operand descriptor differs")),
    }
    let templates = list(&fields[5], 256)?;
    if templates.len() != recipe.turn_templates.len() {
        return Err(failure("world parser template count"));
    }
    for (value, (kind, source)) in templates.iter().zip(&recipe.turn_templates) {
        let row = crate::checked_cell::row(value, 2)?;
        if text(&row[0])? != kind
            || hash(&row[1])? != <[u8; 32]>::from(Sha256::digest(source.as_bytes()))
        {
            return Err(failure("world parser template differs"));
        }
    }
    let Value::Bytes(observations) = &fields[6] else {
        return Err(failure("world parser observations"));
    };
    let items = admit_parser_items(observations, &fields[7])?;
    let realization = text(&fields[9])?.to_owned();
    if realization.is_empty() || realization.len() > 65536 || realization.contains('\0') {
        return Err(failure("world parser realization"));
    }
    let expression_names = admit_world_expression_names(&items, &fields[10])?;
    let mut digest = Sha256::new();
    digest.update(b"TidepoolWorldCellPlan1\0");
    digest.update(&receipt);
    Ok(WorldParserFacts {
        items,
        expression_names,
        realization,
        observations: observations.clone().into(),
        digest: digest.finalize().into(),
        allocation: Arc::new(()),
    })
}

fn admit_world_expression_names(
    items: &[ParsedCellPlanItem],
    value: &Value,
) -> Result<WorldExpressionNames, CompileError> {
    let mut expression_names = std::collections::BTreeMap::new();
    let mut names = std::collections::BTreeSet::new();
    let mut last_ordinal = None;
    for value in list(value, items.len())? {
        let row = crate::checked_cell::row(value, 2)?;
        let ordinal = number(&row[0])?;
        let name = text(&row[1])?;
        if last_ordinal.is_some_and(|previous| ordinal <= previous)
            || items
                .get(ordinal)
                .is_none_or(|item| item.kind() != ParsedCellPlanKind::Expression)
            || !name.starts_with("observation")
            || name.len() > 65536
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            || names.contains(name)
            || names.contains(&format!("{name}_unit"))
        {
            return Err(failure("world parser expression name differs"));
        }
        names.insert(format!("{name}_unit"));
        names.insert(name.to_owned());
        expression_names.insert(ordinal, name.to_owned());
        last_ordinal = Some(ordinal);
    }
    if expression_names.len()
        != items
            .iter()
            .filter(|item| item.kind() == ParsedCellPlanKind::Expression)
            .count()
    {
        return Err(failure(
            "world parser expression names do not cover its items",
        ));
    }
    Ok(WorldExpressionNames(expression_names))
}

pub(crate) fn parse(
    specification: Arc<CellRecipe>,
    include_paths: &[PathBuf],
    settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
) -> Result<Arc<ParsedCellPlan>, CompileError> {
    if specification.cell_source().len() > (2 << 20)
        || specification.template_source().len() > (2 << 20)
        || specification.turn_templates().len() > 256
        || specification
            .turn_templates()
            .iter()
            .any(|(_, source)| source.len() > (2 << 20))
    {
        return Err(CellPlanInputRejection::InputBound.error());
    }
    if include_paths.len() > 4096
        || include_paths
            .iter()
            .any(|path| !path.is_absolute() || path.to_str().is_none_or(|path| path.len() > 65536))
    {
        return Err(CellPlanInputRejection::SearchRoot.error());
    }
    if specification.injected_modules().len() > 4096
        || specification
            .injected_modules()
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != specification.injected_modules().len()
    {
        return Err(CellPlanInputRejection::InjectedInventory.error());
    }
    let scratch = Arc::new(crate::artifacts::compiler_scratch_directory()?);
    let source = scratch.path().join("cell.txt");
    let template = scratch.path().join("cell-template.hs");
    let output = scratch.path().join("cell-plan.cbor");
    std::fs::write(&source, specification.cell_source())?;
    std::fs::write(&template, specification.template_source())?;
    let mut command =
        tidepool_extract_cmd::ExtractCmd::new().map_err(|error| CompileError::Io(error.into()))?;
    command
        .input(&source)
        .cell_plan()
        .cell_template(&template)
        .cell_out(&output)
        .includes(include_paths)
        .inject_vals(specification.injected_modules());
    for (index, (kind, source)) in specification.turn_templates().iter().enumerate() {
        let path = scratch.path().join(format!("turn-template-{index}.hs"));
        std::fs::write(&path, source)?;
        command.turn_template(kind, &path);
    }
    let endpoint = command
        .bind()
        .map_err(|error| CompileError::Io(crate::extract_spawn_error(error.source)))?;
    let endpoint = crate::toolchain::AdmittedCompilerEndpoint::from_bound(endpoint)
        .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
    let producer = *endpoint.identity().producer_bytes();
    let diagnostics = crate::artifacts::CompilerDiagnosticCapture::start(scratch.path(), &command);
    let run =
        endpoint.execute_with_input_directories(&command, vec![scratch.clone()], |close| {
            settlement(close)
        })?;
    diagnostics.completed(scratch.path(), &command, run.success(), &run.output.stderr);
    crate::diag::decode_extract_result(run.success(), &run.output.stdout, &run.output.stderr)?;
    let receipt = crate::checked_cell::read(output, RECEIPT_LIMIT)?;
    admit(specification, include_paths, &producer, receipt)
}

fn admit(
    specification: Arc<CellRecipe>,
    include_paths: &[PathBuf],
    producer: &[u8; 32],
    receipt: Vec<u8>,
) -> Result<Arc<ParsedCellPlan>, CompileError> {
    let root = decode(&receipt)?;
    let fields = crate::checked_cell::row(&root, 8)?;
    if text(&fields[0])? != "TPCELLPLAN3"
        || text(&fields[1])? != crate::checked_cell::hash(specification.cell_source().as_bytes())
        || text(&fields[2])?
            != crate::checked_cell::hash(specification.template_source().as_bytes())
    {
        return Err(failure("source or template differs from parser receipt"));
    }
    let templates = list(&fields[3], 256)?;
    if templates.len() != specification.turn_templates().len() {
        return Err(failure("parser template count"));
    }
    for (row, (kind, source)) in templates.iter().zip(specification.turn_templates()) {
        let row = crate::checked_cell::row(row, 2)?;
        if text(&row[0])? != kind || text(&row[1])? != crate::checked_cell::hash(source.as_bytes())
        {
            return Err(failure("parser template recipe"));
        }
    }
    let paths = texts(&fields[4], 4096)?;
    if paths.len() != include_paths.len()
        || paths
            .iter()
            .zip(include_paths)
            .any(|(expected, actual)| Some(expected.as_str()) != actual.to_str())
        || texts(&fields[5], 4096)? != specification.injected_modules()
    {
        return Err(failure("parser search inputs or injected inventory"));
    }
    let Value::Bytes(observations) = &fields[6] else {
        return Err(failure("parser observations"));
    };
    let items = admit_parser_items(observations, &fields[7])?;
    let producer_sha256 =
        crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(producer)
            .sha256();
    let mut digest = Sha256::new();
    digest.update(b"TidepoolParsedCellPlan1");
    digest.update(producer_sha256);
    digest.update(specification.specification_digest());
    digest.update(&receipt);
    Ok(Arc::new(ParsedCellPlan {
        specification,
        include_paths: include_paths.to_vec(),
        producer_sha256,
        items,
        observations: observations.clone().into(),
        receipt: receipt.into(),
        digest: digest.finalize().into(),
    }))
}

fn admit_parser_items(
    observations: &[u8],
    binding_forms: &Value,
) -> Result<Vec<ParsedCellPlanItem>, CompileError> {
    let observations_value = decode(observations)?;
    let observed = crate::checked_cell::cell_observations(&observations_value)?;
    if !list(&observed[1], 0)?.is_empty()
        || text(&observed[2])? != ""
        || !list(&observed[4], 0)?.is_empty()
    {
        return Err(failure("parser cannot certify checked types"));
    }
    let prologue = crate::checked_cell::row(&observed[3], 2)?;
    for value in list(&prologue[0], ITEM_LIMIT)? {
        let row = crate::checked_cell::row(value, 3)?;
        if !matches!(text(&row[0])?, "language" | "options_ghc") {
            return Err(failure("parser pragma kind"));
        }
        span(&row[1])?;
        text(&row[2])?;
    }
    for value in list(&prologue[1], ITEM_LIMIT)? {
        let row = crate::checked_cell::row(value, 2)?;
        span(&row[0])?;
        text(&row[1])?;
    }
    let binding_forms = texts(binding_forms, ITEM_LIMIT)?;
    if binding_forms.len() != list(&observed[0], ITEM_LIMIT)?.len() {
        return Err(failure("parser binding form inventory differs"));
    }
    let mut items = Vec::new();
    let mut next_ordinal = 0;
    for (index, value) in list(&observed[0], ITEM_LIMIT)?.iter().enumerate() {
        let row = crate::checked_cell::row(value, 6)?;
        let verdict = crate::checked_cell::row(&row[3], 2)?;
        let binders = texts(&verdict[0], ITEM_LIMIT)?;
        if binders
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != binders.len()
        {
            return Err(failure("parser duplicate binder"));
        }
        for value in list(&verdict[1], ITEM_LIMIT)? {
            let row = list(value, 3)?;
            match row {
                [kind, name] if matches!(text(kind)?, "EValue" | "EPattern") => {
                    text(name)?;
                }
                [kind, name, children] if matches!(text(kind)?, "EType" | "EClass") => {
                    text(name)?;
                    texts(children, ITEM_LIMIT)?;
                }
                _ => return Err(failure("parser declaration export")),
            }
        }
        let prologue_only = match &row[5] {
            Value::Bool(value) => *value,
            _ => return Err(failure("parser prologue flag")),
        };
        let kind = match (text(&row[1])?, prologue_only) {
            ("decl", true) if index == 0 && binders.is_empty() => ParsedCellPlanKind::Prologue,
            ("decl", false) => ParsedCellPlanKind::Declaration,
            ("bind", false) => ParsedCellPlanKind::Bind,
            ("expr", false) if binders.is_empty() => ParsedCellPlanKind::Expression,
            _ => return Err(failure("parser item kind")),
        };
        let binding_form = match (kind, binding_forms[index].as_str()) {
            (ParsedCellPlanKind::Bind, "action") => Some(ParsedCellBindingForm::Action),
            (ParsedCellPlanKind::Bind, "let") => Some(ParsedCellBindingForm::Let),
            (ParsedCellPlanKind::Bind, "recursive") => Some(ParsedCellBindingForm::Recursive),
            (
                ParsedCellPlanKind::Prologue
                | ParsedCellPlanKind::Declaration
                | ParsedCellPlanKind::Expression,
                "none",
            ) => None,
            _ => return Err(failure("parser binding form differs from item kind")),
        };
        let mut source_ordinals = Vec::new();
        for value in list(&row[4], ITEM_LIMIT)? {
            let row = crate::checked_cell::row(value, 3)?;
            let ordinal = number(&row[0])?;
            if ordinal != next_ordinal {
                return Err(failure("parser source ordinal sequence"));
            }
            let expected_kind = match kind {
                ParsedCellPlanKind::Prologue | ParsedCellPlanKind::Declaration => "decl",
                ParsedCellPlanKind::Bind => "bind",
                ParsedCellPlanKind::Expression => "expr",
            };
            if text(&row[2])? != expected_kind {
                return Err(failure("parser source item kind"));
            }
            span(&row[1])?;
            next_ordinal += 1;
            source_ordinals.push(ordinal);
        }
        if source_ordinals.is_empty() || next_ordinal > ITEM_LIMIT {
            return Err(failure("parser item source ordinals"));
        }
        let source = text(&row[2])?.to_owned();
        if kind == ParsedCellPlanKind::Prologue
            && (!source.is_empty() || !list(&verdict[1], 0)?.is_empty())
        {
            return Err(failure("parser prologue has a declaration body"));
        }
        items.push(ParsedCellPlanItem {
            index,
            kind,
            binding_form,
            binders,
            source,
            span: span(&row[0])?,
            source_ordinals,
        });
    }
    Ok(items)
}

fn decode(bytes: &[u8]) -> Result<Value, CompileError> {
    if bytes.len() as u64 > RECEIPT_LIMIT {
        return Err(failure("parser receipt bound"));
    }
    let mut cursor = std::io::Cursor::new(bytes);
    let value = ciborium::de::from_reader_with_recursion_limit(&mut cursor, 32).map_err(failure)?;
    if cursor.position() != bytes.len() as u64 {
        return Err(failure("parser receipt trailing bytes"));
    }
    Ok(value)
}
fn text(value: &Value) -> Result<&str, CompileError> {
    crate::checked_cell::string(value)
}
fn list(value: &Value, bound: usize) -> Result<&[Value], CompileError> {
    match value {
        Value::Array(values) if values.len() <= bound => Ok(values),
        _ => Err(failure("parser array bound")),
    }
}
fn texts(value: &Value, bound: usize) -> Result<Vec<String>, CompileError> {
    list(value, bound)?
        .iter()
        .map(|value| text(value).map(str::to_owned))
        .collect()
}
fn number(value: &Value) -> Result<usize, CompileError> {
    match value {
        Value::Integer(value) => usize::try_from(*value).map_err(failure),
        _ => Err(failure("parser integer")),
    }
}
fn span(value: &Value) -> Result<ParsedCellPlanSpan, CompileError> {
    let row = crate::checked_cell::row(value, 4)?;
    let values = row.iter().map(number).collect::<Result<Vec<_>, _>>()?;
    if values.contains(&0) || (values[0], values[1]) > (values[2], values[3]) {
        return Err(failure("parser source span"));
    }
    Ok(ParsedCellPlanSpan {
        start_line: values[0],
        start_column: values[1],
        end_line: values[2],
        end_column: values[3],
    })
}
fn failure(error: impl std::fmt::Display) -> CompileError {
    CompileError::ExtractFailed(format!("ordered parser receipt: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn encoded(value: &Value) -> Vec<u8> {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(value, &mut bytes).unwrap();
        bytes
    }
    fn fixture() -> (Arc<CellRecipe>, Vec<PathBuf>, Value) {
        let specification = Arc::new(
            CellRecipe::new(
                "let owned = (42 :: Int)",
                "compiler-template",
                vec![("bind".to_string(), "turn-template".to_string())],
                vec!["Tidepool.Session.Val.G7".to_string()],
            )
            .unwrap(),
        );
        let a = |values: Vec<Value>| Value::Array(values);
        let t = |value: &str| Value::Text(value.into());
        let span = a(vec![1.into(), 1.into(), 1.into(), 23.into()]);
        let item = a(vec![
            span.clone(),
            t("bind"),
            t(specification.cell_source()),
            a(vec![a(vec![t("owned")]), a(vec![])]),
            a(vec![a(vec![0.into(), span, t("bind")])]),
            Value::Bool(false),
        ]);
        let observations = a(vec![
            t("TPCELLOBSERVATIONS"),
            3.into(),
            a(vec![
                a(vec![item]),
                a(vec![]),
                t(""),
                a(vec![a(vec![]), a(vec![])]),
                a(vec![]),
            ]),
        ]);
        let receipt = a(vec![
            t("TPCELLPLAN3"),
            t(&crate::checked_cell::hash(
                specification.cell_source().as_bytes(),
            )),
            t(&crate::checked_cell::hash(
                specification.template_source().as_bytes(),
            )),
            a(vec![a(vec![
                t("bind"),
                t(&crate::checked_cell::hash(b"turn-template")),
            ])]),
            a(vec![t("/original")]),
            a(vec![t("Tidepool.Session.Val.G7")]),
            Value::Bytes(encoded(&observations)),
            a(vec![t("let")]),
        ]);
        (specification, vec![PathBuf::from("/original")], receipt)
    }
    #[test]
    fn world_reservations_keep_declaration_and_value_generations_separate() {
        use crate::checked_cell::CheckedPlannedCellSlot as Slot;
        let item = |kind, binder: &str| ParsedCellPlanItem {
            index: 0,
            kind,
            binding_form: None,
            binders: if binder.is_empty() {
                Vec::new()
            } else {
                vec![binder.into()]
            },
            source: String::new(),
            span: ParsedCellPlanSpan {
                start_line: 1,
                start_column: 1,
                end_line: 1,
                end_column: 1,
            },
            source_ordinals: Vec::new(),
        };
        let declaration = item(ParsedCellPlanKind::Declaration, "");
        let value = item(ParsedCellPlanKind::Bind, "x");
        let bindings = vec![("x".into(), tidepool_repr::SessionVarId::from_extract(91))];
        assert!(validate_world_cell_reservations(
            &[declaration, value],
            &WorldExpressionNames(Default::default()),
            &[
                Slot::Declaration { declaration: 1 },
                Slot::Bind { value: 1 }
            ],
            &[Vec::new(), bindings],
        )
        .is_ok());
        assert!(validate_world_cell_reservations(
            &[
                item(ParsedCellPlanKind::Prologue, ""),
                item(ParsedCellPlanKind::Declaration, "")
            ],
            &WorldExpressionNames(Default::default()),
            &[
                Slot::Prologue { declaration: 1 },
                Slot::Declaration { declaration: 1 }
            ],
            &[Vec::new(), Vec::new()],
        )
        .is_err());
        assert!(validate_world_cell_reservations(
            &[
                item(ParsedCellPlanKind::Bind, "x"),
                item(ParsedCellPlanKind::Expression, "")
            ],
            &WorldExpressionNames(Default::default()),
            &[
                Slot::Bind { value: 1 },
                Slot::Expression {
                    capture: 1,
                    observation_name: "it".into()
                }
            ],
            &[
                vec![("x".into(), tidepool_repr::SessionVarId::from_extract(91))],
                vec![("it".into(), tidepool_repr::SessionVarId::from_extract(92))]
            ],
        )
        .is_err());
    }

    /// The world identity and parser facts `admit_world_plan` would retain
    /// for the `fixture()` recipe. The held `CompilerWorld` wrapper adds only
    /// `logical_id()` and a fresh issue gate.
    fn world_plan_fixture() -> (
        tidepool_extract_cmd::WorldCellRecipe,
        [u8; 32],
        [u8; 32],
        Value,
    ) {
        let (specification, _, legacy) = fixture();
        let recipe = tidepool_extract_cmd::WorldCellRecipe {
            source: specification.cell_source().to_owned(),
            template: specification.template_source().to_owned(),
            turn_templates: specification.turn_templates().to_vec(),
        };
        let producer =
            crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(&[1; 32])
                .sha256();
        let world: [u8; 32] = [9; 32];
        let hash = |bytes: &[u8]| Value::Bytes(Sha256::digest(bytes).to_vec());
        let fields = legacy.as_array().unwrap();
        let receipt = Value::Array(vec![
            Value::Text("TPCELLPLAN6".into()),
            Value::Bytes(world.to_vec()),
            Value::Bytes(producer.to_vec()),
            hash(recipe.source.as_bytes()),
            hash(recipe.template.as_bytes()),
            Value::Array(
                recipe
                    .turn_templates
                    .iter()
                    .map(|(kind, source)| {
                        Value::Array(vec![Value::Text(kind.clone()), hash(source.as_bytes())])
                    })
                    .collect(),
            ),
            fields[6].clone(),
            fields[7].clone(),
            Value::Null,
            Value::Text("held-world-realization".into()),
            Value::Array(Vec::new()),
        ]);
        (recipe, world, producer, receipt)
    }

    #[test]
    fn world_parser_receipt_binds_the_exact_optional_operand_descriptor() {
        let (recipe, world, producer, mut receipt) = world_plan_fixture();
        let expected = [37; 32];
        receipt.as_array_mut().unwrap()[8] = Value::Bytes(expected.to_vec());
        let bytes = encoded(&receipt);
        assert!(
            admit_world_parser_receipt(&world, &recipe, producer, Some(expected), &bytes).is_ok()
        );
        assert!(admit_world_parser_receipt(&world, &recipe, producer, None, &bytes).is_err());
        assert!(
            admit_world_parser_receipt(&world, &recipe, producer, Some([38; 32]), &bytes).is_err()
        );
        receipt.as_array_mut().unwrap()[8] = Value::Null;
        assert!(admit_world_parser_receipt(
            &world,
            &recipe,
            producer,
            Some(expected),
            &encoded(&receipt)
        )
        .is_err());
        let mut legacy = receipt.clone();
        let fields = legacy.as_array_mut().unwrap();
        fields.truncate(9);
        fields[0] = Value::Text("TPCELLPLAN5".into());
        assert!(
            admit_world_parser_receipt(&world, &recipe, producer, None, &encoded(&legacy)).is_err()
        );
    }

    #[test]
    fn world_expression_names_cover_exact_items_and_bind_reserved_slots() {
        let (recipe, world, producer, receipt) = world_plan_fixture();
        let facts = admit_world_parser_receipt(&world, &recipe, producer, None, &encoded(&receipt))
            .unwrap();
        let mut item = facts.items.into_iter().next().unwrap();
        item.kind = ParsedCellPlanKind::Expression;
        item.binding_form = None;
        item.binders.clear();
        let row = |ordinal: usize, name: &str| {
            Value::Array(vec![(ordinal as u64).into(), Value::Text(name.into())])
        };
        let rows = |values| Value::Array(values);
        let names = admit_world_expression_names(
            std::slice::from_ref(&item),
            &rows(vec![row(0, "observation0")]),
        )
        .unwrap();
        use crate::checked_cell::CheckedPlannedCellSlot as Slot;
        let bindings = [vec![(
            "observation0".into(),
            tidepool_repr::SessionVarId::from_extract(91),
        )]];
        assert!(validate_world_cell_reservations(
            std::slice::from_ref(&item),
            &names,
            &[Slot::Expression {
                capture: 7,
                observation_name: "observation0".into()
            }],
            &bindings
        )
        .is_ok());
        assert!(validate_world_cell_reservations(
            std::slice::from_ref(&item),
            &names,
            &[Slot::Expression {
                capture: 7,
                observation_name: "observation1".into()
            }],
            &bindings
        )
        .is_err());
        for invalid in [
            rows(vec![]),
            rows(vec![row(1, "observation0")]),
            rows(vec![row(0, "observation0"), row(0, "observation1")]),
            rows(vec![row(0, "another")]),
        ] {
            assert!(admit_world_expression_names(std::slice::from_ref(&item), &invalid).is_err());
        }
        let mut second =
            admit_world_parser_receipt(&world, &recipe, producer, None, &encoded(&receipt))
                .unwrap()
                .items
                .into_iter()
                .next()
                .unwrap();
        second.kind = ParsedCellPlanKind::Expression;
        second.binding_form = None;
        second.binders.clear();
        second.index = 1;
        let items = [item, second];
        for (first, next) in [
            ("observation0", "observation0"),
            ("observation0", "observation0_unit"),
            ("observation0_unit", "observation0"),
        ] {
            assert!(
                admit_world_expression_names(&items, &rows(vec![row(0, first), row(1, next)]))
                    .is_err()
            );
        }
        assert!(admit_world_expression_names(
            &items,
            &rows(vec![row(0, "observation0"), row(1, "observation1")])
        )
        .is_ok());
    }

    #[test]
    fn world_parser_receipt_binds_world_canonical_producer_and_authored_recipe() {
        let (recipe, world, producer, receipt) = world_plan_fixture();
        let (_, _, legacy) = fixture();
        let admitted =
            admit_world_parser_receipt(&world, &recipe, producer, None, &encoded(&receipt))
                .unwrap();
        assert_eq!(admitted.items[0].binders(), ["owned"]);
        // Runtime expectations remain data, but cannot substitute another
        // occurrence, item kind or reserved generation for this parser plan.
        use crate::checked_cell::CheckedPlannedCellSlot as Slot;
        let binding = vec![(
            "owned".into(),
            tidepool_repr::SessionVarId::from_extract(91),
        )];
        assert!(validate_world_cell_reservations(
            &admitted.items,
            &admitted.expression_names,
            &[Slot::Bind { value: 7 }],
            &[binding.clone()]
        )
        .is_ok());
        assert!(validate_world_cell_reservations(
            &admitted.items,
            &admitted.expression_names,
            &[Slot::Bind { value: 0 }],
            &[binding.clone()]
        )
        .is_err());
        assert!(validate_world_cell_reservations(
            &admitted.items,
            &admitted.expression_names,
            &[Slot::Declaration { declaration: 7 }],
            &[binding.clone()]
        )
        .is_err());
        assert!(validate_world_cell_reservations(
            &admitted.items,
            &admitted.expression_names,
            &[Slot::Bind { value: 7 }],
            &[vec![("other".into(), binding[0].1)]]
        )
        .is_err());
        assert!(validate_world_cell_reservations(
            &admitted.items,
            &admitted.expression_names,
            &[Slot::Bind { value: 7 }],
            &[Vec::new()]
        )
        .is_err());

        // The admission digest is derived from the owner facts, never chosen
        // by the caller. An owner-less subject is refused, and a different
        // source-admission owner produces a different authorization digest.
        let subject = |owner: [u8; 16]| {
            WorldCellAdmissionSubject::new(
                owner,
                3,
                4,
                5,
                [b"TidepoolPrivateExecutionAdmission1".to_vec()],
            )
            .unwrap()
        };
        let slots = [Slot::Bind { value: 7 }];
        let bindings = [binding.clone()];
        let digest = world_cell_admission_digest(
            &world,
            &producer,
            &admitted.digest,
            &subject([7; 16]),
            &slots,
            &bindings,
        );
        assert_ne!(digest, [0; 32]);
        assert_ne!(
            digest,
            world_cell_admission_digest(
                &world,
                &producer,
                &admitted.digest,
                &subject([8; 16]),
                &slots,
                &bindings,
            )
        );
        assert!(
            WorldCellAdmissionSubject::new([0; 16], 3, 4, 5, std::iter::empty::<Vec<u8>>(),)
                .is_err()
        );

        // A reservation exists only by consuming a token the plan owner
        // issued for this exact plan allocation. There is no constructor for
        // an unissued token and no raw-digest entry point; a token issued by
        // another plan allocation is refused.
        let token = issue_world_cell_admission(
            &world,
            &producer,
            &admitted.digest,
            &admitted.allocation,
            &admitted.items,
            &admitted.expression_names,
            subject([7; 16]),
            slots.to_vec(),
            bindings.to_vec(),
        )
        .expect("the plan owner issues admission for its own exact slots");
        let (reserved_slots, reserved_bindings, admission_digest) =
            consume_world_cell_admission(&admitted.allocation, token).unwrap();
        assert_ne!(admission_digest, [0; 32]);
        assert_eq!(reserved_slots.as_ref(), slots.as_slice());
        assert_eq!(reserved_bindings.as_ref(), bindings.as_slice());

        assert!(
            admit_world_parser_receipt(&world, &recipe, [1; 32], None, &encoded(&receipt)).is_err()
        );
        assert!(
            admit_world_parser_receipt(&[8; 32], &recipe, producer, None, &encoded(&receipt))
                .is_err()
        );
        for index in 0..8 {
            let mut changed = receipt.clone();
            changed.as_array_mut().unwrap()[index] = Value::Null;
            assert!(admit_world_parser_receipt(
                &world,
                &recipe,
                producer,
                None,
                &encoded(&changed)
            )
            .is_err());
        }
        let mut other = receipt.clone();
        let other_producer =
            crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(&[2; 32])
                .sha256();
        other.as_array_mut().unwrap()[2] = Value::Bytes(other_producer.to_vec());
        let other =
            admit_world_parser_receipt(&world, &recipe, other_producer, None, &encoded(&other))
                .unwrap();
        assert_ne!(admitted.digest, other.digest);
        // Authority is the plan allocation, not the echoed digest or the
        // subject facts: a token issued by the other plan cannot reserve this
        // one even though both name the same slots and owner.
        let foreign = issue_world_cell_admission(
            &world,
            &other_producer,
            &other.digest,
            &other.allocation,
            &other.items,
            &other.expression_names,
            subject([7; 16]),
            slots.to_vec(),
            bindings.to_vec(),
        )
        .unwrap();
        assert!(consume_world_cell_admission(&admitted.allocation, foreign).is_err());
        // Re-admitting the same receipt and producer yields a plan with the
        // same echoed digest but a distinct allocation. Because the token is
        // bound to the allocation, the equal digest does not transfer
        // authority to the derived plan.
        let derived =
            admit_world_parser_receipt(&world, &recipe, producer, None, &encoded(&receipt))
                .unwrap();
        assert_eq!(derived.digest, admitted.digest);
        assert!(!Arc::ptr_eq(&derived.allocation, &admitted.allocation));
        let token = issue_world_cell_admission(
            &world,
            &producer,
            &admitted.digest,
            &admitted.allocation,
            &admitted.items,
            &admitted.expression_names,
            subject([7; 16]),
            slots.to_vec(),
            bindings.to_vec(),
        )
        .unwrap();
        assert!(consume_world_cell_admission(&derived.allocation, token).is_err());
        assert!(
            admit_world_parser_receipt(&world, &recipe, producer, None, &encoded(&legacy)).is_err()
        );
    }

    #[test]
    fn live_plan_issues_its_admission_token_once() {
        let (recipe, world, producer, receipt) = world_plan_fixture();
        let admitted =
            admit_world_parser_receipt(&world, &recipe, producer, None, &encoded(&receipt))
                .unwrap();
        use crate::checked_cell::CheckedPlannedCellSlot as Slot;
        let subject = |owner: [u8; 16]| {
            WorldCellAdmissionSubject::new(
                owner,
                3,
                4,
                5,
                [b"TidepoolPrivateExecutionAdmission1".to_vec()],
            )
            .unwrap()
        };
        let bindings = || {
            vec![vec![(
                "owned".to_string(),
                tidepool_repr::SessionVarId::from_extract(91),
            )]]
        };
        let issue = |gate: &AdmissionIssueGate| {
            claim_and_issue_world_cell_admission(
                gate,
                &world,
                &producer,
                &admitted.digest,
                &admitted.allocation,
                &admitted.items,
                &admitted.expression_names,
                subject([7; 16]),
                vec![Slot::Bind { value: 7 }],
                bindings(),
            )
        };
        let issued = AdmissionIssueGate::default();
        assert!(issue(&issued).is_ok());
        // The same live plan refuses a second, independently well-formed token:
        // one plan authorises one execution and one generation range.
        assert!(issue(&issued).is_err());
        // A refused request does not burn a plan, because validation runs
        // before the claim.
        let unburned = AdmissionIssueGate::default();
        assert!(claim_and_issue_world_cell_admission(
            &unburned,
            &world,
            &producer,
            &admitted.digest,
            &admitted.allocation,
            &admitted.items,
            &admitted.expression_names,
            subject([7; 16]),
            vec![Slot::Bind { value: 0 }],
            bindings(),
        )
        .is_err());
        assert!(issue(&unburned).is_ok());
        // A fresh admission of the same receipt has its own allocation and
        // gate, exactly as `admit_world_plan` constructs it, and still issues.
        let fresh = admit_world_parser_receipt(&world, &recipe, producer, None, &encoded(&receipt))
            .unwrap();
        assert!(claim_and_issue_world_cell_admission(
            &AdmissionIssueGate::default(),
            &world,
            &producer,
            &fresh.digest,
            &fresh.allocation,
            &fresh.items,
            &fresh.expression_names,
            subject([7; 16]),
            vec![Slot::Bind { value: 7 }],
            bindings(),
        )
        .is_ok());
    }

    #[test]
    fn parser_receipt_binds_source_recipes_roots_and_unchecked_ordinals() {
        let (specification, include, receipt) = fixture();
        let plan = admit(specification.clone(), &include, &[1; 32], encoded(&receipt)).unwrap();
        let canonical =
            crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(&[1; 32])
                .sha256();
        assert_ne!(canonical, [1; 32]);
        assert_eq!(plan.producer_sha256(), canonical);
        let other = admit(specification.clone(), &include, &[2; 32], encoded(&receipt)).unwrap();
        assert_ne!(other.producer_sha256(), plan.producer_sha256());
        assert_ne!(other.digest(), plan.digest());
        assert_eq!(plan.items()[0].binders(), ["owned"]);
        assert_eq!(
            plan.items()[0].binding_form(),
            Some(ParsedCellBindingForm::Let)
        );
        for index in [0, 1, 2, 3, 4, 5, 7] {
            let mut edited = receipt.clone();
            edited.as_array_mut().unwrap()[index] = Value::Null;
            assert!(admit(specification.clone(), &include, &[1; 32], encoded(&edited)).is_err());
        }
        for forms in [vec![], vec!["none"], vec!["future"], vec!["let", "action"]] {
            let mut edited = receipt.clone();
            edited.as_array_mut().unwrap()[7] = Value::Array(
                forms
                    .into_iter()
                    .map(|form| Value::Text(form.into()))
                    .collect(),
            );
            assert!(admit(specification.clone(), &include, &[1; 32], encoded(&edited)).is_err());
        }
        let mut old_receipt = receipt.clone();
        old_receipt.as_array_mut().unwrap()[0] = Value::Text("TPCELLPLAN2".into());
        assert!(admit(
            specification.clone(),
            &include,
            &[1; 32],
            encoded(&old_receipt)
        )
        .is_err());
        let mut old_observations = receipt.clone();
        let envelope = decode(old_observations.as_array().unwrap()[6].as_bytes().unwrap()).unwrap();
        old_observations.as_array_mut().unwrap()[6] =
            Value::Bytes(encoded(&envelope.as_array().unwrap()[2]));
        assert!(admit(
            specification.clone(),
            &include,
            &[1; 32],
            encoded(&old_observations)
        )
        .is_err());
        let mut trailing = encoded(&receipt);
        trailing.push(0);
        assert!(admit(specification.clone(), &include, &[1; 32], trailing).is_err());
        let mut edited = receipt.clone();
        let bytes = edited.as_array_mut().unwrap()[6].as_bytes().unwrap();
        let mut observations = decode(bytes).unwrap();
        observations.as_array_mut().unwrap()[2]
            .as_array_mut()
            .unwrap()[1] = Value::Array(vec![Value::Null]);
        edited.as_array_mut().unwrap()[6] = Value::Bytes(encoded(&observations));
        assert!(admit(specification, &include, &[1; 32], encoded(&edited)).is_err());
    }
}
