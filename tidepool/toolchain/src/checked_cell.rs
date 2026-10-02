//! Same-offer whole-cell compiler authority. Public cell observations never
//! construct either capability; the bound compiler offer validates the receipt.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
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

/// Independently issued canonical input type and its original owner interfaces.
/// Parser presentation is retained, but never participates in type equality.
#[derive(Clone, Debug)]
pub struct CanonicalInputTypeWitness {
    signature: ExactCheckedSignature,
    structure: Arc<[u8]>,
    interfaces: Vec<(String, String, String)>,
    metadata_digest: [u8; 32],
}

impl PartialEq for CanonicalInputTypeWitness {
    fn eq(&self, other: &Self) -> bool {
        self.structure == other.structure && self.interfaces == other.interfaces
    }
}
impl Eq for CanonicalInputTypeWitness {}

impl CanonicalInputTypeWitness {
    pub(crate) fn metadata_digest(&self) -> [u8; 32] {
        self.metadata_digest
    }
    pub fn commitment(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"tidepool-canonical-input-type-1");
        hasher.update((self.structure.len() as u64).to_le_bytes());
        hasher.update(&self.structure);
        for (unit, module, fingerprint) in &self.interfaces {
            for value in [unit, module, fingerprint] {
                hasher.update((value.len() as u64).to_le_bytes());
                hasher.update(value.as_bytes());
            }
        }
        hasher.finalize().into()
    }
    pub fn signature(&self) -> &ExactCheckedSignature {
        &self.signature
    }
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, CompileError> {
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(failure("canonical input witness byte bound"));
        }
        let decoded = decode(bytes)?;
        let fields = row(&decoded, 5)?;
        if string(&fields[0])? != "TPCANONICALINPUTTYPE1" || string(&fields[1])? != "1" {
            return Err(failure("canonical input witness version"));
        }
        let signature = decode_signature(&fields[2])?;
        if signature.key() != "activation-input" {
            return Err(failure("canonical input witness purpose"));
        }
        let Value::Bytes(structure) = &fields[3] else {
            return Err(failure("canonical input witness structure"));
        };
        let mut names = BTreeSet::new();
        let mut count = 0;
        validate_input_type_shape(&decode(structure)?, 0, 0, &mut count, &mut names)?;
        let interfaces = list(&fields[4], 65536)?
            .iter()
            .map(|value| {
                let fields = row(value, 3)?;
                let unit = string(&fields[0])?.to_owned();
                let module = string(&fields[1])?.to_owned();
                let fingerprint = string(&fields[2])?.to_owned();
                if unit.is_empty()
                    || module.is_empty()
                    || fingerprint.len() != 64
                    || !fingerprint
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
                {
                    return Err(failure("canonical input witness interface seal"));
                }
                Ok((unit, module, fingerprint))
            })
            .collect::<Result<Vec<_>, CompileError>>()?;
        if interfaces
            .windows(2)
            .any(|pair| (&pair[0].0, &pair[0].1) >= (&pair[1].0, &pair[1].1))
            || interfaces
                .iter()
                .map(|(unit, module, _)| (unit.clone(), module.clone()))
                .collect::<BTreeSet<_>>()
                != names
        {
            return Err(failure("canonical input witness owner coverage"));
        }
        Ok(Self {
            signature,
            structure: structure.clone().into(),
            interfaces,
            metadata_digest: Sha256::digest(bytes).into(),
        })
    }
}

impl<'de> serde::Deserialize<'de> for CanonicalInputTypeWitness {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let encoded = <String as serde::Deserialize>::deserialize(deserializer)?;
        if encoded.len() > 8 * 1024 * 1024 || encoded.len() % 2 != 0 {
            return Err(serde::de::Error::custom(
                "canonical input witness hex bound",
            ));
        }
        let bytes = encoded
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let digit = |byte: u8| match byte {
                    b'0'..=b'9' => Some(byte - b'0'),
                    b'a'..=b'f' => Some(byte - b'a' + 10),
                    _ => None,
                };
                digit(pair[0])
                    .zip(digit(pair[1]))
                    .map(|(high, low)| high * 16 + low)
                    .ok_or_else(|| serde::de::Error::custom("canonical input witness hex"))
            })
            .collect::<Result<Vec<_>, D::Error>>()?;
        Self::from_bytes(&bytes).map_err(serde::de::Error::custom)
    }
}

fn validate_input_type_shape(
    value: &Value,
    depth: usize,
    binders: usize,
    count: &mut usize,
    owners: &mut BTreeSet<(String, String)>,
) -> Result<(), CompileError> {
    if depth > 128 || *count >= 65536 {
        return Err(failure("canonical input type shape bound"));
    }
    *count += 1;
    let fields = list(value, 65536)?;
    let tag = fields
        .first()
        .ok_or_else(|| failure("canonical input type shape tag"))?;
    let recurse = |value: &Value, count: &mut usize, owners: &mut BTreeSet<(String, String)>| {
        validate_input_type_shape(value, depth + 1, binders, count, owners)
    };
    match string(tag)? {
        "bound" => {
            let fields = row(value, 2)?;
            let Value::Integer(index) = fields[1] else {
                return Err(failure("canonical input bound variable"));
            };
            if usize::try_from(index)
                .ok()
                .is_none_or(|index| index >= binders)
            {
                return Err(failure("canonical input free variable"));
            }
        }
        "con" => {
            let fields = row(value, 3)?;
            let name = row(&fields[1], 4)?;
            if !matches!(string(&name[2])?, "type" | "data") || string(&name[3])?.is_empty() {
                return Err(failure("canonical input type Name"));
            }
            owners.insert((string(&name[0])?.to_owned(), string(&name[1])?.to_owned()));
            for argument in list(&fields[2], 65536)? {
                recurse(argument, count, owners)?;
            }
        }
        "app" => {
            let fields = row(value, 3)?;
            recurse(&fields[1], count, owners)?;
            recurse(&fields[2], count, owners)?;
        }
        "fun" => {
            let fields = row(value, 5)?;
            if !matches!(fields[1], Value::Integer(tag) if u64::try_from(tag).ok().is_some_and(|tag| tag < 4))
            {
                return Err(failure("canonical input function flag"));
            }
            for field in &fields[2..] {
                recurse(field, count, owners)?;
            }
        }
        "forall" => {
            let fields = row(value, 4)?;
            if !matches!(fields[1], Value::Integer(tag) if u64::try_from(tag).ok().is_some_and(|tag| tag < 3))
            {
                return Err(failure("canonical input forall visibility"));
            }
            recurse(&fields[2], count, owners)?;
            validate_input_type_shape(&fields[3], depth + 1, binders + 1, count, owners)?;
        }
        "literal" => {
            let fields = row(value, 3)?;
            let valid = match (string(&fields[1])?, &fields[2]) {
                ("nat", Value::Text(value)) => {
                    !value.is_empty()
                        && value.bytes().all(|byte| byte.is_ascii_digit())
                        && (value == "0" || !value.starts_with('0'))
                }
                ("symbol", Value::Text(_)) => true,
                ("char", Value::Integer(value)) => u32::try_from(*value)
                    .ok()
                    .and_then(char::from_u32)
                    .is_some(),
                _ => false,
            };
            if !valid {
                return Err(failure("canonical input type literal"));
            }
        }
        _ => return Err(failure("canonical input type shape version")),
    }
    Ok(())
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
    declaration_context: Arc<crate::declaration_context::ExactDeclarationContext>,
    publication_context: Arc<crate::declaration_context::ExactDeclarationContext>,
    receipt_digest: [u8; 32],
    checked_source: String,
    evidence: Vec<(String, crate::cache::DependencyEvidence)>,
    observations: Vec<u8>,
    items: Vec<CheckedItem>,
    include: Vec<std::path::PathBuf>,
    planned_declaration: Option<PlannedCheckedDeclaration>,
    planned_declarations: BTreeMap<usize, PlannedCheckedDeclaration>,
    value_inputs: Arc<CheckedValueInputs>,
}

/// Read-only original identities from a final ordered compiler receipt.
/// Constructing this projection cannot issue an ordered checked capability.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckedPlannedCellSlot {
    Prologue {
        declaration: u64,
    },
    Declaration {
        declaration: u64,
    },
    Bind {
        value: u64,
    },
    Expression {
        capture: u64,
        display: u64,
        observation_name: String,
    },
}

/// An untrusted ordered offer recipe. Only a final same-offer worker receipt
/// can promote these reservations into an `ExactCheckedCell`.
#[derive(Clone, Debug)]
pub struct CheckedPlannedCellSpecification {
    pub parsed_plan: Arc<crate::cell_plan::ParsedCellPlan>,
    pub reservation_digest: [u8; 32],
    pub slots: Vec<CheckedPlannedCellSlot>,
}

impl CheckedPlannedCellSpecification {
    pub(crate) fn authorization(
        &self,
        specification: &CheckedCellSpecification,
        producer: &[u8],
        include: &[std::path::PathBuf],
        scratch: &Path,
    ) -> Result<Vec<Value>, CompileError> {
        use crate::cell_plan::ParsedCellPlanKind;
        if self.reservation_digest == [0; 32]
            || self.reservation_digest != specification.admission_digest
            || self.parsed_plan.specification_digest() != specification.specification_digest()
            || !same_include_paths(self.parsed_plan.include_paths(), include)
            || self.parsed_plan.injected_modules() != specification.injected_modules
            || self.parsed_plan.producer_sha256()
                != crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
                    producer,
                )
                .sha256()
            || self.slots.len() != self.parsed_plan.items().len()
        {
            return Err(planned_input_rejection(
                "ordered source, producer, or reservation differs",
            ));
        }
        let mut libraries = Vec::new();
        let mut values = BTreeSet::new();
        let mut observations = BTreeSet::new();
        let mut encoded = Vec::with_capacity(self.slots.len());
        for (item, slot) in self.parsed_plan.items().iter().zip(&self.slots) {
            let fields = match (item.kind(), slot) {
                (
                    ParsedCellPlanKind::Prologue,
                    CheckedPlannedCellSlot::Prologue { declaration },
                )
                | (
                    ParsedCellPlanKind::Declaration,
                    CheckedPlannedCellSlot::Declaration { declaration },
                ) if *declaration > 0 => {
                    libraries.push(
                        tidepool_repr::SessionModule::lib(tidepool_repr::Generation(*declaration))
                            .module_name(),
                    );
                    vec![
                        text(if item.kind() == ParsedCellPlanKind::Prologue {
                            "prologue"
                        } else {
                            "decl"
                        }),
                        Value::Integer((*declaration).into()),
                    ]
                }
                (ParsedCellPlanKind::Bind, CheckedPlannedCellSlot::Bind { value })
                    if *value > 0 && values.insert(*value) =>
                {
                    vec![text("bind"), Value::Integer((*value).into())]
                }
                (
                    ParsedCellPlanKind::Expression,
                    CheckedPlannedCellSlot::Expression {
                        capture,
                        display,
                        observation_name,
                    },
                ) if *capture > 0
                    && *display > 0
                    && values.insert(*capture)
                    && values.insert(*display)
                    && valid_observation_name(observation_name)
                    && observations.insert(observation_name.as_str()) =>
                {
                    vec![
                        text("expr"),
                        Value::Integer((*capture).into()),
                        Value::Integer((*display).into()),
                        text(observation_name),
                    ]
                }
                _ => {
                    return Err(planned_input_rejection(
                        "ordered item has another reserved slot",
                    ));
                }
            };
            encoded.push(Value::Array(fields));
        }
        if libraries != specification.reserved_declaration_modules
            || libraries.iter().collect::<BTreeSet<_>>().len() != libraries.len()
        {
            return Err(planned_input_rejection(
                "ordered original owner inventory differs",
            ));
        }
        let path = scratch.join("parsed-cell-plan.cbor");
        std::fs::write(&path, self.parsed_plan.receipt())?;
        let path = path
            .to_str()
            .filter(|_| path.is_absolute())
            .ok_or_else(|| planned_input_rejection("ordered parser receipt has no exact path"))?;
        Ok(vec![
            text(hex(&self.parsed_plan.digest())),
            text(path),
            text(hash(self.parsed_plan.receipt())),
            text(hex(&self.reservation_digest)),
            Value::Array(encoded),
        ])
    }
}

