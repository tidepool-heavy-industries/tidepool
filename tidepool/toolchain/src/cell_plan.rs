//! Parser-owned ordered notebook plans. These capabilities identify source
//! items and compiler inputs; they confer no checked types or live values.

use std::path::PathBuf;
use std::sync::Arc;

use ciborium::value::Value;
use sha2::{Digest, Sha256};

use crate::checked_cell::CheckedCellSpecification;
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
    specification: Arc<CheckedCellSpecification>,
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
    pub fn specification(&self) -> &Arc<CheckedCellSpecification> {
        &self.specification
    }

    pub fn specification_digest(&self) -> [u8; 32] {
        self.specification.specification_digest()
    }
    pub fn include_paths(&self) -> &[PathBuf] {
        &self.include_paths
    }
    pub fn injected_modules(&self) -> &[String] {
        &self.specification.injected_modules
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
    producer_sha256: [u8; 32],
    items: Vec<ParsedCellPlanItem>,
    receipt: Arc<[u8]>,
    observations: Arc<[u8]>,
    digest: [u8; 32],
}

impl CompilerWorldCellPlan {
    pub fn world(&self) -> &crate::compiler_world::CompilerWorld {
        &self.world
    }
    pub fn recipe(&self) -> &tidepool_extract_cmd::WorldCellRecipe {
        &self.recipe
    }
    pub fn items(&self) -> &[ParsedCellPlanItem] {
        &self.items
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
    plan: Arc<CompilerWorldCellPlan>,
    admission_digest: [u8; 32],
    slots: Arc<[crate::checked_cell::CheckedPlannedCellSlot]>,
    expected_bindings: Arc<[Vec<(String, tidepool_repr::SessionVarId)>]>,
}

impl CompilerWorldCellPlan {
    pub fn reserve(
        self: &Arc<Self>,
        admission_digest: [u8; 32],
        slots: Vec<crate::checked_cell::CheckedPlannedCellSlot>,
        expected_bindings: Vec<Vec<(String, tidepool_repr::SessionVarId)>>,
    ) -> Result<Arc<CompilerWorldCellReservation>, CompileError> {
        validate_world_cell_reservations(
            &self.items,
            admission_digest,
            &slots,
            &expected_bindings,
        )?;
        Ok(Arc::new(CompilerWorldCellReservation {
            plan: Arc::clone(self),
            admission_digest,
            slots: slots.into(),
            expected_bindings: expected_bindings.into(),
        }))
    }
}

fn validate_world_cell_reservations(
    items: &[ParsedCellPlanItem],
    admission_digest: [u8; 32],
    slots: &[crate::checked_cell::CheckedPlannedCellSlot],
    expected_bindings: &[Vec<(String, tidepool_repr::SessionVarId)>],
) -> Result<(), CompileError> {
    use crate::checked_cell::CheckedPlannedCellSlot as Slot;
    if admission_digest == [0; 32]
        || slots.len() != items.len()
        || expected_bindings.len() != items.len()
    {
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
            ) if !observation_name.is_empty() && !observation_name.contains('\0') => {
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
    producer: [u8; 32],
    receipt: Vec<u8>,
) -> Result<Arc<CompilerWorldCellPlan>, CompileError> {
    let facts = admit_world_parser_receipt(world.logical_id(), &recipe, producer, &receipt)?;
    Ok(Arc::new(CompilerWorldCellPlan {
        world,
        recipe,
        producer_sha256: producer,
        items: facts.items,
        observations: facts.observations,
        receipt: receipt.into(),
        digest: facts.digest,
    }))
}

struct WorldParserFacts {
    items: Vec<ParsedCellPlanItem>,
    observations: Arc<[u8]>,
    digest: [u8; 32],
}

fn admit_world_parser_receipt(
    expected_world: &[u8; 32],
    recipe: &tidepool_extract_cmd::WorldCellRecipe,
    producer: [u8; 32],
    receipt: &[u8],
) -> Result<WorldParserFacts, CompileError> {
    let value = decode(&receipt)?;
    let fields = crate::checked_cell::row(&value, 8)?;
    let hash = |value: &Value| -> Result<[u8; 32], CompileError> {
        match value {
            Value::Bytes(bytes) => bytes.as_slice().try_into().map_err(failure),
            _ => Err(failure("world parser hash")),
        }
    };
    if text(&fields[0])? != "TPCELLPLAN4"
        || hash(&fields[1])? != *expected_world
        || hash(&fields[2])? != producer
        || hash(&fields[3])? != <[u8; 32]>::from(Sha256::digest(recipe.source.as_bytes()))
        || hash(&fields[4])? != <[u8; 32]>::from(Sha256::digest(recipe.template.as_bytes()))
    {
        return Err(failure(
            "world parser source, template, world or producer differs",
        ));
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
    let mut digest = Sha256::new();
    digest.update(b"TidepoolWorldCellPlan1\0");
    digest.update(&receipt);
    Ok(WorldParserFacts {
        items,
        observations: observations.clone().into(),
        digest: digest.finalize().into(),
    })
}

pub(crate) fn parse(
    specification: Arc<CheckedCellSpecification>,
    include_paths: &[PathBuf],
    settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
) -> Result<Arc<ParsedCellPlan>, CompileError> {
    if specification.cell_source.len() > (2 << 20)
        || specification.template_source.len() > (2 << 20)
        || specification.turn_templates.len() > 256
        || specification
            .turn_templates
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
    if !specification.reserved_declaration_modules.is_empty() {
        return Err(CellPlanInputRejection::ReservedOwners.error());
    }
    if specification.injected_modules.len() > 4096
        || specification
            .injected_modules
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != specification.injected_modules.len()
    {
        return Err(CellPlanInputRejection::InjectedInventory.error());
    }
    let scratch = Arc::new(crate::artifacts::compiler_scratch_directory()?);
    let source = scratch.path().join("cell.txt");
    let template = scratch.path().join("cell-template.hs");
    let output = scratch.path().join("cell-plan.cbor");
    std::fs::write(&source, &specification.cell_source)?;
    std::fs::write(&template, &specification.template_source)?;
    let mut command =
        tidepool_extract_cmd::ExtractCmd::new().map_err(|error| CompileError::Io(error.into()))?;
    command
        .input(&source)
        .cell_plan()
        .cell_template(&template)
        .cell_out(&output)
        .includes(include_paths)
        .inject_vals(&specification.injected_modules);
    for (index, (kind, source)) in specification.turn_templates.iter().enumerate() {
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
    specification: Arc<CheckedCellSpecification>,
    include_paths: &[PathBuf],
    producer: &[u8; 32],
    receipt: Vec<u8>,
) -> Result<Arc<ParsedCellPlan>, CompileError> {
    let root = decode(&receipt)?;
    let fields = crate::checked_cell::row(&root, 8)?;
    if text(&fields[0])? != "TPCELLPLAN3"
        || text(&fields[1])? != crate::checked_cell::hash(specification.cell_source.as_bytes())
        || text(&fields[2])? != crate::checked_cell::hash(specification.template_source.as_bytes())
    {
        return Err(failure("source or template differs from parser receipt"));
    }
    let templates = list(&fields[3], 256)?;
    if templates.len() != specification.turn_templates.len() {
        return Err(failure("parser template count"));
    }
    for (row, (kind, source)) in templates.iter().zip(&specification.turn_templates) {
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
        || texts(&fields[5], 4096)? != specification.injected_modules
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
                [kind, name] if text(kind)? == "EValue" => {
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
    fn fixture() -> (Arc<CheckedCellSpecification>, Vec<PathBuf>, Value) {
        let specification = Arc::new(CheckedCellSpecification {
            admission_digest: [0; 32],
            cell_source: "let owned = (42 :: Int)".into(),
            template_source: "compiler-template".into(),
            turn_templates: vec![("bind".into(), "turn-template".into())],
            injected_modules: vec!["Tidepool.Session.Val.G7".into()],
            reserved_declaration_modules: Vec::new(),
        });
        let a = |values: Vec<Value>| Value::Array(values);
        let t = |value: &str| Value::Text(value.into());
        let span = a(vec![1.into(), 1.into(), 1.into(), 23.into()]);
        let item = a(vec![
            span.clone(),
            t("bind"),
            t(&specification.cell_source),
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
                specification.cell_source.as_bytes(),
            )),
            t(&crate::checked_cell::hash(
                specification.template_source.as_bytes(),
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
            [3; 32],
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
            [3; 32],
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
            [3; 32],
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

    #[test]
    fn world_parser_receipt_binds_world_canonical_producer_and_authored_recipe() {
        let (specification, _, legacy) = fixture();
        let recipe = tidepool_extract_cmd::WorldCellRecipe {
            source: specification.cell_source.clone(),
            template: specification.template_source.clone(),
            turn_templates: specification.turn_templates.clone(),
        };
        let producer =
            crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(&[1; 32])
                .sha256();
        let world = [9; 32];
        let hash = |bytes: &[u8]| Value::Bytes(Sha256::digest(bytes).to_vec());
        let fields = legacy.as_array().unwrap();
        let receipt = Value::Array(vec![
            Value::Text("TPCELLPLAN4".into()),
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
        ]);
        let admitted =
            admit_world_parser_receipt(&world, &recipe, producer, &encoded(&receipt)).unwrap();
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
            [3; 32],
            &[Slot::Bind { value: 7 }],
            &[binding.clone()]
        )
        .is_ok());
        assert!(validate_world_cell_reservations(
            &admitted.items,
            [0; 32],
            &[Slot::Bind { value: 7 }],
            &[binding.clone()]
        )
        .is_err());
        assert!(validate_world_cell_reservations(
            &admitted.items,
            [3; 32],
            &[Slot::Bind { value: 0 }],
            &[binding.clone()]
        )
        .is_err());
        assert!(validate_world_cell_reservations(
            &admitted.items,
            [3; 32],
            &[Slot::Declaration { declaration: 7 }],
            &[binding.clone()]
        )
        .is_err());
        assert!(validate_world_cell_reservations(
            &admitted.items,
            [3; 32],
            &[Slot::Bind { value: 7 }],
            &[vec![("other".into(), binding[0].1)]]
        )
        .is_err());
        assert!(validate_world_cell_reservations(
            &admitted.items,
            [3; 32],
            &[Slot::Bind { value: 7 }],
            &[Vec::new()]
        )
        .is_err());

        assert!(admit_world_parser_receipt(&world, &recipe, [1; 32], &encoded(&receipt)).is_err());
        assert!(
            admit_world_parser_receipt(&[8; 32], &recipe, producer, &encoded(&receipt)).is_err()
        );
        for index in 0..8 {
            let mut changed = receipt.clone();
            changed.as_array_mut().unwrap()[index] = Value::Null;
            assert!(
                admit_world_parser_receipt(&world, &recipe, producer, &encoded(&changed)).is_err()
            );
        }
        let mut other = receipt.clone();
        let other_producer =
            crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(&[2; 32])
                .sha256();
        other.as_array_mut().unwrap()[2] = Value::Bytes(other_producer.to_vec());
        let other =
            admit_world_parser_receipt(&world, &recipe, other_producer, &encoded(&other)).unwrap();
        assert_ne!(admitted.digest, other.digest);
        assert!(admit_world_parser_receipt(&world, &recipe, producer, &encoded(&legacy)).is_err());
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
