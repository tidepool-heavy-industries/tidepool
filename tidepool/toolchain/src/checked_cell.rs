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
}

impl ExactCheckedCell {
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
    pub(crate) fn cell(&self) -> &Arc<ExactCheckedCell> {
        &self.cell
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

pub(crate) fn admit_checked_cell(
    root: &Path,
    producer: &[u8],
    context: [u8; 32],
    request_digest: &str,
    specification: &CheckedCellSpecification,
    admissions: Vec<ExactSourceAdmission>,
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