fn valid_observation_name(name: &str) -> bool {
    name.starts_with("observation")
        && name.len() <= 65536
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn planned_input_rejection(message: &str) -> CompileError {
    CompileError::InputRejected(vec![crate::diag::ExtractDiag {
        span: None,
        severity: crate::diag::DiagnosticSeverity::Error,
        message: message.to_owned(),
    }])
}

/// Complete immutable output of one admitted compiler transaction. Preparing
/// interfaces never asserts that the corresponding native binding is live.
#[derive(Debug)]
pub struct CellProgram {
    pub(crate) checked: Arc<ExactCheckedCell>,
    pub(crate) parsed: Arc<crate::cell_plan::ParsedCellPlan>,
    pub(crate) slots: Vec<CheckedPlannedCellSlot>,
    pub(crate) items: Vec<CellProgramItem>,
}

#[derive(Debug)]
pub struct CellProgramItem {
    pub(crate) checked: ExactCheckedItem,
    pub(crate) native: Option<Arc<ExactCompiledItem>>,
    pub(crate) display: Option<Arc<ExactCompiledDisplay>>,
    pub(crate) native_observations: Option<CellProgramObservations>,
    pub(crate) display_observations: Option<CellProgramObservations>,
}

#[derive(Debug)]
pub(crate) struct CellProgramObservations {
    pub(crate) turn: Arc<[u8]>,
    pub(crate) metadata: Arc<[u8]>,
    pub(crate) products: Arc<crate::artifacts::SealedTurnProducts>,
}

impl CellProgram {
    pub fn checked_cell(&self) -> &Arc<ExactCheckedCell> {
        &self.checked
    }
    pub fn parsed_plan(&self) -> &Arc<crate::cell_plan::ParsedCellPlan> {
        &self.parsed
    }
    pub fn admission_digest(&self) -> [u8; 32] {
        self.checked.admission_digest()
    }
    pub fn slots(&self) -> &[CheckedPlannedCellSlot] {
        &self.slots
    }
    pub fn items(&self) -> &[CellProgramItem] {
        &self.items
    }
}

impl CellProgramItem {
    pub fn checked_item(&self) -> &ExactCheckedItem {
        &self.checked
    }
    pub fn native(&self) -> Option<&Arc<ExactCompiledItem>> {
        self.native.as_ref()
    }
    pub fn display(&self) -> Option<&Arc<ExactCompiledDisplay>> {
        self.display.as_ref()
    }
    pub fn native_turn_bytes(&self) -> Option<&[u8]> {
        self.native_observations
            .as_ref()
            .map(|value| value.turn.as_ref())
    }
    pub fn native_metadata_bytes(&self) -> Option<&[u8]> {
        self.native_observations
            .as_ref()
            .map(|value| value.metadata.as_ref())
    }
    pub fn native_products(&self) -> Option<&crate::artifacts::SealedTurnProducts> {
        self.native_observations
            .as_ref()
            .map(|value| value.products.as_ref())
    }
    pub fn display_turn_bytes(&self) -> Option<&[u8]> {
        self.display_observations
            .as_ref()
            .map(|value| value.turn.as_ref())
    }
    pub fn display_metadata_bytes(&self) -> Option<&[u8]> {
        self.display_observations
            .as_ref()
            .map(|value| value.metadata.as_ref())
    }
    pub fn display_products(&self) -> Option<&crate::artifacts::SealedTurnProducts> {
        self.display_observations
            .as_ref()
            .map(|value| value.products.as_ref())
    }
}

/// The existing checked-cell artifact owner retains one directory for exact
/// input bytes. Directory membership is never authority: every request lists
/// only the original inventory and its sealed completed deltas.
#[derive(Debug)]
pub(crate) struct CheckedValueInputs {
    directory: tempfile::TempDir,
    baseline: Vec<Arc<CheckedValueArtifact>>,
    initial_bytes: u64,
    output_files_hashed: AtomicU64,
    output_bytes_hashed: AtomicU64,
}

/// Work performed by the immutable checked input owner. These observations
/// exclude compiler-side reads and publication copies and grant no authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckedInputWork {
    pub initial_files_written: u64,
    pub initial_bytes_written_and_hashed: u64,
    pub output_files_hashed: u64,
    pub output_bytes_hashed: u64,
}

/// Immutable thin-interface bytes retained by the checked compiler owner.
/// Only sealed native item/display getters expose stamped output artifacts.
#[derive(Debug, PartialEq, Eq)]
pub struct CheckedValueArtifact {
    owner: tidepool_repr::SessionModule,
    module: String,
    bytes: Arc<[u8]>,
    path: std::path::PathBuf,
    digest: String,
    authority: Option<([u8; 32], [u8; 32])>,
    certified_interface: Option<Arc<crate::recovery_artifacts::CertifiedValueInterface>>,
    artifact_view: Option<crate::artifact_inventory::ArtifactView>,
    source_lexical: Vec<crate::declaration_join::ExactLexicalNode>,
}

/// The exact checked interface bytes named by one compiler authorization.
/// This permits their imports without making other hydrated owners lexical.
#[derive(Clone, Default)]
pub(crate) struct CheckedValueImportAuthority {
    values: Arc<BTreeMap<String, (Arc<[u8]>, String, std::path::PathBuf)>>,
}

impl CheckedValueImportAuthority {
    fn capture<'a>(values: impl Iterator<Item = &'a CheckedValueArtifact>) -> Self {
        Self {
            values: Arc::new(
                values
                    .map(|artifact| {
                        (
                            artifact.module.clone(),
                            (
                                artifact.bytes.clone(),
                                artifact.digest.clone(),
                                artifact.path.clone(),
                            ),
                        )
                    })
                    .collect(),
            ),
        }
    }

    pub(crate) fn owners(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values.keys().map(|module| ("main", module.as_str()))
    }

    pub(crate) fn validate(&self) -> Result<(), CompileError> {
        for (bytes, digest, path) in self.values.values() {
            let observed = read(path, 32 * 1024 * 1024)?;
            if hash(&observed) != *digest || observed.as_slice() != bytes.as_ref() {
                return Err(failure("checked value import interface changed"));
            }
        }
        Ok(())
    }
}

impl CheckedValueInputs {
    pub(crate) fn capture(
        values: Vec<(tidepool_repr::SessionModule, Arc<[u8]>)>,
    ) -> Result<Arc<Self>, CompileError> {
        let directory = tempfile::Builder::new()
            .prefix("tidepool-checked-values-")
            .tempdir()?;
        let mut baseline = Vec::with_capacity(values.len());
        let mut initial_bytes = 0;
        for (owner, bytes) in values {
            let path = directory.path().join(owner.relative_hi_path());
            std::fs::create_dir_all(path.parent().expect("generated interface parent"))?;
            std::fs::write(&path, &bytes)?;
            initial_bytes += bytes.len() as u64;
            baseline.push(Arc::new(CheckedValueArtifact {
                owner,
                module: owner.module_name(),
                digest: hash(&bytes),
                bytes,
                path,
                authority: None,
                certified_interface: None,
                artifact_view: None,
                source_lexical: Vec::new(),
            }));
        }
        Ok(Arc::new(Self {
            directory,
            baseline,
            initial_bytes,
            output_files_hashed: AtomicU64::new(0),
            output_bytes_hashed: AtomicU64::new(0),
        }))
    }

