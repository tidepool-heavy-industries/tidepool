//! Same-offer whole-cell compiler authority. Public cell observations never
//! construct either capability; the bound compiler offer validates the receipt.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use ciborium::value::Value;
use sha2::{Digest, Sha256};

use crate::declaration_context::ExactSourceAdmission;
use crate::CompileError;

#[derive(Clone, Debug)]
pub struct CheckedCellSpecification {
    pub admission_digest: [u8; 32],
    pub cell_source: String,
    pub template_source: String,
    pub turn_templates: Vec<(String, String)>,
    pub injected_modules: Vec<String>,
    pub reserved_declaration_modules: Vec<String>,
}

impl CheckedCellSpecification {
    /// Actor admission binds the exact source and compiler recipe inputs.
    /// Runtime view/native identities enter the separate admission digest.
    pub fn specification_digest(&self) -> [u8; 32] {
        let value = array([
            text("TidepoolCheckedCellSpecification1"),
            text(&self.cell_source),
            text(&self.template_source),
            Value::Array(
                self.turn_templates
                    .iter()
                    .map(|(kind, source)| array([text(kind), text(source)]))
                    .collect(),
            ),
        ]);
        let mut bytes = Vec::new();
        // Serialization of a closed Value is infallible for a Vec writer.
        ciborium::ser::into_writer(&value, &mut bytes).expect("specification encodes to memory");
        Sha256::digest(bytes).into()
    }
    pub(crate) fn manifest_value(&self) -> Result<Value, CompileError> {
        if self.admission_digest == [0; 32] {
            return Err(failure("runtime admission digest is absent"));
        }
        let modules = &self.reserved_declaration_modules;
        if modules.iter().collect::<BTreeSet<_>>().len() != modules.len()
            || self.injected_modules.iter().collect::<BTreeSet<_>>().len()
                != self.injected_modules.len()
        {
            return Err(failure("duplicate planned or injected module"));
        }
        Ok(array([
            text("cell-check"),
            text(hex(&self.admission_digest)),
            text(hash(self.cell_source.as_bytes())),
            text(hash(self.template_source.as_bytes())),
            Value::Array(
                self.turn_templates
                    .iter()
                    .map(|(kind, source)| array([text(kind), text(hash(source.as_bytes()))]))
                    .collect(),
            ),
            Value::Array(self.injected_modules.iter().map(text).collect()),
            Value::Array(modules.iter().map(text).collect()),
        ]))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactSignatureName {
    qualifier: String,
    unit: String,
    module: String,
    namespace: String,
    occurrence: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactCheckedSignature {
    key: String,
    source: String,
    names: Vec<ExactSignatureName>,
}

impl ExactCheckedSignature {
    pub fn key(&self) -> &str {
        &self.key
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn names(&self) -> &[ExactSignatureName] {
        &self.names
    }
}

impl ExactSignatureName {
    pub fn qualifier(&self) -> &str {
        &self.qualifier
    }
    pub fn unit(&self) -> &str {
        &self.unit
    }
    pub fn module(&self) -> &str {
        &self.module
    }
    pub fn namespace(&self) -> &str {
        &self.namespace
    }
    pub fn occurrence(&self) -> &str {
        &self.occurrence
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckedItemKind {
    Declaration,
    Bind,
    Expression,
}

#[derive(Clone, Debug)]
struct CheckedItem {
    kind: CheckedItemKind,
    source: String,
    binders: Vec<String>,
    pins: Vec<Value>,
    expression: Option<Value>,
    signatures: Vec<ExactCheckedSignature>,
}

#[derive(Debug)]
pub struct ExactCheckedCell {
    specification: CheckedCellSpecification,
    producer: [u8; 32],
    context: [u8; 32],
    receipt_digest: [u8; 32],
    checked_source: String,
    evidence: crate::cache::DependencyEvidence,
    observations: Vec<u8>,
    items: Vec<CheckedItem>,
    include: Vec<std::path::PathBuf>,
}

impl ExactCheckedCell {
    pub fn item_count(&self) -> usize {
        self.items.len()
    }
    pub fn admission_digest(&self) -> [u8; 32] {
        self.specification.admission_digest
    }
    pub fn checked_source(&self) -> &str {
        &self.checked_source
    }
    pub fn observations(&self) -> &[u8] {
        &self.observations
    }
    pub fn item(self: &Arc<Self>, index: usize) -> Result<ExactCheckedItem, CompileError> {
        self.items
            .get(index)
            .ok_or_else(|| failure("item index is outside the checked cell"))?;
        Ok(ExactCheckedItem {
            cell: self.clone(),
            index,
        })
    }
    pub(crate) fn revalidate(
        &self,
        producer: &[u8],
        context: &[u8; 32],
    ) -> Result<(), CompileError> {
        if self.producer != <[u8; 32]>::from(Sha256::digest(producer))
            || &self.context != context
            || !self.evidence.valid(&self.checked_source)
        {
            return Err(failure(
                "checked cell producer, context or consumed inputs changed",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ExactCheckedItem {
    cell: Arc<ExactCheckedCell>,
    index: usize,
}

/// Compiler proofs for an ordered prefix. Runtime completion retains this
/// value only after the corresponding native installation and execution.
#[derive(Clone, Debug)]
pub struct ExactCompiledPrefix {
    cell: Arc<ExactCheckedCell>,
    completed: Vec<Arc<ExactCompiledItem>>,
}

/// A checked recipe and its exact prepared target, issued together by the
/// product-sealing entry point. Compiling alone makes no execution claim.
#[derive(Debug)]
pub struct ExactCompiledItem {
    item: ExactCheckedItem,
    target: Arc<tidepool_repr::execution_schema::PreparedProgram>,
    value_interface: Option<(String, Arc<[u8]>)>,
    generation: u64,
    bound_binders: Vec<Value>,
}

impl ExactCompiledItem {
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn validate_bound_binders(&self, bound: &[Value]) -> Result<(), CompileError> {
        if bound != self.bound_binders {
            return Err(failure(
                "compiled binder metadata was edited before execution",
            ));
        }
        Ok(())
    }
    pub fn item(&self) -> &ExactCheckedItem {
        &self.item
    }
    pub fn matches_target(
        &self,
        target: &tidepool_repr::execution_schema::PreparedProgram,
    ) -> bool {
        self.target.as_ref() == target
    }
    pub fn value_interface(&self) -> Option<(&str, &[u8])> {
        self.value_interface
            .as_ref()
            .map(|(module, bytes)| (module.as_str(), bytes.as_ref()))
    }
}

impl ExactCompiledPrefix {
    pub fn next_item(&self) -> usize {
        self.completed.len()
    }
    pub fn admission_digest(&self) -> [u8; 32] {
        self.cell.admission_digest()
    }
    pub fn append(&self, completed: Arc<ExactCompiledItem>) -> Result<Self, CompileError> {
        if completed.item.index != self.next_item()
            || !Arc::ptr_eq(&completed.item.cell, &self.cell)
        {
            return Err(failure(
                "compiled prefix is not the next item of its same cell",
            ));
        }
        let mut next = self.clone();
        next.completed.push(completed);
        Ok(next)
    }
    pub fn injected_modules(&self) -> Vec<String> {
        self.cell
            .specification
            .injected_modules
            .iter()
            .cloned()
            .chain(self.completed.iter().filter_map(|completed| {
                completed
                    .value_interface
                    .as_ref()
                    .map(|(module, _)| module.clone())
            }))
            .collect()
    }
    pub fn completed_interfaces(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.completed
            .iter()
            .filter_map(|completed| completed.value_interface())
    }
    fn value_imports(&self) -> Vec<(String, Vec<String>)> {
        let mut winners = std::collections::BTreeMap::new();
        for completed in &self.completed {
            if let Some((module, _)) = completed.value_interface() {
                for name in completed.item.binders() {
                    winners.insert(name.clone(), module.to_owned());
                }
            }
        }
        let mut imports = std::collections::BTreeMap::<String, Vec<String>>::new();
        for (name, module) in winners {
            imports.entry(module).or_default().push(name);
        }
        imports.into_iter().collect()
    }
}

impl PartialEq for ExactCheckedItem {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index && Arc::ptr_eq(&self.cell, &other.cell)
    }
}
impl Eq for ExactCheckedItem {}

impl ExactCheckedItem {
    pub fn index(&self) -> usize {
        self.index
    }
    pub fn admission_digest(&self) -> [u8; 32] {
        self.cell.admission_digest()
    }
    pub fn source(&self) -> &str {
        &self.cell.items[self.index].source
    }
    pub fn kind(&self) -> CheckedItemKind {
        self.cell.items[self.index].kind
    }
    pub fn binders(&self) -> &[String] {
        &self.cell.items[self.index].binders
    }
    pub fn signatures(&self) -> &[ExactCheckedSignature] {
        &self.cell.items[self.index].signatures
    }
    pub fn same_cell(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.cell, &other.cell)
    }
    pub fn initial_prefix(&self) -> Result<ExactCompiledPrefix, CompileError> {
        if self.index != 0 {
            return Err(failure("a prefix must start at item zero"));
        }
        Ok(ExactCompiledPrefix {
            cell: self.cell.clone(),
            completed: Vec::new(),
        })
    }
    pub fn validate_observations(
        &self,
        source: &str,
        kind: CheckedItemKind,
        binders: &[String],
        pins: &[Value],
        expression: Option<&Value>,
    ) -> Result<(), CompileError> {
        let expected = &self.cell.items[self.index];
        if source != expected.source
            || kind != expected.kind
            || binders != expected.binders
            || pins != expected.pins
            || expression != expected.expression.as_ref()
        {
            return Err(failure(
                "checked item body, verdict, pins or expression plan was edited",
            ));
        }
        Ok(())
    }
}

pub(crate) fn seal_checked_fold(
    root: &Path,
    producer: &[u8],
    context: [u8; 32],
    request: &str,
    cell: &Arc<ExactCheckedCell>,
    generation: u64,
    source: &str,
    target: &tidepool_repr::execution_schema::PreparedProgram,
) -> Result<Arc<ExactCompiledItem>, CompileError> {
    cell.revalidate(producer, &context)?;
    if cell.items.len() != 1
        || cell.items[0].kind != CheckedItemKind::Bind
        || hash(&read(root.join("checked-cell.cbor"), 8 * 1024 * 1024)?)
            != hex(&cell.receipt_digest)
    {
        return Err(failure(
            "fold is not the sole bind of its same checked offer",
        ));
    }
    let item = cell.item(0)?;
    CheckedItemOffer {
        prefix: item.initial_prefix()?,
        item,
        runtime_prefix_digest: cell.admission_digest(),
        generation,
    }
    .seal(root, request, source, target)
}

#[derive(Clone, Debug)]
pub(crate) struct CheckedItemOffer {
    pub(crate) item: ExactCheckedItem,
    pub(crate) prefix: ExactCompiledPrefix,
    pub(crate) runtime_prefix_digest: [u8; 32],
    pub(crate) generation: u64,
}

impl CheckedItemOffer {
    pub(crate) fn authorization(
        &self,
        producer: &[u8],
        context: [u8; 32],
    ) -> Result<Value, CompileError> {
        self.item.cell.revalidate(producer, &context)?;
        if self.item.index != self.prefix.next_item()
            || !Arc::ptr_eq(&self.item.cell, &self.prefix.cell)
            || self.runtime_prefix_digest == [0; 32]
        {
            return Err(failure(
                "checked item has another or incomplete completed prefix",
            ));
        }
        if self.item.kind() == CheckedItemKind::Declaration {
            return Err(failure(
                "planned original declaration certificate is unavailable",
            ));
        }
        let expected = &self.item.cell.items[self.item.index];
        Ok(array([
            text("checked-item"),
            text(hex(&self.item.admission_digest())),
            text(hex(&self.item.cell.receipt_digest)),
            Value::Integer((self.item.index as u64).into()),
            text(hash(self.item.source().as_bytes())),
            text(match self.item.kind() {
                CheckedItemKind::Bind => "bind",
                CheckedItemKind::Expression => "expr",
                CheckedItemKind::Declaration => "decl",
            }),
            Value::Array(self.item.binders().iter().map(text).collect()),
            Value::Array(
                self.item
                    .cell
                    .specification
                    .turn_templates
                    .iter()
                    .map(|(kind, source)| array([text(kind), text(hash(source.as_bytes()))]))
                    .collect(),
            ),
            Value::Array(self.prefix.injected_modules().iter().map(text).collect()),
            Value::Array(
                self.item
                    .signatures()
                    .iter()
                    .map(encode_signature)
                    .collect(),
            ),
            expected.expression.clone().unwrap_or(Value::Null),
            Value::Integer(self.generation.into()),
            text(hex(&self.runtime_prefix_digest)),
            Value::Array(
                self.prefix
                    .value_imports()
                    .iter()
                    .map(|(module, names)| {
                        array([text(module), Value::Array(names.iter().map(text).collect())])
                    })
                    .collect(),
            ),
        ]))
    }
    pub(crate) fn validate_templates(
        &self,
        templates: &[(String, String)],
    ) -> Result<(), CompileError> {
        if templates != &self.item.cell.specification.turn_templates {
            return Err(failure("checked-item wrapper was edited after admission"));
        }
        Ok(())
    }
    pub(crate) fn validate_include(
        &self,
        include: &[std::path::PathBuf],
    ) -> Result<(), CompileError> {
        if include != self.item.cell.include {
            return Err(failure("checked item include search order changed"));
        }
        Ok(())
    }
    pub(crate) fn seal(
        &self,
        root: &Path,
        request: &str,
        source: &str,
        target: &tidepool_repr::execution_schema::PreparedProgram,
    ) -> Result<Arc<ExactCompiledItem>, CompileError> {
        let receipt = decode(&read(root.join("checked-item.cbor"), 4 * 1024 * 1024)?)?;
        let fields = row(&receipt, 8)?;
        if string(&fields[0])? != "TPEXACTITEM"
            || string(&fields[1])? != "1"
            || string(&fields[2])? != request
            || string(&fields[3])? != hex(&self.item.admission_digest())
            || string(&fields[4])? != hex(&self.item.cell.receipt_digest)
            || fields[5] != Value::Integer((self.item.index as u64).into())
            || string(&fields[6])? != hash(source.as_bytes())
            || string(&fields[7])? != "tidepool-checked-recipe-1"
        {
            return Err(failure(
                "checked-item recipe receipt differs from its same compiler offer",
            ));
        }
        let turn = decode(&read(root.join("turn.cbor"), 32 * 1024 * 1024)?)?;
        let turn = row(&turn, 2)?;
        let bound_binders = match (self.item.kind(), string(&turn[0])?) {
            (CheckedItemKind::Bind, "Bind") => {
                let fields = row(&turn[1], 5)?;
                if fields[0] != Value::Array(self.item.binders().iter().map(text).collect())
                    || string(&fields[4])? != source
                {
                    return Err(failure(
                        "compiled bind has another authored verdict or wrapper",
                    ));
                }
                let bound = list(&fields[2], 65536)?.to_vec();
                if bound.len() != self.item.binders().len() {
                    return Err(failure("compiled binder inventory differs"));
                }
                for (value, binder) in bound.iter().zip(self.item.binders()) {
                    let fields = row(value, 7)?;
                    if string(&fields[0])? != binder
                        || string(&fields[2])?
                            != tidepool_repr::SessionModule::val(tidepool_repr::Generation(
                                self.generation,
                            ))
                            .module_name()
                    {
                        return Err(failure(
                            "compiled binding has another reserved native generation",
                        ));
                    }
                }
                bound
            }
            (CheckedItemKind::Expression, "Expr") => {
                if string(&row(&turn[1], 3)?[2])? != source {
                    return Err(failure("compiled expression wrapper differs"));
                }
                Vec::new()
            }
            _ => return Err(failure("compiled turn kind differs from checked item")),
        };
        let value_interface =
            if self.item.kind() == CheckedItemKind::Bind && !self.item.binders().is_empty() {
                let module =
                    tidepool_repr::SessionModule::val(tidepool_repr::Generation(self.generation));
                let bytes = read(
                    root.join("admitted-values").join(module.relative_hi_path()),
                    32 * 1024 * 1024,
                )?;
                Some((module.module_name(), bytes.into()))
            } else {
                None
            };
        Ok(Arc::new(ExactCompiledItem {
            item: self.item.clone(),
            target: Arc::new(target.clone()),
            value_interface,
            generation: self.generation,
            bound_binders,
        }))
    }
}

fn encode_signature(signature: &ExactCheckedSignature) -> Value {
    array([
        text(&signature.key),
        text(&signature.source),
        Value::Array(
            signature
                .names
                .iter()
                .map(|name| {
                    array([
                        text(&name.qualifier),
                        text(&name.unit),
                        text(&name.module),
                        text(&name.namespace),
                        text(&name.occurrence),
                    ])
                })
                .collect(),
        ),
    ])
}

pub(crate) fn admit_checked_cell(
    root: &Path,
    producer: &[u8],
    context: [u8; 32],
    request_digest: &str,
    specification: &CheckedCellSpecification,
    admissions: Vec<ExactSourceAdmission>,
    include: &[std::path::PathBuf],
) -> Result<Arc<ExactCheckedCell>, CompileError> {
    let receipt = read(root.join("checked-cell.cbor"), 8 * 1024 * 1024)?;
    let value = decode(&receipt)?;
    let header = row(&value, 9)?;
    let observations = read(root.join("cell.cbor"), 32 * 1024 * 1024)?;
    let output = decode(&observations)?;
    let output = row(&output, 5)?;
    let checked_source = string(&output[2])?.to_owned();
    if string(&header[0])? != "TPEXACTCHECK"
        || string(&header[1])? != "1"
        || string(&header[2])? != request_digest
        || string(&header[3])? != hex(&specification.admission_digest)
        || string(&header[4])? != hash(specification.cell_source.as_bytes())
        || string(&header[5])? != hash(specification.template_source.as_bytes())
        || string(&header[6])? != hash(&observations)
        || string(&header[7])? != hash(checked_source.as_bytes())
    {
        return Err(failure(
            "whole-cell receipt differs from the same admitted compiler offer",
        ));
    }
    let source = admissions
        .into_iter()
        .find(|source| {
            source.witness.source_sha256()
                == &<[u8; 32]>::from(Sha256::digest(checked_source.as_bytes()))
        })
        .ok_or_else(|| failure("whole-cell receipt has no final exact compilation witness"))?;
    let signatures = list(&header[8], 65536)?
        .iter()
        .map(decode_signature)
        .collect::<Result<Vec<_>, _>>()?;
    let mut signature_keys = BTreeSet::new();
    if signatures
        .iter()
        .any(|signature| !signature_keys.insert(&signature.key))
    {
        return Err(failure("duplicate signature authority"));
    }
    let pins = list(&output[1], 65536)?;
    let expressions = list(&output[4], 65536)?;
    let items = list(&output[0], 65536)?
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let fields = row(value, 6)?;
            let kind = match string(&fields[1])? {
                "decl" => CheckedItemKind::Declaration,
                "bind" => CheckedItemKind::Bind,
                "expr" => CheckedItemKind::Expression,
                _ => return Err(failure("unknown checked item kind")),
            };
            let verdict = row(&fields[3], 2)?;
            let binders = list(&verdict[0], 65536)?
                .iter()
                .map(|value| string(value).map(str::to_owned))
                .collect::<Result<Vec<_>, _>>()?;
            let item_pins = if kind == CheckedItemKind::Bind {
                binders
                    .iter()
                    .map(|binder| {
                        unique_key(pins, &format!("__tidepool_cell_pin_{index}_{binder}"), 4)
                    })
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                Vec::new()
            };
            let expression = if kind == CheckedItemKind::Expression {
                Some(unique_key(
                    expressions,
                    &format!("__tidepool_cell_expr_{index}"),
                    6,
                )?)
            } else {
                None
            };
            let mut keys = item_pins
                .iter()
                .map(|value| row(value, 4).and_then(|row| string(&row[0])))
                .collect::<Result<Vec<_>, _>>()?;
            if let Some(expression) = &expression {
                keys.push(string(&row(expression, 6)?[0])?);
            }
            let item_signatures = keys
                .iter()
                .map(|key| {
                    signatures
                        .iter()
                        .find(|signature| &signature.key == key)
                        .cloned()
                        .ok_or_else(|| {
                            failure("checked item lacks complete signature Name authority")
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CheckedItem {
                kind,
                source: string(&fields[2])?.to_owned(),
                binders,
                pins: item_pins,
                expression,
                signatures: item_signatures,
            })
        })
        .collect::<Result<Vec<_>, CompileError>>()?;
    let used = items
        .iter()
        .flat_map(|item| &item.signatures)
        .map(|signature| &signature.key)
        .collect::<BTreeSet<_>>();
    if used.len() != signatures.len()
        || pins.len() != items.iter().map(|item| item.pins.len()).sum::<usize>()
        || expressions.len()
            != items
                .iter()
                .filter(|item| item.expression.is_some())
                .count()
    {
        return Err(failure(
            "whole-cell authority contains an unowned pin, plan or signature",
        ));
    }
    Ok(Arc::new(ExactCheckedCell {
        specification: specification.clone(),
        producer: Sha256::digest(producer).into(),
        context,
        receipt_digest: Sha256::digest(&receipt).into(),
        checked_source,
        evidence: source.evidence,
        observations,
        items,
        include: include.to_vec(),
    }))
}

fn decode_signature(value: &Value) -> Result<ExactCheckedSignature, CompileError> {
    let fields = row(value, 3)?;
    let mut qualifiers = BTreeSet::new();
    let names = list(&fields[2], 65536)?
        .iter()
        .map(|value| {
            let fields = row(value, 5)?;
            let qualifier = string(&fields[0])?.to_owned();
            let namespace = string(&fields[3])?.to_owned();
            if !qualifier.starts_with("TidepoolCheckedName")
                || !qualifiers.insert(qualifier.clone())
                || !matches!(namespace.as_str(), "type" | "data")
            {
                return Err(failure("invalid signature Name qualifier or namespace"));
            }
            Ok(ExactSignatureName {
                qualifier,
                unit: string(&fields[1])?.to_owned(),
                module: string(&fields[2])?.to_owned(),
                namespace,
                occurrence: string(&fields[4])?.to_owned(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ExactCheckedSignature {
        key: string(&fields[0])?.to_owned(),
        source: string(&fields[1])?.to_owned(),
        names,
    })
}
fn unique_key(values: &[Value], key: &str, count: usize) -> Result<Value, CompileError> {
    let matches = values
        .iter()
        .map(|value| Ok((value, string(&row(value, count)?[0])?)))
        .collect::<Result<Vec<_>, CompileError>>()?
        .into_iter()
        .filter(|(_, found)| *found == key)
        .map(|(value, _)| value)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [value] => Ok((*value).clone()),
        _ => Err(failure("missing or duplicate checked key")),
    }
}
fn read(path: impl AsRef<Path>, limit: u64) -> Result<Vec<u8>, CompileError> {
    if std::fs::metadata(path.as_ref())?.len() > limit {
        return Err(failure("checked evidence exceeds bound"));
    }
    let bytes = std::fs::read(path)?;
    if bytes.len() as u64 > limit {
        return Err(failure("checked evidence exceeds bound"));
    }
    Ok(bytes)
}
fn decode(bytes: &[u8]) -> Result<Value, CompileError> {
    let mut cursor = std::io::Cursor::new(bytes);
    let value = ciborium::de::from_reader(&mut cursor).map_err(failure)?;
    if cursor.position() != bytes.len() as u64 {
        return Err(failure("checked evidence has trailing bytes"));
    }
    Ok(value)
}
fn row(value: &Value, count: usize) -> Result<&[Value], CompileError> {
    let values = list(value, count)?;
    if values.len() != count {
        return Err(failure("invalid checked evidence row"));
    }
    Ok(values)
}
fn list(value: &Value, limit: usize) -> Result<&[Value], CompileError> {
    match value {
        Value::Array(values) if values.len() <= limit => Ok(values),
        _ => Err(failure("invalid checked evidence array")),
    }
}
fn string(value: &Value) -> Result<&str, CompileError> {
    match value {
        Value::Text(value) => Ok(value),
        _ => Err(failure("invalid checked evidence text")),
    }
}
fn array<const N: usize>(values: [Value; N]) -> Value {
    Value::Array(Vec::from(values))
}
fn text(value: impl AsRef<str>) -> Value {
    Value::Text(value.as_ref().to_owned())
}
fn hash(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes).into())
}
fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn failure(error: impl std::fmt::Display) -> CompileError {
    CompileError::ExtractFailed(format!("checked cell: {error}"))
}