    fn work(&self) -> CheckedInputWork {
        CheckedInputWork {
            initial_files_written: self.baseline.len() as u64,
            initial_bytes_written_and_hashed: self.initial_bytes,
            output_files_hashed: self.output_files_hashed.load(Ordering::Relaxed),
            output_bytes_hashed: self.output_bytes_hashed.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn root(&self) -> &Path {
        self.directory.path()
    }

    pub(crate) fn baseline_authorization(&self) -> Value {
        Value::Array(
            self.baseline
                .iter()
                .map(|artifact| artifact.authorization())
                .collect(),
        )
    }

    pub(crate) fn import_authority(&self) -> CheckedValueImportAuthority {
        CheckedValueImportAuthority::capture(self.baseline.iter().map(AsRef::as_ref))
    }

    pub(crate) fn retain_diagnostics(
        &self,
        prefix: Option<&ExactCompiledPrefix>,
        destination: &Path,
    ) -> std::io::Result<()> {
        let inputs = match prefix {
            Some(prefix) => prefix.value_artifacts().map_err(std::io::Error::other)?,
            None => self
                .baseline
                .iter()
                .map(|artifact| (artifact.module.as_str(), artifact.as_ref()))
                .collect(),
        };
        let destination = destination.join("checked-value-inputs");
        std::fs::create_dir(&destination)?;
        let mut captured = Vec::with_capacity(inputs.len());
        for artifact in inputs.into_values() {
            let relative = artifact.owner.relative_hi_path();
            let expected = destination.join("expected").join(&relative);
            std::fs::create_dir_all(expected.parent().expect("generated interface parent"))?;
            std::fs::write(expected, &artifact.bytes)?;
            let (observed_digest, observation_error) = match read(&artifact.path, 32 * 1024 * 1024)
            {
                Ok(bytes) => {
                    let observed = destination.join("observed").join(&relative);
                    std::fs::create_dir_all(
                        observed.parent().expect("generated interface parent"),
                    )?;
                    std::fs::write(observed, &bytes)?;
                    (Some(hash(&bytes)), None)
                }
                Err(error) => (None, Some(error.to_string())),
            };
            captured.push(serde_json::json!({
                "module": artifact.module,
                "original_path": artifact.path,
                "relative_path": relative,
                "expected_sha256": artifact.digest,
                "observed_sha256": observed_digest,
                "observation_error": observation_error,
            }));
        }
        std::fs::write(
            destination.join("inputs.json"),
            serde_json::to_vec(&captured).map_err(std::io::Error::other)?,
        )
    }

    fn capture_output(
        &self,
        generation: u64,
        cell: &ExactCheckedCell,
        context: &Arc<crate::declaration_context::ExactDeclarationContext>,
        source_lexical: &[crate::declaration_join::ExactLexicalNode],
    ) -> Result<Arc<CheckedValueArtifact>, CompileError> {
        let owner = tidepool_repr::SessionModule::val(tidepool_repr::Generation(generation));
        let path = self.root().join(owner.relative_hi_path());
        let bytes: Arc<[u8]> = read(&path, 32 * 1024 * 1024)?.into();
        let digest = hash(&bytes);
        let certificate = certify_value_interface(cell.producer, owner, &path, &bytes)?;
        let context = (**context).clone().extend_with_value_interfaces(
            std::slice::from_ref(&certificate),
            context.lexical_graph().to_vec(),
        )?;
        let value_view = context
            .artifact_view()
            .select_roots(vec![certificate.artifact_id()])?;
        let (artifact_view, source_lexical) =
            context.retain_value_source_surface(&value_view, source_lexical)?;
        self.output_files_hashed.fetch_add(1, Ordering::Relaxed);
        self.output_bytes_hashed
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Ok(Arc::new(CheckedValueArtifact {
            owner,
            module: owner.module_name(),
            digest,
            bytes,
            path,
            authority: Some((cell.producer, cell.receipt_digest)),
            certified_interface: Some(certificate),
            artifact_view: Some(artifact_view),
            source_lexical,
        }))
    }
}

impl CheckedValueArtifact {
    pub fn owner(&self) -> tidepool_repr::SessionModule {
        self.owner
    }
    pub fn bytes_owned(&self) -> &Arc<[u8]> {
        &self.bytes
    }
    pub fn is_checked_output(&self) -> bool {
        self.authority.is_some()
    }

    pub fn certified_interface(
        &self,
    ) -> Option<&Arc<crate::recovery_artifacts::CertifiedValueInterface>> {
        self.certified_interface.as_ref()
    }

    pub(crate) fn artifact_view(
        &self,
    ) -> Result<&crate::artifact_inventory::ArtifactView, CompileError> {
        self.artifact_view
            .as_ref()
            .ok_or_else(|| failure("value interface has no original artifact closure"))
    }

    pub(crate) fn source_lexical(&self) -> &[crate::declaration_join::ExactLexicalNode] {
        &self.source_lexical
    }

    fn authorization(&self) -> Value {
        array([
            text("main"),
            text(&self.module),
            text(self.path.to_string_lossy()),
            text(&self.digest),
        ])
    }
}

pub(crate) fn certify_value_interface(
    producer: [u8; 32],
    owner: tidepool_repr::SessionModule,
    path: &Path,
    bytes: &[u8],
) -> Result<Arc<crate::recovery_artifacts::CertifiedValueInterface>, CompileError> {
    let requirements = decode(&read(path.with_extension("hi.requirements"), 4 << 20)?)?;
    let requirements = list(&requirements, 16384)?
        .iter()
        .map(|value| {
            let fields = row(value, 2)?;
            Ok(crate::declaration_join::ExactModuleIdentity {
                unit: string(&fields[0])?.to_owned(),
                module: string(&fields[1])?.to_owned(),
            })
        })
        .collect::<Result<Vec<_>, CompileError>>()?;
    Ok(Arc::new(
        crate::recovery_artifacts::CertifiedValueInterface::from_checked_compilation(
            producer,
            crate::declaration_join::ExactModuleIdentity {
                unit: "main".into(),
                module: owner.module_name(),
            },
            bytes.to_vec(),
            read(path.with_extension("hi.packages"), 4 << 20)?,
            requirements,
        )
        .map_err(failure)?,
    ))
}

#[derive(Debug)]
pub(crate) struct PlannedCheckedDeclaration {
    pub(crate) source: String,
    pub(crate) interface_fingerprint: String,
    pub(crate) certificate: Arc<crate::declaration_join::CertifiedAuthoredDeclaration>,
    pub(crate) receipt_digest: [u8; 32],
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
        if self.producer
            != crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(producer)
                .sha256()
            || &self.context != context
            || self
                .evidence
                .iter()
                .any(|(source, evidence)| !evidence.valid(source))
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckedExpressionLift {
    Pure,
    Effectful,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckedExpressionPresentation {
    Rendered,
    Opaque,
}

/// Compiler proofs for an ordered prefix. Runtime completion retains this
/// value only after the corresponding native installation and execution.
#[derive(Clone, Debug)]
pub struct ExactCompiledPrefix {
    cell: Arc<ExactCheckedCell>,
    completed: CheckedPrefixSequence<CompletedCheckedItem>,
    displays: CheckedPrefixSequence<Arc<ExactCompiledDisplay>>,
}

/// Immutable ordered proof links. Appending shares the historical leaves and
/// copies only a logarithmic branch path; snapshots never clone the full prefix.
#[derive(Clone, Debug)]
struct CheckedPrefixSequence<T> {
    root: Option<Arc<CheckedPrefixNode<T>>>,
}

#[derive(Debug)]
struct CheckedPrefixNode<T> {
    length: usize,
    contents: CheckedPrefixContents<T>,
}

#[derive(Debug)]
enum CheckedPrefixContents<T> {
    Leaf(T),
    Branch(Arc<CheckedPrefixNode<T>>, Arc<CheckedPrefixNode<T>>),
}

impl<T> CheckedPrefixSequence<T> {
    fn new() -> Self {
        Self { root: None }
    }
    fn len(&self) -> usize {
        self.root.as_ref().map_or(0, |root| root.length)
    }
    fn push(&mut self, value: T) {
        fn branch<T>(
            left: Arc<CheckedPrefixNode<T>>,
            right: Arc<CheckedPrefixNode<T>>,
        ) -> Arc<CheckedPrefixNode<T>> {
            Arc::new(CheckedPrefixNode {
                length: left.length + right.length,
                contents: CheckedPrefixContents::Branch(left, right),
            })
        }
        fn append<T>(
            node: Arc<CheckedPrefixNode<T>>,
            leaf: Arc<CheckedPrefixNode<T>>,
        ) -> Arc<CheckedPrefixNode<T>> {
            if node.length.is_power_of_two() {
                branch(node, leaf)
            } else {
                let CheckedPrefixContents::Branch(left, right) = &node.contents else {
                    unreachable!("non-power-of-two prefix is a branch")
                };
                branch(left.clone(), append(right.clone(), leaf))
            }
        }
        let leaf = Arc::new(CheckedPrefixNode {
            length: 1,
            contents: CheckedPrefixContents::Leaf(value),
        });
        self.root = Some(
            self.root
                .take()
                .map_or_else(|| leaf.clone(), |root| append(root, leaf.clone())),
        );
    }
    fn get(&self, mut index: usize) -> Option<&T> {
        let mut node = self.root.as_deref()?;
        if index >= node.length {
            return None;
        }
        loop {
            match &node.contents {
                CheckedPrefixContents::Leaf(value) => return Some(value),
                CheckedPrefixContents::Branch(left, right) => {
                    if index < left.length {
                        node = left;
                    } else {
                        index -= left.length;
                        node = right;
                    }
                }
            }
        }
    }
    fn iter(&self) -> CheckedPrefixIter<'_, T> {
        CheckedPrefixIter {
            pending: self.root.as_deref().into_iter().collect(),
        }
    }
}

struct CheckedPrefixIter<'a, T> {
    pending: Vec<&'a CheckedPrefixNode<T>>,
}
impl<'a, T> Iterator for CheckedPrefixIter<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<Self::Item> {
        while let Some(node) = self.pending.pop() {
            match &node.contents {
                CheckedPrefixContents::Leaf(value) => return Some(value),
                CheckedPrefixContents::Branch(left, right) => {
                    self.pending.push(right);
                    self.pending.push(left);
                }
            }
        }
        None
    }
}
impl<'a, T> IntoIterator for &'a CheckedPrefixSequence<T> {
    type Item = &'a T;
    type IntoIter = CheckedPrefixIter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[derive(Clone, Debug)]
enum CompletedCheckedItem {
    Native(Arc<ExactCompiledItem>),
    Declaration(ExactCheckedItem),
}
impl CompletedCheckedItem {
    fn native(&self) -> Option<&Arc<ExactCompiledItem>> {
        match self {
            Self::Native(item) => Some(item),
            Self::Declaration(_) => None,
        }
    }
}

type SettledNativeBinding = (
    String,
    tidepool_repr::execution_schema::SymbolIdentity,
    u64,
    u64,
);

#[derive(Clone, Debug, Default)]
pub(crate) struct CheckedSettledValues {
    rows: Vec<SettledNativeBinding>,
    imports: Vec<(String, Vec<String>)>,
    authorization: Vec<Value>,
}

impl CheckedSettledValues {
    fn validate_required<'a>(
        &self,
        target: &tidepool_repr::execution_schema::PreparedProgram,
        actual: impl IntoIterator<
            Item = (
                &'a str,
                &'a tidepool_repr::execution_schema::SymbolIdentity,
                u64,
                u64,
            ),
        >,
    ) -> Result<(), CompileError> {
        let actual = actual.into_iter().collect::<Vec<_>>();
        for global in target.globals() {
            let Some((name, identity, generation, identifier)) = self
                .rows
                .iter()
                .find(|(_, identity, _, _)| identity == &global.identity)
            else {
                continue;
            };
            if !actual.iter().any(
                |(actual_name, actual_identity, actual_generation, actual_id)| {
                    *actual_name == name
                        && *actual_identity == identity
                        && *actual_generation == *generation
                        && *actual_id == *identifier
                },
            ) {
                return Err(failure(
                    "prepared import lacks its exact completed native binding",
                ));
            }
        }
        Ok(())
    }
    fn validate<'a>(
        &self,
        actual: impl IntoIterator<
            Item = (
                &'a str,
                &'a tidepool_repr::execution_schema::SymbolIdentity,
                u64,
                u64,
            ),
        >,
    ) -> Result<(), CompileError> {
        let mut actual = actual.into_iter();
        for (name, identity, generation, id) in &self.rows {
            let Some((actual_name, actual_identity, actual_generation, actual_id)) = actual.next()
            else {
                return Err(failure(
                    "compiled lexical selection lacks actual protected native settlement",
                ));
            };
            if name != actual_name
                || identity != actual_identity
                || *generation != actual_generation
                || *id != actual_id
            {
                return Err(failure(
                    "compiled lexical selection differs from actual protected native settlement",
                ));
            }
        }
        if actual.next().is_some() {
            return Err(failure(
                "compiled lexical selection omits actual protected native settlement",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
enum CheckedExecutionAdmission {
    InitialFold([u8; 32]),
    RuntimeItem([u8; 32]),
    CellProgram([u8; 32]),
    HostActivationInput([u8; 32]),
}

/// A checked recipe and its exact prepared target, issued together by the
/// product-sealing entry point. Compiling alone makes no execution claim.
#[derive(Debug)]
pub struct ExactCompiledItem {
    item: ExactCheckedItem,
    target: Arc<tidepool_repr::execution_schema::PreparedProgram>,
    table: tidepool_repr::DataConTable,
    yield_sites_digest: [u8; 32],
    value_interface: Option<Arc<CheckedValueArtifact>>,
    generation: u64,
    bound_binders: Vec<Value>,
    observation_name: Option<String>,
    admission: CheckedExecutionAdmission,
    settled_values: CheckedSettledValues,
}

/// A compiler-issued host input interface and preview target. It cannot enter
/// an authored checked prefix or settle the placeholder used to infer its type.
#[derive(Debug)]
pub struct ExactCompiledActivationInput {
    compiled: Arc<ExactCompiledItem>,
    input_type_witness: CanonicalInputTypeWitness,
}

impl ExactCompiledActivationInput {
    pub fn validate_yield_sites(&self, sites: &[crate::YieldSite]) -> Result<(), CompileError> {
        self.compiled.validate_yield_sites(sites)
    }
    pub fn input_type_witness(&self) -> &CanonicalInputTypeWitness {
        &self.input_type_witness
    }
    pub fn item(&self) -> &ExactCheckedItem {
        self.compiled.item()
    }
    pub fn generation(&self) -> u64 {
        self.compiled.generation()
    }
    pub fn target_owned(&self) -> Arc<tidepool_repr::execution_schema::PreparedProgram> {
        self.compiled.target_owned()
    }
    pub fn matches_target(
        &self,
        target: &tidepool_repr::execution_schema::PreparedProgram,
    ) -> bool {
        self.compiled.matches_target(target)
    }
    pub fn validate_table(&self, table: &tidepool_repr::DataConTable) -> Result<(), CompileError> {
        self.compiled.validate_table(table)
    }
    pub fn validate_bound_binders(&self, bound: &[Value]) -> Result<(), CompileError> {
        self.compiled.validate_bound_binders(bound)
    }
    pub fn validate_runtime_admission(
        &self,
        item_digest: [u8; 32],
        cell_digest: [u8; 32],
    ) -> Result<(), CompileError> {
        if !matches!(self.compiled.admission, CheckedExecutionAdmission::HostActivationInput(digest)
            if digest == item_digest && self.item().admission_digest() == cell_digest)
        {
            return Err(failure(
                "compiled activation input has another protected runtime admission",
            ));
        }
        Ok(())
    }
    pub fn value_interface_certificate(&self) -> Option<Arc<CheckedValueArtifact>> {
        self.compiled.value_interface_certificate()
    }
    pub fn validate_settled_native_bindings<'a>(
        &self,
        actual: impl IntoIterator<
            Item = (
                &'a str,
                &'a tidepool_repr::execution_schema::SymbolIdentity,
                u64,
                u64,
            ),
        >,
    ) -> Result<(), CompileError> {
        self.compiled.validate_settled_native_bindings(actual)
    }
}

#[derive(Debug)]
pub struct ExactCompiledDisplay {
    capture: Arc<ExactCompiledItem>,
    target: Arc<tidepool_repr::execution_schema::PreparedProgram>,
    table: tidepool_repr::DataConTable,
    yield_sites_digest: [u8; 32],
    generation: u64,
    admission_digest: [u8; 32],
    program_admission: bool,
    bound_binders: Vec<Value>,
    value_interface: Arc<CheckedValueArtifact>,
    settled_values: CheckedSettledValues,
}

impl ExactCompiledDisplay {
    pub fn validate_yield_sites(&self, sites: &[crate::YieldSite]) -> Result<(), CompileError> {
        if crate::artifacts::yield_sites_metadata_digest(sites)? != self.yield_sites_digest {
            return Err(failure(
                "compiled typed-site metadata was edited before installation",
            ));
        }
        Ok(())
    }
    pub fn target_owned(&self) -> Arc<tidepool_repr::execution_schema::PreparedProgram> {
        self.target.clone()
    }
    pub fn validate_runtime_admission(
        &self,
        display_digest: [u8; 32],
        cell_digest: [u8; 32],
    ) -> Result<(), CompileError> {
        if self.admission_digest != display_digest
            && !(self.program_admission && self.admission_digest == cell_digest)
        {
            return Err(failure("display belongs to another runtime admission"));
        }
        Ok(())
    }
    pub fn validate_settled_native_bindings<'a>(
        &self,
        actual: impl IntoIterator<
            Item = (
                &'a str,
                &'a tidepool_repr::execution_schema::SymbolIdentity,
                u64,
                u64,
            ),
        >,
    ) -> Result<(), CompileError> {
        if self.program_admission {
            self.settled_values.validate_required(&self.target, actual)
        } else {
            self.settled_values.validate(actual)
        }
    }
    pub fn target_definition_identities(
        &self,
    ) -> impl Iterator<Item = &tidepool_repr::execution_schema::SymbolIdentity> {
        target_definition_identities(&self.target)
    }
    pub fn bound_binder_identities(&self) -> impl Iterator<Item = (&str, u64)> {
        bound_binder_identities(&self.bound_binders)
    }
    pub fn validate_table(&self, table: &tidepool_repr::DataConTable) -> Result<(), CompileError> {
        if table != &self.table {
            return Err(failure("compiled display constructor metadata was edited"));
        }
        Ok(())
    }
    pub fn value_interface_owned(&self) -> (&str, &Arc<[u8]>) {
        (&self.value_interface.module, &self.value_interface.bytes)
    }
    pub fn value_interface_certificate(&self) -> Arc<CheckedValueArtifact> {
        self.value_interface.clone()
    }
    pub fn capture(&self) -> &Arc<ExactCompiledItem> {
        &self.capture
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn admission_digest(&self) -> [u8; 32] {
        self.admission_digest
    }
    pub fn validate_bound_binders(&self, bound: &[Value]) -> Result<(), CompileError> {
        if bound != self.bound_binders {
            return Err(failure("compiled display binder metadata was edited"));
        }
        Ok(())
    }
    pub fn matches_target(
        &self,
        target: &tidepool_repr::execution_schema::PreparedProgram,
    ) -> bool {
        std::ptr::eq(self.target.as_ref(), target) || self.target.as_ref() == target
    }
}

pub(crate) struct CheckedDisplayOffer {
    pub(crate) capture: Arc<ExactCompiledItem>,
    pub(crate) prefix: ExactCompiledPrefix,
    pub(crate) generation: u64,
    pub(crate) admission_digest: [u8; 32],
    pub(crate) budget: u64,
    pub(crate) presented: Vec<String>,
    pub(crate) settled_values: CheckedSettledValues,
    pub(crate) is_program: bool,
}

impl CheckedDisplayOffer {
    pub(crate) fn authorization(
        &self,
        producer: &[u8],
        context: [u8; 32],
    ) -> Result<Value, CompileError> {
        self.prefix.revalidate_context(producer, &context)?;
        if self.admission_digest == [0; 32]
            || self
                .prefix
                .completed_item(self.capture.item.index())
                .is_none_or(|completed| !Arc::ptr_eq(completed, &self.capture))
        {
            return Err(failure(
                "display has no protected same-cell completed capture",
            ));
        }
        let observation = self
            .capture
            .observation_name()
            .ok_or_else(|| failure("display item is not an observation capture"))?;
        let presentation = self
            .capture
            .item
            .expression_presentation()?
            .ok_or_else(|| failure("display has no checked presentation"))?;
        Ok(array([
            text("checked-display"),
            text(hex(&self.capture.item.admission_digest())),
            text(hex(&self.capture.item.cell.receipt_digest)),
            Value::Integer((self.capture.item.index as u64).into()),
            text(observation),
            Value::Integer(self.capture.generation.into()),
            Value::Integer(self.generation.into()),
            text(hex(&self.admission_digest)),
            Value::Integer(self.budget.into()),
            Value::Array(self.presented.iter().map(text).collect()),
            Value::Array(
                self.capture
                    .item
                    .turn_templates()
                    .iter()
                    .map(|(kind, source)| array([text(kind), text(hash(source.as_bytes()))]))
                    .collect(),
            ),
            Value::Array(self.prefix.injected_modules().iter().map(text).collect()),
            Value::Array(
                self.settled_values
                    .imports
                    .iter()
                    .map(|(module, names)| {
                        array([text(module), Value::Array(names.iter().map(text).collect())])
                    })
                    .collect(),
            ),
            text(match presentation {
                CheckedExpressionPresentation::Rendered => "rendered",
                CheckedExpressionPresentation::Opaque => "opaque",
            }),
            self.prefix.planned_authorization(),
            Value::Array(self.settled_values.authorization.clone()),
            self.prefix.value_interface_authorization()?,
        ]))
    }
    pub(crate) fn seal(
        &self,
        root: &Path,
        request: &str,
        source: &str,
        target: &Arc<tidepool_repr::execution_schema::PreparedProgram>,
        artifact_context: &Arc<crate::declaration_context::ExactDeclarationContext>,
        source_lexical: &[crate::declaration_join::ExactLexicalNode],
    ) -> Result<Arc<ExactCompiledDisplay>, CompileError> {
        let receipt = decode(&read(root.join("checked-display.cbor"), 4 * 1024 * 1024)?)?;
        let fields = row(&receipt, 8)?;
        if string(&fields[0])? != "TPEXACTDISPLAY"
            || string(&fields[1])? != "1"
            || string(&fields[2])? != request
            || string(&fields[3])?
                != hex(&if self.is_program {
                    self.capture.item.admission_digest()
                } else {
                    self.capture.item.cell.receipt_digest
                })
            || fields[4] != Value::Integer((self.capture.item.index as u64).into())
            || string(&fields[5])? != hex(&self.admission_digest)
            || string(&fields[6])? != hash(source.as_bytes())
            || string(&fields[7])? != "tidepool-display-recipe-1"
        {
            return Err(failure(
                "display recipe differs from its completed capture offer",
            ));
        }
        let turn = decode(&read(root.join("turn.cbor"), 32 * 1024 * 1024)?)?;
        let turn = row(&turn, 2)?;
        if string(&turn[0])? != "Bind" {
            return Err(failure("display recipe is not its binding bundle"));
        }
        let fields = row(&turn[1], 5)?;
        let names = [
            format!("__tidepoolPage{}", self.generation),
            format!("__tidepoolMetadata{}", self.generation),
            "cellDisplay".into(),
        ];
        if fields[0] != Value::Array(names.iter().map(text).collect())
            || string(&fields[4])? != source
        {
            return Err(failure("display bundle names or wrapper changed"));
        }
        let yield_sites_digest = crate::artifacts::yield_sites_metadata_digest(
            &crate::turn_observations::decode_turn_yield_sites(&fields[3])?,
        )?;
        let bound = list(&fields[2], 3)?.to_vec();
        let module = tidepool_repr::SessionModule::val(tidepool_repr::Generation(self.generation))
            .module_name();
        if bound.len() != 3 {
            return Err(failure("display binder inventory differs"));
        }
        for (binder, name) in bound.iter().zip(&names) {
            let fields = row(binder, 7)?;
            if string(&fields[0])? != name || string(&fields[2])? != module {
                return Err(failure("display binder has another reserved native owner"));
            }
        }
        let interface = self.capture.item.cell.value_inputs.capture_output(
            self.generation,
            &self.capture.item.cell,
            artifact_context,
            source_lexical,
        )?;
        Ok(Arc::new(ExactCompiledDisplay {
            capture: self.capture.clone(),
            target: target.clone(),
            table: read_table(root)?,
            yield_sites_digest,
            generation: self.generation,
            admission_digest: self.admission_digest,
            program_admission: self.is_program,
            bound_binders: bound,
            value_interface: interface,
            settled_values: self.settled_values.clone(),
        }))
    }
}

impl ExactCompiledItem {
    pub fn validate_yield_sites(&self, sites: &[crate::YieldSite]) -> Result<(), CompileError> {
        if crate::artifacts::yield_sites_metadata_digest(sites)? != self.yield_sites_digest {
            return Err(failure(
                "compiled typed-site metadata was edited before installation",
            ));
        }
        Ok(())
    }
    pub fn target_owned(&self) -> Arc<tidepool_repr::execution_schema::PreparedProgram> {
        self.target.clone()
    }
    pub fn validate_runtime_admission(
        &self,
        item_digest: [u8; 32],
        cell_digest: [u8; 32],
    ) -> Result<(), CompileError> {
        let valid = match self.admission {
            CheckedExecutionAdmission::HostActivationInput(_) => false,
            CheckedExecutionAdmission::RuntimeItem(digest) => digest == item_digest,
            CheckedExecutionAdmission::CellProgram(digest) => digest == cell_digest,
            CheckedExecutionAdmission::InitialFold(digest) => {
                digest == cell_digest
                    && self.item.index() == 0
                    && self.item.kind() == CheckedItemKind::Bind
                    && self.settled_values.rows.is_empty()
            }
        };
        if !valid {
            return Err(failure(
                "compiled item has another protected runtime admission",
            ));
        }
        Ok(())
    }
    pub fn validate_settled_native_bindings<'a>(
        &self,
        actual: impl IntoIterator<
            Item = (
                &'a str,
                &'a tidepool_repr::execution_schema::SymbolIdentity,
                u64,
                u64,
            ),
        >,
    ) -> Result<(), CompileError> {
        if matches!(self.admission, CheckedExecutionAdmission::CellProgram(_)) {
            self.settled_values.validate_required(&self.target, actual)
        } else {
            self.settled_values.validate(actual)
        }
    }
    pub fn target_definition_identities(
        &self,
    ) -> impl Iterator<Item = &tidepool_repr::execution_schema::SymbolIdentity> {
        target_definition_identities(&self.target)
    }
    pub fn bound_binder_identities(&self) -> impl Iterator<Item = (&str, u64)> {
        bound_binder_identities(&self.bound_binders)
    }
    /// Only same-check compiled native Value rows may overlay declaration
    /// spellings in their owning private scope. Their declaration context and
    /// qualified original owners remain unchanged.
    pub fn private_value_overlay_binders(&self) -> impl Iterator<Item = &str> {
        self.bound_binders.iter().map(|binder| {
            let fields = row(binder, 7).expect("sealed native binder row");
            string(&fields[0]).expect("sealed native binder name")
        })
    }
    pub fn validate_table(&self, table: &tidepool_repr::DataConTable) -> Result<(), CompileError> {
        if table != &self.table {
            return Err(failure("compiled item constructor metadata was edited"));
        }
        Ok(())
    }
    pub fn shares_target(
        &self,
        target: &Arc<tidepool_repr::execution_schema::PreparedProgram>,
    ) -> bool {
        Arc::ptr_eq(&self.target, target)
    }
    pub fn observation_name(&self) -> Option<&str> {
        self.observation_name.as_deref()
    }
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
        std::ptr::eq(self.target.as_ref(), target) || self.target.as_ref() == target
    }
    pub fn value_interface(&self) -> Option<(&str, &[u8])> {
        self.value_interface
            .as_ref()
            .map(|artifact| (artifact.module.as_str(), artifact.bytes.as_ref()))
    }
    pub fn value_interface_owned(&self) -> Option<(&str, &Arc<[u8]>)> {
        self.value_interface
            .as_ref()
            .map(|artifact| (artifact.module.as_str(), &artifact.bytes))
    }
    pub fn value_interface_certificate(&self) -> Option<Arc<CheckedValueArtifact>> {
        self.value_interface.clone()
    }
}

impl ExactCompiledPrefix {
    pub(crate) fn with_initial_value_context(
        &self,
        current: Arc<crate::declaration_context::ExactDeclarationContext>,
    ) -> Result<Arc<crate::declaration_context::ExactDeclarationContext>, CompileError> {
        Ok(Arc::new(
            (*current).clone().extend_checked_value_input_context(
                &self.cell.declaration_context,
                self.cell
                    .value_inputs
                    .baseline
                    .iter()
                    .map(|artifact| (artifact.owner, artifact.bytes.as_ref())),
            )?,
        ))
    }

    fn planned_authorization(&self) -> Value {
        self.completed_declaration(0)
            .and_then(|item| item.cell.planned_declaration.as_ref())
            .map_or(Value::Null, |planned| {
                array([
                    text(&planned.certificate.product().owner().unit),
                    text(&planned.certificate.product().owner().module),
                    text(&planned.interface_fingerprint),
                ])
            })
    }
    fn revalidate_context(&self, producer: &[u8], context: &[u8; 32]) -> Result<(), CompileError> {
        self.cell.revalidate(producer, &self.cell.context)?;
        let Some(item) = self.completed_declaration(0) else {
            return if context == &self.cell.context {
                Ok(())
            } else {
                Err(failure("completed prefix has another declaration context"))
            };
        };
        let certificate = item
            .planned_declaration()
            .ok_or_else(|| failure("completed declaration has no original certificate"))?;
        // The cell's exact compiler context also contains temporary retained
        // value lexical authority. Revalidation reconstructs the declaration
        // publication surface from the original declaration baseline and the
        // certificate's reachable source closure, as publication does.
        let baseline = &self.cell.publication_context;
        let inherited = baseline
            .lexical_graph()
            .iter()
            .filter(|node| !node.owner.module.starts_with("Tidepool.Session."))
            .cloned()
            .collect::<Vec<_>>();
        let surface = certificate.shared_source_lexical_surface(&inherited)?;
        let mut lexical = surface
            .lexical
            .into_iter()
            .map(|node| (node.owner, node.imports))
            .collect::<BTreeMap<_, _>>();
        let mut roots = baseline
            .lexical_graph()
            .iter()
            .filter(|node| node.owner.module.starts_with("Tidepool.Session."))
            .flat_map(|node| node.imports.iter().cloned())
            .collect::<Vec<_>>();
        roots.extend(surface.roots);
        roots.sort();
        roots.dedup();
        let owner = crate::declaration_join::ExactModuleIdentity {
            unit: certificate.product().owner().unit.clone(),
            module: certificate.product().owner().module.clone(),
        };
        lexical.insert(owner, roots);
        let expected = (**baseline).clone().extend(
            std::slice::from_ref(certificate),
            &[],
            lexical
                .into_iter()
                .map(
                    |(owner, imports)| crate::declaration_join::ExactLexicalNode { owner, imports },
                )
                .collect(),
        )?;
        if &expected.semantic_sha256() != context {
            return Err(failure(
                "completed declaration context differs from its same original certificate",
            ));
        }
        Ok(())
    }
    pub fn append_display(&self, display: Arc<ExactCompiledDisplay>) -> Result<Self, CompileError> {
        if self
            .completed_item(display.capture.item.index())
            .is_none_or(|capture| !Arc::ptr_eq(capture, &display.capture))
            || self
                .displays
                .iter()
                .any(|prior| prior.generation == display.generation)
        {
            return Err(failure(
                "display is not a new bundle of its same completed capture",
            ));
        }
        let mut next = self.clone();
        next.displays.push(display);
        Ok(next)
    }
    pub fn completed_item(&self, index: usize) -> Option<&Arc<ExactCompiledItem>> {
        self.completed
            .get(index)
            .and_then(CompletedCheckedItem::native)
    }
    pub fn completed_declaration(&self, index: usize) -> Option<&ExactCheckedItem> {
        match self.completed.get(index) {
            Some(CompletedCheckedItem::Declaration(item)) => Some(item),
            _ => None,
        }
    }
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
        next.completed.push(CompletedCheckedItem::Native(completed));
        Ok(next)
    }
    pub fn append_declaration(&self, item: ExactCheckedItem) -> Result<Self, CompileError> {
        if item.index != self.next_item()
            || !Arc::ptr_eq(&item.cell, &self.cell)
            || item.planned_declaration().is_none()
        {
            return Err(failure(
                "declaration is not the next original certified item of this cell",
            ));
        }
        let mut next = self.clone();
        next.completed.push(CompletedCheckedItem::Declaration(item));
        Ok(next)
    }
    pub fn injected_modules(&self) -> Vec<String> {
        self.cell
            .specification
            .injected_modules
            .iter()
            .cloned()
            .chain(
                self.completed
                    .iter()
                    .filter_map(CompletedCheckedItem::native)
                    .filter_map(|completed| {
                        completed
                            .value_interface
                            .as_ref()
                            .map(|artifact| artifact.module.clone())
                    }),
            )
            .chain(
                self.displays
                    .iter()
                    .map(|display| display.value_interface.module.clone()),
            )
            .collect()
    }
    pub fn completed_interfaces(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.completed
            .iter()
            .filter_map(CompletedCheckedItem::native)
            .filter_map(|completed| completed.value_interface())
            .chain(self.displays.iter().map(|display| {
                (
                    display.value_interface.module.as_str(),
                    display.value_interface.bytes.as_ref(),
                )
            }))
    }
    fn value_artifacts(&self) -> Result<BTreeMap<&str, &CheckedValueArtifact>, CompileError> {
        let mut inputs = self
            .cell
            .value_inputs
            .baseline
            .iter()
            .map(|artifact| (artifact.module.as_str(), artifact.as_ref()))
            .collect::<BTreeMap<_, _>>();
        for item in self
            .completed
            .iter()
            .filter_map(CompletedCheckedItem::native)
        {
            if let Some(artifact) = &item.value_interface {
                if inputs.insert(&artifact.module, artifact).is_some() {
                    return Err(failure("duplicate checked value interface"));
                }
            }
        }
        for display in &self.displays {
            let artifact = &display.value_interface;
            if inputs.insert(&artifact.module, artifact).is_some() {
                return Err(failure("duplicate checked display interface"));
            }
        }
        Ok(inputs)
    }
    fn value_interface_authorization(&self) -> Result<Value, CompileError> {
        Ok(Value::Array(
            self.value_artifacts()?
                .into_values()
                .map(CheckedValueArtifact::authorization)
                .collect(),
        ))
    }

    pub(crate) fn import_authority(&self) -> Result<CheckedValueImportAuthority, CompileError> {
        Ok(CheckedValueImportAuthority::capture(
            self.value_artifacts()?.into_values(),
        ))
    }
    pub(crate) fn prepared_value_selection(&self) -> Result<CheckedSettledValues, CompileError> {
        // Requirements keep every original qualified binder, including names
        // shadowed by a later declaration or Val. Runtime leases retain those
        // owners independently of the authored lexical selection.
        let mut winners = BTreeMap::new();
        for completed in &self.completed {
            match completed {
                CompletedCheckedItem::Declaration(_) => {}
                CompletedCheckedItem::Native(item) => {
                    for binder in &item.bound_binders {
                        let fields = row(binder, 7)?;
                        let name = string(&fields[0])?.to_owned();
                        let identity = tidepool_repr::execution_schema::SymbolIdentity {
                            unit: "main".into(),
                            module: string(&fields[2])?.to_owned(),
                            namespace: "value".into(),
                            occurrence: name.clone(),
                            record_parent: None,
                        };
                        let Value::Integer(identifier) = fields[1] else {
                            return Err(failure("binder id is not integer"));
                        };
                        winners.insert(
                            (identity.module.clone(), name.clone()),
                            (
                                name,
                                identity,
                                item.generation,
                                u64::try_from(identifier).map_err(failure)?,
                            ),
                        );
                    }
                }
            }
        }
        for display in &self.displays {
            for (index, binder) in display.bound_binders.iter().enumerate() {
                if index == 1 {
                    continue;
                }
                let fields = row(binder, 7)?;
                let name = string(&fields[0])?.to_owned();
                let identity = tidepool_repr::execution_schema::SymbolIdentity {
                    unit: "main".into(),
                    module: string(&fields[2])?.to_owned(),
                    namespace: "value".into(),
                    occurrence: name.clone(),
                    record_parent: None,
                };
                let Value::Integer(identifier) = fields[1] else {
                    return Err(failure("binder id is not integer"));
                };
                winners.insert(
                    (identity.module.clone(), name.clone()),
                    (
                        name,
                        identity,
                        display.generation,
                        u64::try_from(identifier).map_err(failure)?,
                    ),
                );
            }
        }
        self.select_settled_values(winners.into_values().collect())
    }

    pub(crate) fn select_settled_values(
        &self,
        rows: Vec<SettledNativeBinding>,
    ) -> Result<CheckedSettledValues, CompileError> {
        let mut sealed = BTreeMap::new();
        let native = self
            .completed
            .iter()
            .filter_map(CompletedCheckedItem::native)
            .filter_map(|item| {
                item.value_interface.as_ref().map(|artifact| {
                    (
                        item.generation,
                        artifact,
                        item.bound_binders.as_slice(),
                        false,
                    )
                })
            });
        let displays = self.displays.iter().map(|display| {
            (
                display.generation,
                &display.value_interface,
                display.bound_binders.as_slice(),
                true,
            )
        });
        for (generation, artifact, binders, display) in native.chain(displays) {
            for (index, binder) in binders.iter().enumerate() {
                // The display metadata row is consumed by rendering, never a lexical binding.
                if display && index == 1 {
                    continue;
                }
                let fields = row(binder, 7)?;
                let name = string(&fields[0])?;
                let id = match &fields[1] {
                    Value::Integer(id) => u64::try_from(*id)
                        .map_err(|_| failure("sealed binder identity is not unsigned"))?,
                    _ => return Err(failure("sealed binder identity is not an integer")),
                };
                if sealed
                    .insert((artifact.module.as_str(), name), (generation, id, artifact))
                    .is_some()
                {
                    return Err(failure("duplicate sealed completed native binding"));
                }
            }
        }
        let mut seen = BTreeSet::new();
        let mut winners = BTreeMap::new();
        for (name, identity, generation, id) in &rows {
            if identity.unit != "main"
                || identity.namespace != "value"
                || identity.record_parent.is_some()
                || identity.occurrence != *name
                || !seen.insert((identity, generation, id))
            {
                return Err(failure(
                    "actual native selection has a duplicate or foreign identity",
                ));
            }
            let (expected_generation, expected_id, artifact) = sealed
                .get(&(identity.module.as_str(), name.as_str()))
                .ok_or_else(|| failure("actual native selection has no same-cell sealed binder"))?;
            if generation != expected_generation || id != expected_id {
                return Err(failure(
                    "actual native selection differs from its sealed binder",
                ));
            }
            winners.insert(name.as_str(), (*artifact, *id));
        }
        let mut by_module = BTreeMap::<&str, (&CheckedValueArtifact, Vec<(&str, u64)>)>::new();
        for (name, (artifact, id)) in winners {
            by_module
                .entry(&artifact.module)
                .or_insert_with(|| (artifact, Vec::new()))
                .1
                .push((name, id));
        }
        let imports = by_module
            .iter()
            .map(|(module, (_, names))| {
                (
                    (*module).to_owned(),
                    names.iter().map(|(name, _)| (*name).to_owned()).collect(),
                )
            })
            .collect();
        let authorization = by_module
            .into_iter()
            .map(|(module, (artifact, names))| {
                array([
                    text("main"),
                    text(module),
                    text(artifact.path.to_string_lossy()),
                    text(&artifact.digest),
                    Value::Array(
                        names
                            .into_iter()
                            .map(|(name, id)| array([text(name), Value::Integer(id.into())]))
                            .collect(),
                    ),
                ])
            })
            .collect();
        Ok(CheckedSettledValues {
            rows,
            imports,
            authorization,
        })
    }
}

impl PartialEq for ExactCheckedItem {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index && Arc::ptr_eq(&self.cell, &other.cell)
    }
}
impl Eq for ExactCheckedItem {}

impl ExactCheckedItem {
    pub fn specification_digest(&self) -> [u8; 32] {
        self.cell.specification.specification_digest()
    }
    pub fn reserved_declaration_modules(&self) -> &[String] {
        &self.cell.specification.reserved_declaration_modules
    }
    pub fn injected_modules(&self) -> &[String] {
        &self.cell.specification.injected_modules
    }
    pub fn include_paths(&self) -> &[std::path::PathBuf] {
        &self.cell.include
    }
    pub fn input_work(&self) -> CheckedInputWork {
        self.cell.value_inputs.work()
    }
    pub(crate) fn value_input_root(&self) -> &Path {
        self.cell.value_inputs.root()
    }
    pub(crate) fn retain_input_diagnostics(
        &self,
        prefix: &ExactCompiledPrefix,
        destination: &Path,
    ) -> std::io::Result<()> {
        self.cell
            .value_inputs
            .retain_diagnostics(Some(prefix), destination)
    }
    pub fn baseline_value_interfaces(
        &self,
    ) -> impl Iterator<Item = (&tidepool_repr::SessionModule, &Arc<[u8]>)> {
        self.cell
            .value_inputs
            .baseline
            .iter()
            .map(|artifact| (&artifact.owner, &artifact.bytes))
    }
    pub fn cell_observations(&self) -> &[u8] {
        &self.cell.observations
    }
    pub fn planned_declaration(
        &self,
    ) -> Option<&Arc<crate::declaration_join::CertifiedAuthoredDeclaration>> {
        (self.kind() == CheckedItemKind::Declaration)
            .then_some(
                self.cell
                    .planned_declarations
                    .get(&self.index)
                    .or(self.cell.planned_declaration.as_ref()),
            )
            .flatten()
            .map(|planned| &planned.certificate)
    }
    pub fn planned_declaration_source(&self) -> Option<&str> {
        (self.kind() == CheckedItemKind::Declaration)
            .then_some(
                self.cell
                    .planned_declarations
                    .get(&self.index)
                    .or(self.cell.planned_declaration.as_ref()),
            )
            .flatten()
            .map(|planned| planned.source.as_str())
    }
    pub(crate) fn cell_include(&self) -> &[std::path::PathBuf] {
        &self.cell.include
    }
    pub fn turn_templates(&self) -> &[(String, String)] {
        &self.cell.specification.turn_templates
    }
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
    pub fn expression_lift(&self) -> Result<Option<CheckedExpressionLift>, CompileError> {
        let Some(expression) = &self.cell.items[self.index].expression else {
            return Ok(None);
        };
        Ok(Some(match string(&row(expression, 6)?[1])? {
            "pure" => CheckedExpressionLift::Pure,
            "effectful" => CheckedExpressionLift::Effectful,
            _ => return Err(failure("sealed expression has an unknown lift")),
        }))
    }
    pub fn expression_presentation(
        &self,
    ) -> Result<Option<CheckedExpressionPresentation>, CompileError> {
        let Some(expression) = &self.cell.items[self.index].expression else {
            return Ok(None);
        };
        Ok(Some(match string(&row(expression, 6)?[2])? {
            "rendered" => CheckedExpressionPresentation::Rendered,
            "opaque" => CheckedExpressionPresentation::Opaque,
            _ => return Err(failure("sealed expression has an unknown presentation")),
        }))
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
            completed: CheckedPrefixSequence::new(),
            displays: CheckedPrefixSequence::new(),
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
    target: &Arc<tidepool_repr::execution_schema::PreparedProgram>,
    artifact_context: &Arc<crate::declaration_context::ExactDeclarationContext>,
    source_lexical: &[crate::declaration_join::ExactLexicalNode],
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
        purpose: CheckedItemPurpose::Authored,
        prefix: item.initial_prefix()?,
        item,
        runtime_prefix_digest: cell.admission_digest(),
        generation,
        observation_name: None,
        is_fold: true,
        is_program: false,
        settled_values: CheckedSettledValues::default(),
    }
    .seal(
        root,
        request,
        source,
        target,
        artifact_context,
        source_lexical,
    )
}

#[derive(Clone, Debug)]
pub(crate) struct CheckedItemOffer {
    pub(crate) purpose: CheckedItemPurpose,
    pub(crate) item: ExactCheckedItem,
    pub(crate) prefix: ExactCompiledPrefix,
    pub(crate) runtime_prefix_digest: [u8; 32],
    pub(crate) generation: u64,
    pub(crate) observation_name: Option<String>,
    pub(crate) is_fold: bool,
    pub(crate) is_program: bool,
    pub(crate) settled_values: CheckedSettledValues,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CheckedItemPurpose {
    Authored,
    HostActivationInput,
}

impl CheckedItemOffer {
    pub(crate) fn validate_activation_input(&self) -> Result<(), CompileError> {
        if self.item.cell.item_count() != 1
            || self.item.index() != 0
            || self.item.kind() != CheckedItemKind::Bind
            || self.item.binders() != ["sessionInput"]
            || self.item.turn_templates().len() != 1
            || self.item.turn_templates()[0].0 != "bind"
            || self.item.signatures().len() != 1
            || self.item.signatures()[0].key() != "__tidepool_cell_pin_0_sessionInput"
            || self.item.admission_digest() == [0; 32]
            || self.item.cell.receipt_digest == [0; 32]
            || self.runtime_prefix_digest == [0; 32]
            || self.observation_name.is_some()
            || self.is_fold
            || self.is_program
            || self.generation == 0
        {
            return Err(failure("host activation input requires its one checked input binder and protected template"));
        }
        Ok(())
    }
    pub(crate) fn authorization(
        &self,
        producer: &[u8],
        context: [u8; 32],
    ) -> Result<Value, CompileError> {
        self.prefix.revalidate_context(producer, &context)?;
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
        if (self.item.kind() == CheckedItemKind::Expression) != self.observation_name.is_some()
            || self.observation_name.as_ref().is_some_and(|name| {
                name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
        {
            return Err(failure(
                "checked expression has no owning observation identity",
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
                self.settled_values
                    .imports
                    .iter()
                    .map(|(module, names)| {
                        array([text(module), Value::Array(names.iter().map(text).collect())])
                    })
                    .collect(),
            ),
            self.observation_name.as_ref().map_or(Value::Null, text),
            self.prefix.planned_authorization(),
            Value::Array(self.settled_values.authorization.clone()),
            self.prefix.value_interface_authorization()?,
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
        if !same_include_paths(include, &self.item.cell.include) {
            return Err(failure("checked item include search order changed"));
        }
        Ok(())
    }
    pub(crate) fn seal(
        &self,
        root: &Path,
        request: &str,
        source: &str,
        target: &Arc<tidepool_repr::execution_schema::PreparedProgram>,
        artifact_context: &Arc<crate::declaration_context::ExactDeclarationContext>,
        source_lexical: &[crate::declaration_join::ExactLexicalNode],
    ) -> Result<Arc<ExactCompiledItem>, CompileError> {
        if self.purpose != CheckedItemPurpose::Authored {
            return Err(failure(
                "host activation input cannot issue authored execution authority",
            ));
        }
        self.seal_recipe(
            root,
            request,
            source,
            target,
            artifact_context,
            source_lexical,
        )
        .map(|(compiled, _)| compiled)
    }

    pub(crate) fn seal_activation_input(
        &self,
        root: &Path,
        request: &str,
        source: &str,
        target: &Arc<tidepool_repr::execution_schema::PreparedProgram>,
        artifact_context: &Arc<crate::declaration_context::ExactDeclarationContext>,
        source_lexical: &[crate::declaration_join::ExactLexicalNode],
    ) -> Result<Arc<ExactCompiledActivationInput>, CompileError> {
        if self.purpose != CheckedItemPurpose::HostActivationInput {
            return Err(failure(
                "authored item cannot issue host activation authority",
            ));
        }
        self.validate_activation_input()?;
        let (compiled, witness) = self.seal_recipe(
            root,
            request,
            source,
            target,
            artifact_context,
            source_lexical,
        )?;
        Ok(Arc::new(ExactCompiledActivationInput {
            compiled,
            input_type_witness: witness
                .ok_or_else(|| failure("host input lacks canonical type witness"))?,
        }))
    }

    fn seal_recipe(
        &self,
        root: &Path,
        request: &str,
        source: &str,
        target: &Arc<tidepool_repr::execution_schema::PreparedProgram>,
        artifact_context: &Arc<crate::declaration_context::ExactDeclarationContext>,
        source_lexical: &[crate::declaration_join::ExactLexicalNode],
    ) -> Result<(Arc<ExactCompiledItem>, Option<CanonicalInputTypeWitness>), CompileError> {
        let (file, magic, profile) = match self.purpose {
            CheckedItemPurpose::Authored => (
                "checked-item.cbor",
                "TPEXACTITEM",
                "tidepool-checked-recipe-2",
            ),
            CheckedItemPurpose::HostActivationInput => (
                "activation-input.cbor",
                "TPEXACTACTIVATIONINPUT2",
                "tidepool-host-activation-input-2",
            ),
        };
        let receipt = decode(&read(root.join(file), 4 * 1024 * 1024)?)?;
        let host = self.purpose == CheckedItemPurpose::HostActivationInput;
        let fields = row(&receipt, if host { 9 } else { 8 })?;
        let input_witness = if host {
            let Value::Bytes(bytes) = &fields[8] else {
                return Err(failure("host input canonical type witness missing"));
            };
            Some(CanonicalInputTypeWitness::from_bytes(bytes)?)
        } else {
            None
        };
        if string(&fields[0])? != magic
            || string(&fields[1])? != if host { "2" } else { "1" }
            || string(&fields[2])? != request
            || string(&fields[3])? != hex(&self.item.admission_digest())
            || string(&fields[4])?
                != hex(&if self.is_program {
                    self.item.admission_digest()
                } else {
                    self.item.cell.receipt_digest
                })
            || fields[5] != Value::Integer((self.item.index as u64).into())
            || string(&fields[6])? != hash(source.as_bytes())
            || string(&fields[7])? != profile
        {
            return Err(failure(
                "checked-item recipe receipt differs from its same compiler offer",
            ));
        }
        let turn = decode(&read(root.join("turn.cbor"), 32 * 1024 * 1024)?)?;
        let turn = row(&turn, 2)?;
        let expected_binders = if let Some(observation) = &self.observation_name {
            vec![observation.clone()]
        } else {
            self.item.binders().to_vec()
        };
        let (bound_binders, authenticated_sites) = match (self.item.kind(), string(&turn[0])?) {
            (CheckedItemKind::Bind | CheckedItemKind::Expression, "Bind") => {
                let fields = row(&turn[1], 5)?;
                if fields[0] != Value::Array(expected_binders.iter().map(text).collect())
                    || string(&fields[4])? != source
                {
                    return Err(failure(
                        "compiled bind has another authored verdict or wrapper",
                    ));
                }
                let authenticated_sites = crate::artifacts::yield_sites_metadata_digest(
                    &crate::turn_observations::decode_turn_yield_sites(&fields[3])?,
                )?;
                let bound = list(&fields[2], 65536)?.to_vec();
                if bound.len() != expected_binders.len() {
                    return Err(failure("compiled binder inventory differs"));
                }
                for (value, binder) in bound.iter().zip(&expected_binders) {
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
                (bound, authenticated_sites)
            }
            _ => return Err(failure("compiled turn kind differs from checked item")),
        };
        let value_interface = if !expected_binders.is_empty() {
            Some(self.item.cell.value_inputs.capture_output(
                self.generation,
                &self.item.cell,
                artifact_context,
                source_lexical,
            )?)
        } else {
            None
        };
        Ok((
            Arc::new(ExactCompiledItem {
                item: self.item.clone(),
                target: target.clone(),
                table: read_table(root)?,
                yield_sites_digest: authenticated_sites,
                value_interface,
                generation: self.generation,
                bound_binders,
                observation_name: self.observation_name.clone(),
                admission: if self.purpose == CheckedItemPurpose::HostActivationInput {
                    CheckedExecutionAdmission::HostActivationInput(self.runtime_prefix_digest)
                } else if self.is_program {
                    CheckedExecutionAdmission::CellProgram(self.item.admission_digest())
                } else if self.is_fold {
                    CheckedExecutionAdmission::InitialFold(self.runtime_prefix_digest)
                } else {
                    CheckedExecutionAdmission::RuntimeItem(self.runtime_prefix_digest)
                },
                settled_values: self.settled_values.clone(),
            }),
            input_witness,
        ))
    }
}

fn target_definition_identities(
    target: &tidepool_repr::execution_schema::PreparedProgram,
) -> impl Iterator<Item = &tidepool_repr::execution_schema::SymbolIdentity> {
    use tidepool_repr::execution_schema::Group;
    target.bindings().iter().flat_map(|group| {
        let bindings = match group {
            Group::NonRecursive(binding) => std::slice::from_ref(binding),
            Group::Recursive(bindings) => bindings.as_slice(),
        };
        bindings.iter().map(|binding| &binding.identity)
    })
}

fn bound_binder_identities(bound: &[Value]) -> impl Iterator<Item = (&str, u64)> {
    bound.iter().map(|binder| {
        let fields = row(binder, 7).expect("sealed native binder row");
        (
            string(&fields[0]).expect("sealed native binder name"),
            u64::try_from(fields[1].as_integer().expect("sealed native binder ID"))
                .expect("sealed native binder ID range"),
        )
    })
}

fn read_table(root: &Path) -> Result<tidepool_repr::DataConTable, CompileError> {
    let (table, _) =
        tidepool_repr::serial::read_metadata(&read(root.join("meta.cbor"), 32 * 1024 * 1024)?)?;
    Ok(table)
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

pub(crate) fn same_include_paths(
    left: &[std::path::PathBuf],
    right: &[std::path::PathBuf],
) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.as_os_str() == right.as_os_str())
}

pub(crate) fn admit_checked_cell(
    root: &Path,
    producer: &[u8],
    context: [u8; 32],
    declaration_context: Arc<crate::declaration_context::ExactDeclarationContext>,
    publication_context: Arc<crate::declaration_context::ExactDeclarationContext>,
    request_digest: &str,
    specification: &CheckedCellSpecification,
    admissions: Vec<ExactSourceAdmission>,
    include: &[std::path::PathBuf],
    planned_declaration: Option<PlannedCheckedDeclaration>,
    value_inputs: Arc<CheckedValueInputs>,
    planned_declarations: BTreeMap<usize, PlannedCheckedDeclaration>,
    program: Option<&CheckedPlannedCellSpecification>,
) -> Result<Arc<ExactCheckedCell>, CompileError> {
    let receipt = read(root.join("checked-cell.cbor"), 8 * 1024 * 1024)?;
    let value = decode(&receipt)?;
    let header = row(&value, if program.is_some() { 11 } else { 10 })?;
    let observations = read(root.join("cell.cbor"), 32 * 1024 * 1024)?;
    let output = decode(&observations)?;
    let output = row(&output, 5)?;
    let checked_source = string(&output[2])?.to_owned();
    if string(&header[0])?
        != if program.is_some() {
            "TPEXACTPROGRAM"
        } else {
            "TPEXACTCHECK"
        }
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
    if let Some(program) = program {
        if string(&header[10])? != hex(&program.parsed_plan.digest()) {
            return Err(failure("program has another parser plan"));
        }
        let receipts = list(&header[9], 10000)?;
        if receipts.len() != planned_declarations.len() {
            return Err(failure("program declaration receipts are incomplete"));
        }
        for value in receipts {
            let fields = row(value, 2)?;
            let Value::Integer(index) = fields[0] else {
                return Err(failure("declaration index is not integer"));
            };
            let index = usize::try_from(index).map_err(failure)?;
            if planned_declarations.get(&index).is_none_or(|declaration| {
                string(&fields[1]).ok() != Some(hex(&declaration.receipt_digest).as_str())
            }) {
                return Err(failure("program original declaration receipt differs"));
            }
        }
    } else {
        match (&planned_declaration, &header[9]) {
            (Some(planned), Value::Text(digest)) if digest == &hex(&planned.receipt_digest) => {}
            (None, Value::Null) => {}
            _ => return Err(failure("whole-cell original declaration receipt differs")),
        }
    }
    let evidence = if program.is_some() {
        admissions
            .into_iter()
            .map(|admitted| {
                let source = std::fs::read_to_string(admitted.witness.source_path())?;
                if !admitted
                    .witness
                    .matches_source(admitted.witness.source_path(), &source)
                    || !admitted.evidence.valid(&source)
                {
                    return Err(failure("compiled program source evidence changed"));
                }
                Ok((source, admitted.evidence))
            })
            .collect::<Result<Vec<_>, CompileError>>()?
    } else {
        let source = admissions
            .into_iter()
            .find(|source| {
                source.witness.source_sha256()
                    == &<[u8; 32]>::from(Sha256::digest(checked_source.as_bytes()))
            })
            .ok_or_else(|| failure("cell has no exact final source witness"))?;
        vec![(checked_source.clone(), source.evidence)]
    };
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
    let declaration_indices = items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.kind == CheckedItemKind::Declaration)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let expected_declarations = if program.is_some() {
        planned_declarations.keys().copied().collect::<Vec<_>>()
    } else if planned_declaration.is_some() {
        vec![0]
    } else {
        Vec::new()
    };
    if declaration_indices != expected_declarations {
        return Err(failure("program lacks original declaration certificates"));
    }
    if let Some(program) = program {
        if items.len() != program.slots.len() {
            return Err(failure("program item count differs from reservations"));
        }
        for (item, parsed) in items.iter().zip(program.parsed_plan.items()) {
            let kind = match parsed.kind() {
                crate::cell_plan::ParsedCellPlanKind::Declaration
                | crate::cell_plan::ParsedCellPlanKind::Prologue => CheckedItemKind::Declaration,
                crate::cell_plan::ParsedCellPlanKind::Bind => CheckedItemKind::Bind,
                crate::cell_plan::ParsedCellPlanKind::Expression => CheckedItemKind::Expression,
            };
            if item.kind != kind
                || item.binders != parsed.binders()
                || (kind != CheckedItemKind::Declaration && item.source != parsed.source())
            {
                return Err(failure("prepared items differ from parser plan"));
            }
        }
    }
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
        producer: crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
            producer,
        )
        .sha256(),
        context,
        declaration_context,
        publication_context,
        receipt_digest: Sha256::digest(&receipt).into(),
        evidence,
        checked_source,
        observations,
        items,
        include: include.to_vec(),
        planned_declaration,
        planned_declarations,
        value_inputs,
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
pub(crate) fn read(path: impl AsRef<Path>, limit: u64) -> Result<Vec<u8>, CompileError> {
    if std::fs::metadata(path.as_ref())
        .map_err(|error| {
            failure(format!(
                "checked evidence {}: {error}",
                path.as_ref().display()
            ))
        })?
        .len()
        > limit
    {
        return Err(failure("checked evidence exceeds bound"));
    }
    let bytes = std::fs::read(path.as_ref()).map_err(|error| {
        failure(format!(
            "checked evidence {}: {error}",
            path.as_ref().display()
        ))
    })?;
    if bytes.len() as u64 > limit {
        return Err(failure("checked evidence exceeds bound"));
    }
    Ok(bytes)
}
pub(crate) fn decode(bytes: &[u8]) -> Result<Value, CompileError> {
    let mut cursor = std::io::Cursor::new(bytes);
    let value = ciborium::de::from_reader(&mut cursor).map_err(failure)?;
    if cursor.position() != bytes.len() as u64 {
        return Err(failure("checked evidence has trailing bytes"));
    }
    Ok(value)
}
pub(crate) fn row(value: &Value, count: usize) -> Result<&[Value], CompileError> {
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
pub(crate) fn string(value: &Value) -> Result<&str, CompileError> {
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
pub(crate) fn hash(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes).into())
}
fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn failure(error: impl std::fmt::Display) -> CompileError {
    CompileError::ExtractFailed(format!("checked cell: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{CheckedPrefixSequence, CheckedValueInputs};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    fn witness_bytes(shape: ciborium::Value, seal: &str) -> Vec<u8> {
        use super::*;
        let mut structure = Vec::new();
        ciborium::into_writer(&shape, &mut structure).unwrap();
        let wire = array([
            text("TPCANONICALINPUTTYPE1"),
            text("1"),
            array([text("activation-input"), text("presentation"), array([])]),
            Value::Bytes(structure),
            array([array([text("main"), text("Owner"), text(seal)])]),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&wire, &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn planned_prefix_revalidation_uses_publication_context_not_unrelated_value_rows() {
        use super::*;
        use crate::declaration_join::{ExactLexicalNode, ExactModuleIdentity};

        let module = |name: &str| ExactModuleIdentity {
            unit: "main".into(),
            module: name.into(),
        };
        let authored = module("Authored");
        let selected = module("CheckedHomeValue");
        let unrelated = module("UnrelatedHomeValue");
        let certificate = Arc::new(
            crate::declaration_join::CertifiedAuthoredDeclaration::test_certificate(
                authored.clone(),
                vec![selected.clone()],
                vec![ExactLexicalNode {
                    owner: selected.clone(),
                    imports: Vec::new(),
                }],
            ),
        );

        let publication_context = Arc::new(
            crate::declaration_join::ExactDeclarationContext::new(&[], &[], Vec::new()).unwrap(),
        );
        let enriched_context = Arc::new(
            crate::declaration_join::ExactDeclarationContext::new(
                &[],
                &[],
                vec![
                    ExactLexicalNode {
                        owner: selected.clone(),
                        imports: Vec::new(),
                    },
                    ExactLexicalNode {
                        owner: unrelated.clone(),
                        imports: Vec::new(),
                    },
                ],
            )
            .unwrap(),
        );
        assert!(enriched_context
            .lexical_graph()
            .iter()
            .any(|node| node.owner == unrelated));

        let expected_publication = publication_context
            .as_ref()
            .clone()
            .extend(
                std::slice::from_ref(&certificate),
                &[],
                vec![
                    ExactLexicalNode {
                        owner: authored.clone(),
                        imports: vec![selected.clone()],
                    },
                    ExactLexicalNode {
                        owner: selected.clone(),
                        imports: Vec::new(),
                    },
                ],
            )
            .unwrap();
        assert!(!expected_publication
            .lexical_graph()
            .iter()
            .any(|node| node.owner == unrelated));
        let incorrectly_promoted_publication = publication_context
            .as_ref()
            .clone()
            .extend(
                std::slice::from_ref(&certificate),
                &[],
                vec![
                    ExactLexicalNode {
                        owner: authored.clone(),
                        imports: vec![selected.clone()],
                    },
                    ExactLexicalNode {
                        owner: selected.clone(),
                        imports: Vec::new(),
                    },
                    ExactLexicalNode {
                        owner: unrelated,
                        imports: Vec::new(),
                    },
                ],
            )
            .unwrap();
        assert!(prefix_context_differs_only_by_unrelated_owner(
            &expected_publication,
            &incorrectly_promoted_publication
        ));

        let producer = b"checked-prefix revalidation fixture";
        let cell = Arc::new(ExactCheckedCell {
            specification: CheckedCellSpecification {
                admission_digest: [4; 32],
                cell_source: "module Authored where".into(),
                template_source: String::new(),
                turn_templates: Vec::new(),
                injected_modules: Vec::new(),
                reserved_declaration_modules: vec!["Authored".into()],
            },
            producer: crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
                producer,
            )
            .sha256(),
            context: enriched_context.semantic_sha256(),
            declaration_context: enriched_context,
            publication_context,
            receipt_digest: [9; 32],
            checked_source: "module Authored where".into(),
            evidence: Vec::new(),
            observations: Vec::new(),
            items: vec![CheckedItem {
                kind: CheckedItemKind::Declaration,
                source: "module Authored where".into(),
                binders: Vec::new(),
                pins: Vec::new(),
                expression: None,
                signatures: Vec::new(),
            }],
            include: Vec::new(),
            planned_declaration: Some(PlannedCheckedDeclaration {
                source: "module Authored where".into(),
                interface_fingerprint: "fixture".into(),
                certificate,
                receipt_digest: [10; 32],
            }),
            planned_declarations: Default::default(),
            value_inputs: CheckedValueInputs::capture(Vec::new()).unwrap(),
        });
        let item = ExactCheckedItem {
            cell: Arc::clone(&cell),
            index: 0,
        };
        let mut completed = CheckedPrefixSequence::new();
        completed.push(CompletedCheckedItem::Declaration(item));
        let prefix = ExactCompiledPrefix {
            cell,
            completed,
            displays: CheckedPrefixSequence::new(),
        };

        prefix
            .revalidate_context(producer, &expected_publication.semantic_sha256())
            .expect("same certificate revalidates the declaration publication surface");
        assert!(prefix
            .revalidate_context(
                producer,
                &incorrectly_promoted_publication.semantic_sha256()
            )
            .is_err());
    }

    fn prefix_context_differs_only_by_unrelated_owner(
        expected: &crate::declaration_join::ExactDeclarationContext,
        promoted: &crate::declaration_join::ExactDeclarationContext,
    ) -> bool {
        let expected = expected
            .lexical_graph()
            .iter()
            .map(|node| (node.owner.clone(), node.imports.clone()))
            .collect::<std::collections::BTreeMap<_, _>>();
        let promoted = promoted
            .lexical_graph()
            .iter()
            .map(|node| (node.owner.clone(), node.imports.clone()))
            .collect::<std::collections::BTreeMap<_, _>>();
        promoted.len() == expected.len() + 1
            && expected
                .iter()
                .all(|(owner, imports)| promoted.get(owner) == Some(imports))
    }

    #[test]
    fn canonical_input_witness_preserves_shape_and_original_interface_seals() {
        use super::*;
        let con = |name: &str| {
            array([
                text("con"),
                array([text("main"), text("Owner"), text("type"), text(name)]),
                array([]),
            ])
        };
        let fun = |argument, result| {
            array([
                text("fun"),
                Value::Integer(0.into()),
                con("Many"),
                argument,
                result,
            ])
        };
        let forward = CanonicalInputTypeWitness::from_bytes(&witness_bytes(
            fun(con("A"), con("B")),
            &"a".repeat(64),
        ))
        .unwrap();
        let same = CanonicalInputTypeWitness::from_bytes(&witness_bytes(
            fun(con("A"), con("B")),
            &"a".repeat(64),
        ))
        .unwrap();
        let swapped = CanonicalInputTypeWitness::from_bytes(&witness_bytes(
            fun(con("B"), con("A")),
            &"a".repeat(64),
        ))
        .unwrap();
        let changed_interface = CanonicalInputTypeWitness::from_bytes(&witness_bytes(
            fun(con("A"), con("B")),
            &"b".repeat(64),
        ))
        .unwrap();
        assert_eq!(forward, same);
        assert_eq!(forward.commitment(), same.commitment());
        assert_ne!(forward, swapped);
        assert_ne!(forward, changed_interface);
        assert_ne!(forward.commitment(), changed_interface.commitment());
        let mut presentation_wire =
            decode(&witness_bytes(fun(con("A"), con("B")), &"a".repeat(64))).unwrap();
        let Value::Array(fields) = &mut presentation_wire else {
            unreachable!()
        };
        let Value::Array(signature) = &mut fields[2] else {
            unreachable!()
        };
        signature[1] = text("different parser presentation");
        let mut presentation_bytes = Vec::new();
        ciborium::into_writer(&presentation_wire, &mut presentation_bytes).unwrap();
        let presentation = CanonicalInputTypeWitness::from_bytes(&presentation_bytes).unwrap();
        assert_eq!(forward, presentation);
        assert_eq!(forward.commitment(), presentation.commitment());
        assert_ne!(forward.metadata_digest(), presentation.metadata_digest());
        let site = crate::YieldSite {
            site: 1,
            origin: "original".into(),
            ordinal: 0,
            ty: "Int".into(),
            modules: Vec::new(),
            heads: Vec::new(),
            inputs: vec![crate::SiteType {
                ty: "A -> B".into(),
                modules: vec!["Owner".into()],
                heads: Vec::new(),
            }],
            input_type_witnesses: vec![Some(forward.clone())],
            reply_declaration: None,
        };
        let mut edited = site.clone();
        edited.input_type_witnesses[0] = Some(presentation);
        assert_eq!(site, edited);
        assert!(!site.same_metadata(&edited));
        assert!(crate::artifacts::yield_sites_metadata_digest(&[site.clone(), edited]).is_err());
        let original_digest =
            crate::artifacts::yield_sites_metadata_digest(&[site.clone()]).unwrap();
        let mut absent = site.clone();
        absent.input_type_witnesses.clear();
        let mut input_changed = site.clone();
        input_changed.inputs[0].ty = "B -> A".into();
        let mut origin_changed = site.clone();
        origin_changed.origin = "another owner".into();
        for altered in [absent, input_changed, origin_changed] {
            assert_ne!(
                original_digest,
                crate::artifacts::yield_sites_metadata_digest(&[altered]).unwrap()
            );
        }
        for shape in [
            array([text("bound"), Value::Integer(0.into())]),
            array([
                text("forall"),
                Value::Integer(0.into()),
                con("Type"),
                array([text("bound"), Value::Integer(1.into())]),
            ]),
            array([text("unconstructible"), text("function"), text("A -> B")]),
        ] {
            assert!(
                CanonicalInputTypeWitness::from_bytes(&witness_bytes(shape, &"a".repeat(64)))
                    .is_err()
            );
        }
        let mut wire = decode(&witness_bytes(con("A"), &"a".repeat(64))).unwrap();
        let Value::Array(fields) = &mut wire else {
            unreachable!()
        };
        fields[4] = array([]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&wire, &mut bytes).unwrap();
        assert!(CanonicalInputTypeWitness::from_bytes(&bytes).is_err());
    }

    #[test]
    #[ignore = "requires original Haskell canonical input producer vectors"]
    fn canonical_input_witness_matches_haskell_original_and_preview() {
        use super::*;
        let root = std::env::var_os("TIDEPOOL_CANONICAL_INPUT_FIXTURE")
            .expect("exact producer vector directory");
        let root = std::path::Path::new(&root);
        let witness = |name| {
            CanonicalInputTypeWitness::from_bytes(&std::fs::read(root.join(name)).unwrap()).unwrap()
        };
        let original = witness("original-input.cbor");
        let preview = witness("preview-input.cbor");
        assert_eq!(original, preview);
        assert_eq!(original.commitment(), preview.commitment());
        assert_eq!(witness("alpha-first.cbor"), witness("alpha-second.cbor"));
        assert_ne!(
            witness("original-owner.cbor"),
            witness("changed-owner.cbor")
        );
        assert_ne!(
            witness("forward-function.cbor"),
            witness("backward-function.cbor")
        );
    }

    fn activation_offer(binder: &str) -> super::CheckedItemOffer {
        use super::*;
        let source = "sessionInput <- pure (undefined :: Int)";
        let cell = Arc::new(ExactCheckedCell {
            specification: CheckedCellSpecification {
                admission_digest: [4; 32],
                cell_source: source.into(),
                template_source: String::new(),
                turn_templates: vec![("bind".into(), "protected template".into())],
                injected_modules: Vec::new(),
                reserved_declaration_modules: Vec::new(),
            },
            producer: [7; 32],
            context: [8; 32],
            declaration_context: Arc::new(
                crate::declaration_context::ExactDeclarationContext::new(&[], &[], Vec::new())
                    .unwrap(),
            ),
            publication_context: Arc::new(
                crate::declaration_context::ExactDeclarationContext::new(&[], &[], Vec::new())
                    .unwrap(),
            ),
            receipt_digest: [9; 32],
            checked_source: source.into(),
            evidence: Vec::new(),
            observations: Vec::new(),
            items: vec![CheckedItem {
                kind: CheckedItemKind::Bind,
                source: source.into(),
                binders: vec![binder.into()],
                pins: Vec::new(),
                expression: None,
                signatures: vec![ExactCheckedSignature {
                    key: "__tidepool_cell_pin_0_sessionInput".into(),
                    source: "Int".into(),
                    names: Vec::new(),
                }],
            }],
            include: Vec::new(),
            planned_declaration: None,
            planned_declarations: Default::default(),
            value_inputs: CheckedValueInputs::capture(Vec::new()).unwrap(),
        });
        let item = cell.item(0).unwrap();
        CheckedItemOffer {
            purpose: CheckedItemPurpose::HostActivationInput,
            prefix: item.initial_prefix().unwrap(),
            item,
            runtime_prefix_digest: [6; 32],
            generation: 1,
            observation_name: None,
            is_fold: false,
            is_program: false,
            settled_values: Default::default(),
        }
    }

    #[test]
    fn activation_role_refuses_cross_sealing_before_reading_output_files() {
        use super::*;
        let offer = activation_offer("sessionInput");
        offer.validate_activation_input().unwrap();
        assert!(activation_offer("other")
            .validate_activation_input()
            .is_err());
        let prepared = Arc::new(
            tidepool_repr::execution_schema::parse_program(
                include_bytes!(
                    "../../../bridge/haskell/test-prepared-stg/fixtures/m3-vertical.cbor"
                ),
                &crate::prepared_artifact::production_requirements().unwrap(),
                tidepool_repr::execution_schema::DecodeLimits::default(),
            )
            .unwrap(),
        );
        let empty = tempfile::tempdir().unwrap();
        let artifact_context = offer.item.cell.declaration_context.clone();
        let source_lexical = artifact_context.lexical_graph();
        assert!(offer
            .seal(
                empty.path(),
                "request",
                "source",
                &prepared,
                &artifact_context,
                source_lexical,
            )
            .unwrap_err()
            .to_string()
            .contains("cannot issue authored execution authority"));
        let authored = CheckedItemOffer {
            purpose: CheckedItemPurpose::Authored,
            ..offer
        };
        assert!(authored
            .seal_activation_input(
                empty.path(),
                "request",
                "source",
                &prepared,
                &artifact_context,
                source_lexical,
            )
            .unwrap_err()
            .to_string()
            .contains("cannot issue host activation authority"));
    }

    #[test]
    fn failed_checked_inputs_retain_sealed_and_observed_bytes_after_owner_drop() {
        let owner = tidepool_repr::SessionModule::val(tidepool_repr::Generation(3));
        let inputs = CheckedValueInputs::capture(vec![(owner, Arc::from(&b"sealed"[..]))])
            .expect("capture exact input");
        std::fs::write(inputs.root().join(owner.relative_hi_path()), b"edited")
            .expect("alter observed file");
        std::fs::write(inputs.root().join("unselected.hi"), b"unselected")
            .expect("unselected directory member");
        let retained = tempfile::tempdir().expect("retained diagnostic directory");
        inputs
            .retain_diagnostics(None, retained.path())
            .expect("retain checked inputs");
        drop(inputs);
        let saved = retained.path().join("checked-value-inputs");
        assert_eq!(
            std::fs::read(saved.join("expected").join(owner.relative_hi_path())).unwrap(),
            b"sealed"
        );
        assert_eq!(
            std::fs::read(saved.join("observed").join(owner.relative_hi_path())).unwrap(),
            b"edited"
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(saved.join("inputs.json")).unwrap()).unwrap();
        assert_eq!(manifest.as_array().unwrap().len(), 1);
        assert_ne!(
            manifest[0]["expected_sha256"],
            manifest[0]["observed_sha256"]
        );
        assert!(!saved.join("expected/unselected.hi").exists());
    }

    #[test]
    fn completed_prefix_snapshots_keep_order_without_copying_historical_links() {
        #[derive(Debug)]
        struct Link {
            ordinal: usize,
            copies: Arc<AtomicUsize>,
        }
        impl Clone for Link {
            fn clone(&self) -> Self {
                self.copies.fetch_add(1, Ordering::Relaxed);
                Self {
                    ordinal: self.ordinal,
                    copies: self.copies.clone(),
                }
            }
        }
        let copies = Arc::new(AtomicUsize::new(0));
        let mut prefix = CheckedPrefixSequence::new();
        let mut snapshots = Vec::new();
        for ordinal in 0..100 {
            let mut next = prefix.clone();
            next.push(Link {
                ordinal,
                copies: copies.clone(),
            });
            assert_eq!(prefix.len(), ordinal);
            assert!(prefix.get(ordinal).is_none());
            prefix = next;
            if [1, 10, 100].contains(&prefix.len()) {
                snapshots.push(prefix.clone());
            }
        }
        for snapshot in snapshots {
            assert_eq!(
                snapshot.iter().map(|link| link.ordinal).collect::<Vec<_>>(),
                (0..snapshot.len()).collect::<Vec<_>>()
            );
            for ordinal in 0..snapshot.len() {
                assert_eq!(snapshot.get(ordinal).unwrap().ordinal, ordinal);
                assert!(std::ptr::eq(
                    snapshot.get(ordinal).unwrap(),
                    prefix.get(ordinal).unwrap()
                ));
            }
        }
        assert_eq!(
            copies.load(Ordering::Relaxed),
            0,
            "snapshot append copied historical proof links"
        );
    }
}
