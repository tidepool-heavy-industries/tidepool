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

/// Caller-authored, immutable whole-cell recipe. It names the exact source,
/// template and turn-template texts and the injected owners, and carries no
/// admission facts at all. A recipe may be cloned and re-parsed freely; it can
/// never carry, acquire or be relabelled with an admission digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellRecipe {
    cell_source: String,
    template_source: String,
    turn_templates: Vec<(String, String)>,
    injected_modules: Vec<String>,
}

impl CellRecipe {
    /// The authored specification's own constructor. Duplicate injected
    /// owners are refused here, at the owning entry point, rather than at
    /// each manifest consumer.
    pub fn new(
        cell_source: impl Into<String>,
        template_source: impl Into<String>,
        turn_templates: Vec<(String, String)>,
        injected_modules: Vec<String>,
    ) -> Result<Self, CompileError> {
        if injected_modules.iter().collect::<BTreeSet<_>>().len() != injected_modules.len() {
            return Err(failure("duplicate injected module"));
        }
        Ok(Self {
            cell_source: cell_source.into(),
            template_source: template_source.into(),
            turn_templates,
            injected_modules,
        })
    }
    pub fn cell_source(&self) -> &str {
        &self.cell_source
    }
    pub fn template_source(&self) -> &str {
        &self.template_source
    }
    pub fn turn_templates(&self) -> &[(String, String)] {
        &self.turn_templates
    }
    pub fn injected_modules(&self) -> &[String] {
        &self.injected_modules
    }
    pub(crate) fn template_sources(&self) -> Vec<String> {
        std::iter::once(self.template_source.clone())
            .chain(self.turn_templates.iter().map(|(_, source)| source.clone()))
            .collect()
    }
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
}

/// The one runtime-admission digest the source owner finalized for a cell.
/// The newtype keeps a bare `[u8; 32]` out of the consumer API, refuses the
/// all-zero value so an absent digest cannot stand in for an admission, and is
/// only ever produced once, before the admission is shared. A digest value is
/// still not authority: installing the compiled cell compares it with the
/// runtime admission that issued it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionDigest([u8; 32]);

impl AdmissionDigest {
    /// Finalize a runtime admission's digest. The admission owner computes the
    /// digest, calls this once before constructing its admission, and never
    /// re-derives or patches it afterwards.
    pub fn finalize(digest: [u8; 32]) -> Result<Self, CompileError> {
        if digest == [0; 32] {
            return Err(failure("runtime admission digest is absent"));
        }
        Ok(Self(digest))
    }
    pub fn bytes(self) -> [u8; 32] {
        self.0
    }
}

/// One issued whole-cell request: the exact admitted recipe together with the
/// admission digest and original declaration owners the source owner finalized.
/// Fields are private, there is no mutable access and no `Clone`; a consumer
/// path can only read the request it was handed. The only issuer is the source
/// admission owner, once per reservation, through [`CheckedCellRequest::issue`].
#[derive(Debug)]
pub struct CheckedCellRequest {
    recipe: Arc<CellRecipe>,
    admission: AdmissionDigest,
    reserved_declaration_modules: Arc<[String]>,
}

impl CheckedCellRequest {
    /// Issue the one request for an admitted recipe. `reserved` is the exact
    /// original declaration owner inventory in declaration order; duplicates
    /// are refused here, before any consumer observes the request.
    pub fn issue(
        recipe: Arc<CellRecipe>,
        admission: AdmissionDigest,
        reserved_declaration_modules: Vec<String>,
    ) -> Result<Self, CompileError> {
        if reserved_declaration_modules
            .iter()
            .collect::<BTreeSet<_>>()
            .len()
            != reserved_declaration_modules.len()
        {
            return Err(failure("duplicate planned module"));
        }
        Ok(Self {
            recipe,
            admission,
            reserved_declaration_modules: reserved_declaration_modules.into(),
        })
    }
    pub fn recipe(&self) -> &Arc<CellRecipe> {
        &self.recipe
    }
    pub fn admission(&self) -> AdmissionDigest {
        self.admission
    }
    pub fn admission_digest(&self) -> [u8; 32] {
        self.admission.bytes()
    }
    pub fn reserved_declaration_modules(&self) -> &[String] {
        &self.reserved_declaration_modules
    }
    pub(crate) fn template_sources(&self) -> Vec<String> {
        self.recipe.template_sources()
    }
    pub(crate) fn manifest_value(
        &self,
        purpose: CheckedCellManifestPurpose,
    ) -> Result<Value, CompileError> {
        let modules = self.reserved_declaration_modules();
        Ok(array([
            text(match purpose {
                CheckedCellManifestPurpose::Authored => {
                    crate::artifacts::CheckedPurpose::Cell.wire_tag()
                }
                CheckedCellManifestPurpose::Program => {
                    crate::artifacts::CheckedPurpose::Program.wire_tag()
                }
            }),
            text(hex(&self.admission_digest())),
            text(hash(self.recipe.cell_source.as_bytes())),
            text(hash(self.recipe.template_source.as_bytes())),
            Value::Array(
                self.recipe
                    .turn_templates
                    .iter()
                    .map(|(kind, source)| array([text(kind), text(hash(source.as_bytes()))]))
                    .collect(),
            ),
            Value::Array(self.recipe.injected_modules.iter().map(text).collect()),
            Value::Array(modules.iter().map(text).collect()),
        ]))
    }
}

pub(crate) enum CheckedCellManifestPurpose {
    Authored,
    Program,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactSignatureName {
    unit: String,
    module: String,
    namespace: String,
    occurrence: String,
}

/// A compiler-issued native IfaceType and its original external Names.
/// `presentation` is human-readable text; compilation consumes the opaque payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactCheckedSignature {
    key: String,
    presentation: String,
    iface: Arc<[u8]>,
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
    original_bytes: Arc<[u8]>,
}

impl PartialEq for CanonicalInputTypeWitness {
    fn eq(&self, other: &Self) -> bool {
        self.structure == other.structure && self.interfaces == other.interfaces
    }
}
impl Eq for CanonicalInputTypeWitness {}

impl CanonicalInputTypeWitness {
    /// Seal of the complete compiler-issued payload, including its native
    /// signature. Semantic equality alone does not authenticate that payload.
    pub fn metadata_digest(&self) -> [u8; 32] {
        self.metadata_digest
    }
    /// Complete original compiler payload, retained independently of semantic equality.
    pub fn original_bytes(&self) -> &[u8] {
        &self.original_bytes
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
    pub fn interface_seals(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        self.interfaces
            .iter()
            .map(|(unit, module, seal)| (unit.as_str(), module.as_str(), seal.as_str()))
    }
    pub fn signature(&self) -> &ExactCheckedSignature {
        &self.signature
    }
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, CompileError> {
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(failure("canonical input witness byte bound"));
        }
        let decoded = decode(bytes)?;
        let mut canonical = Vec::new();
        ciborium::ser::into_writer(&decoded, &mut canonical).expect("witness encodes to memory");
        if canonical != bytes {
            return Err(failure("canonical input witness uses noncanonical CBOR"));
        }
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
        let decoded_structure = decode(structure)?;
        let mut canonical_structure = Vec::new();
        ciborium::ser::into_writer(&decoded_structure, &mut canonical_structure)
            .expect("input structure encodes to memory");
        if canonical_structure != *structure {
            return Err(failure("canonical input structure uses noncanonical CBOR"));
        }
        let mut names = BTreeSet::new();
        let mut count = 0;
        validate_input_type_shape(&decoded_structure, 0, 0, &mut count, &mut names)?;
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
            original_bytes: bytes.into(),
        })
    }
}

impl<'de> serde::Deserialize<'de> for CanonicalInputTypeWitness {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_bytes(&deserialize_type_evidence_bytes(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

fn deserialize_type_evidence_bytes<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<u8>, D::Error> {
    let encoded = <String as serde::Deserialize>::deserialize(deserializer)?;
    if encoded.len() > 8 * 1024 * 1024 || encoded.len() % 2 != 0 {
        return Err(serde::de::Error::custom("native type evidence hex bound"));
    }
    encoded
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
                .ok_or_else(|| serde::de::Error::custom("native type evidence hex"))
        })
        .collect::<Result<Vec<_>, D::Error>>()
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
    pub fn presentation(&self) -> &str {
        &self.presentation
    }
    pub fn names(&self) -> &[ExactSignatureName] {
        &self.names
    }
}

/// Compiler-issued closure/data tier for one original bound binder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorldItemBinderTier {
    ForceData,
    RetainOpaque,
}

/// The host authority attached to an original binder, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorldItemHostAuthority {
    JsonValue,
    Text,
    CommandJob,
}

/// One decoded original binder from a world item's field 10. It retains the
/// compiler-issued name, stable variable id, module, tier and optional type
/// identity; it issues no runtime value or execution permission.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WorldItemBinder {
    name: String,
    varid: u64,
    module: String,
    tier: WorldItemBinderTier,
    type_display: String,
    root_head: Option<Value>,
    host_authority: Option<WorldItemHostAuthority>,
}

impl WorldItemBinder {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn varid(&self) -> u64 {
        self.varid
    }
    pub(crate) fn module(&self) -> &str {
        &self.module
    }
    pub(crate) fn tier(&self) -> WorldItemBinderTier {
        self.tier
    }
    pub(crate) fn type_display(&self) -> &str {
        &self.type_display
    }
    pub(crate) fn root_head(&self) -> Option<&Value> {
        self.root_head.as_ref()
    }
    pub(crate) fn host_authority(&self) -> Option<WorldItemHostAuthority> {
        self.host_authority
    }
}

/// Native result types captured at one original request site. The complete
/// payload belongs to the site's metadata seal; presentation never issues it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestTypeSignatures {
    reply: ExactCheckedSignature,
    progress: Option<ExactCheckedSignature>,
    metadata_digest: [u8; 32],
}

impl RequestTypeSignatures {
    pub fn reply(&self) -> &ExactCheckedSignature {
        &self.reply
    }
    pub fn progress(&self) -> Option<&ExactCheckedSignature> {
        self.progress.as_ref()
    }
    pub fn metadata_digest(&self) -> [u8; 32] {
        self.metadata_digest
    }
    pub fn authorization_value(&self) -> Value {
        array([
            text("TPREQUESTTYPESIGNATURES1"),
            text("1"),
            encode_signature(&self.reply),
            self.progress.as_ref().map_or(Value::Null, encode_signature),
        ])
    }
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, CompileError> {
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(failure("request type signatures byte bound"));
        }
        let value = decode(bytes)?;
        let fields = row(&value, 4)?;
        if string(&fields[0])? != "TPREQUESTTYPESIGNATURES1" || string(&fields[1])? != "1" {
            return Err(failure("request type signatures version"));
        }
        let reply = decode_signature(&fields[2])?;
        let progress = match &fields[3] {
            Value::Null => None,
            value => Some(decode_signature(value)?),
        };
        if reply.key() != "request-reply"
            || progress
                .as_ref()
                .is_some_and(|signature| signature.key() != "request-progress")
        {
            return Err(failure("request type signatures purpose"));
        }
        Ok(Self {
            reply,
            progress,
            metadata_digest: Sha256::digest(bytes).into(),
        })
    }
}

impl<'de> serde::Deserialize<'de> for RequestTypeSignatures {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_bytes(&deserialize_type_evidence_bytes(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

impl ExactSignatureName {
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
pub(crate) struct CheckedItem {
    kind: CheckedItemKind,
    source: String,
    binders: Vec<String>,
    pins: Vec<Value>,
    expression: Option<Value>,
    signatures: Vec<ExactCheckedSignature>,
}

enum CheckedCellSpecification {
    Finite(Arc<CheckedCellRequest>),
    World {
        program: Arc<crate::compiler_world::CheckedWorldCellProgram>,
        injected: Vec<String>,
        reserved: Vec<String>,
    },
}

impl std::fmt::Debug for CheckedCellSpecification {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("CheckedCellSpecification")
            .field(&self.admission_digest())
            .finish()
    }
}

impl CheckedCellSpecification {
    fn admission_digest(&self) -> [u8; 32] {
        match self {
            Self::Finite(specification) => specification.admission_digest(),
            Self::World { program, .. } => *program.reservation().admission_digest(),
        }
    }
    fn specification_digest(&self) -> [u8; 32] {
        match self {
            Self::Finite(specification) => specification.recipe().specification_digest(),
            Self::World { program, .. } => *program.reservation().plan().digest(),
        }
    }
    fn template_sources(&self) -> Vec<String> {
        match self {
            Self::Finite(specification) => specification.template_sources(),
            Self::World { program, .. } => {
                let recipe = program.reservation().plan().recipe();
                std::iter::once(recipe.template.clone())
                    .chain(
                        recipe
                            .turn_templates
                            .iter()
                            .map(|(_, source)| source.clone()),
                    )
                    .collect()
            }
        }
    }
    fn injected_modules(&self) -> &[String] {
        match self {
            Self::Finite(specification) => specification.recipe().injected_modules(),
            Self::World { injected, .. } => injected,
        }
    }
    fn reserved_declaration_modules(&self) -> &[String] {
        match self {
            Self::Finite(specification) => specification.reserved_declaration_modules(),
            Self::World { reserved, .. } => reserved,
        }
    }
    fn turn_templates(&self) -> &[(String, String)] {
        match self {
            Self::Finite(specification) => specification.recipe().turn_templates(),
            Self::World { program, .. } => &program.reservation().plan().recipe().turn_templates,
        }
    }
}

enum CheckedCellItems {
    Finite(Vec<CheckedItem>),
    World(Arc<crate::compiler_world::CheckedWorldCellProgram>),
}

impl std::fmt::Debug for CheckedCellItems {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_list().entries(self.iter()).finish()
    }
}

impl From<Vec<CheckedItem>> for CheckedCellItems {
    fn from(items: Vec<CheckedItem>) -> Self {
        Self::Finite(items)
    }
}

impl CheckedCellItems {
    fn len(&self) -> usize {
        match self {
            Self::Finite(items) => items.len(),
            Self::World(program) => program.items().len(),
        }
    }
    fn get(&self, index: usize) -> Option<&CheckedItem> {
        match self {
            Self::Finite(items) => items.get(index),
            Self::World(program) => program
                .item(index)
                .map(|item| item.checked_item_observation()),
        }
    }
    fn iter(&self) -> impl Iterator<Item = &CheckedItem> {
        (0..self.len()).map(|index| &self[index])
    }
}

impl std::ops::Index<usize> for CheckedCellItems {
    type Output = CheckedItem;
    fn index(&self, index: usize) -> &Self::Output {
        self.get(index)
            .expect("checked item index belongs to its cell")
    }
}

#[derive(Debug)]
pub struct ExactCheckedCell {
    specification: CheckedCellSpecification,
    producer: [u8; 32],
    context: [u8; 32],
    declaration_context: Arc<crate::declaration_context::ExactDeclarationContext>,
    retained_projections: Vec<Arc<crate::declaration_join::AcceptedJoin>>,
    receipt_digest: [u8; 32],
    checked_source: Arc<str>,
    evidence: Vec<(String, Arc<crate::cache::CompletedSourceEvidence>)>,
    observations: Arc<[u8]>,
    items: CheckedCellItems,
    include: Vec<std::path::PathBuf>,
    planned_declaration: Option<PlannedCheckedDeclaration>,
    planned_declarations: BTreeMap<usize, PlannedCheckedDeclaration>,
    typed_segments: Vec<CheckedTypedSegmentPlan>,
    value_inputs: Arc<CheckedValueInputs>,
}

/// Compiler-issued normalization of one ordered inference segment. This
/// observation cannot reserve identities or construct a checked capability.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckedTypedSegmentPlan {
    digest: String,
    root: String,
    items: Vec<CheckedTypedSegmentItem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckedTypedSegmentItem {
    ordinal: usize,
    entry: String,
    generation: u64,
    body: CheckedTypedSegmentBody,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckedTypedSegmentBody {
    Action {
        step: String,
        probe: String,
        marker: String,
        captures: Vec<String>,
    },
    Let {
        marker: String,
        captures: Vec<String>,
    },
    Observation {
        probe: String,
        capture: String,
    },
}

impl CheckedTypedSegmentPlan {
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn root(&self) -> &str {
        &self.root
    }
    pub fn items(&self) -> &[CheckedTypedSegmentItem] {
        &self.items
    }
}

impl CheckedTypedSegmentItem {
    pub fn ordinal(&self) -> usize {
        self.ordinal
    }
    pub fn entry(&self) -> &str {
        &self.entry
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn body(&self) -> &CheckedTypedSegmentBody {
        &self.body
    }
}

impl CheckedTypedSegmentBody {
    /// Ordered capture names the compiler issued for this typed item. A world
    /// item's original binders and checked signatures must follow this order.
    pub(crate) fn captures(&self) -> Vec<&str> {
        match self {
            Self::Action { captures, .. } | Self::Let { captures, .. } => {
                captures.iter().map(String::as_str).collect()
            }
            Self::Observation { capture, .. } => vec![capture.as_str()],
        }
    }
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
        specification: &CheckedCellRequest,
        producer: &[u8],
        include: &[std::path::PathBuf],
        scratch: &Path,
    ) -> Result<Vec<Value>, CompileError> {
        use crate::cell_plan::ParsedCellPlanKind;
        if self.reservation_digest == [0; 32]
            || self.reservation_digest != specification.admission_digest()
            || self.parsed_plan.specification_digest()
                != specification.recipe().specification_digest()
            || !same_include_paths(self.parsed_plan.include_paths(), include)
            || self.parsed_plan.injected_modules() != specification.recipe().injected_modules()
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
                        observation_name,
                    },
                ) if *capture > 0
                    && values.insert(*capture)
                    && valid_observation_name(observation_name)
                    && observations.insert(observation_name.as_str()) =>
                {
                    vec![
                        text("expr"),
                        Value::Integer((*capture).into()),
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
        if libraries != specification.reserved_declaration_modules()
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

fn decode_typed_segment_plans(
    value: &Value,
    program: &CheckedPlannedCellSpecification,
) -> Result<Vec<CheckedTypedSegmentPlan>, CompileError> {
    decode_typed_segment_plan_facts(
        value,
        program.parsed_plan.items(),
        &program.slots,
        program.reservation_digest,
    )
}

pub(crate) fn decode_world_typed_segment_plans(
    value: &Value,
    reservation: &crate::cell_plan::CompilerWorldCellReservation,
) -> Result<Vec<CheckedTypedSegmentPlan>, CompileError> {
    decode_typed_segment_plan_facts(
        value,
        reservation.plan().items(),
        reservation.slots(),
        *reservation.admission_digest(),
    )
}

fn decode_typed_segment_plan_facts(
    value: &Value,
    parsed_items: &[crate::cell_plan::ParsedCellPlanItem],
    slots: &[CheckedPlannedCellSlot],
    reservation_digest: [u8; 32],
) -> Result<Vec<CheckedTypedSegmentPlan>, CompileError> {
    use crate::cell_plan::ParsedCellPlanKind as Kind;
    let expected = parsed_items
        .iter()
        .filter(|item| matches!(item.kind(), Kind::Bind | Kind::Expression))
        .map(|item| item.index())
        .collect::<Vec<_>>();
    let mut consumed = Vec::new();
    let mut plans = Vec::new();
    for value in list(value, 10000)? {
        let fields = row(value, 4)?;
        let digest = string(&fields[0])?;
        let reservation = string(&fields[1])?;
        let root = string(&fields[2])?;
        if reservation != hex(&reservation_digest) || root.is_empty() || root.len() > 65536 {
            return Err(failure("typed segment root or reservation differs"));
        }
        let mut hash = Sha256::new();
        let mut frame = |value: &str| {
            hash.update(value.len().to_string().as_bytes());
            hash.update(b":");
            hash.update(value.as_bytes());
        };
        frame("tidepool-typed-segment-plan-v1");
        frame(reservation);
        frame(root);
        let mut reserved = BTreeSet::from([root.to_owned()]);
        let mut generations = BTreeSet::new();
        let mut items = Vec::new();
        for value in list(&fields[3], 10000)? {
            let item = row(value, 8)?;
            let integer = |value: &Value| match value {
                Value::Integer(value) => u64::try_from(*value).map_err(failure),
                _ => Err(failure("typed segment item number is not integer")),
            };
            let ordinal = usize::try_from(integer(&item[0])?).map_err(failure)?;
            let entry = string(&item[1])?;
            let generation = integer(&item[2])?;
            let kind = string(&item[3])?;
            let step = string(&item[4])?;
            let probe = string(&item[5])?;
            let marker = string(&item[6])?;
            let captures = list(&item[7], 65536)?
                .iter()
                .map(|value| string(value).map(str::to_owned))
                .collect::<Result<Vec<_>, _>>()?;
            let parsed = parsed_items
                .get(ordinal)
                .ok_or_else(|| failure("typed segment item is outside parser plan"))?;
            if consumed.len() >= expected.len()
                || expected[consumed.len()] != ordinal
                || generation == 0
                || !generations.insert(generation)
                || entry.is_empty()
                || entry.len() > 65536
                || !reserved.insert(entry.to_owned())
                || captures
                    .iter()
                    .any(|name| name.is_empty() || name.len() > 65536)
                || captures.iter().collect::<BTreeSet<_>>().len() != captures.len()
                || items
                    .last()
                    .is_some_and(|prior: &CheckedTypedSegmentItem| prior.ordinal + 1 != ordinal)
            {
                return Err(failure("typed segment item order or identities differ"));
            }
            frame(&ordinal.to_string());
            frame(entry);
            frame(&generation.to_string());
            frame(kind);
            let body = match (kind, parsed.kind(), slots.get(ordinal)) {
                ("action", Kind::Bind, Some(CheckedPlannedCellSlot::Bind { value }))
                    if *value == generation
                        && captures == parsed.binders()
                        && matches!(
                            parsed.binding_form(),
                            Some(
                                crate::cell_plan::ParsedCellBindingForm::Action
                                    | crate::cell_plan::ParsedCellBindingForm::Recursive
                            )
                        ) =>
                {
                    for name in [step, probe, marker] {
                        if name.is_empty()
                            || name.len() > 65536
                            || !reserved.insert(name.to_owned())
                        {
                            return Err(failure("typed action reserved names differ"));
                        }
                        frame(name);
                    }
                    frame(&captures.len().to_string());
                    for name in &captures {
                        frame(name);
                    }
                    CheckedTypedSegmentBody::Action {
                        step: step.into(),
                        probe: probe.into(),
                        marker: marker.into(),
                        captures,
                    }
                }
                ("let", Kind::Bind, Some(CheckedPlannedCellSlot::Bind { value }))
                    if *value == generation
                        && step.is_empty()
                        && probe.is_empty()
                        && captures == parsed.binders()
                        && parsed.binding_form()
                            == Some(crate::cell_plan::ParsedCellBindingForm::Let) =>
                {
                    if marker.is_empty()
                        || marker.len() > 65536
                        || !reserved.insert(marker.to_owned())
                    {
                        return Err(failure("typed let reserved marker differs"));
                    }
                    frame(marker);
                    frame(&captures.len().to_string());
                    for name in &captures {
                        frame(name);
                    }
                    CheckedTypedSegmentBody::Let {
                        marker: marker.into(),
                        captures,
                    }
                }
                (
                    "observation",
                    Kind::Expression,
                    Some(CheckedPlannedCellSlot::Expression {
                        capture,
                        observation_name,
                    }),
                ) if *capture == generation
                    && marker == observation_name
                    && step.is_empty()
                    && captures.is_empty() =>
                {
                    for name in [probe, marker] {
                        if name.is_empty()
                            || name.len() > 65536
                            || !reserved.insert(name.to_owned())
                        {
                            return Err(failure("typed observation reserved names differ"));
                        }
                        frame(name);
                    }
                    CheckedTypedSegmentBody::Observation {
                        probe: probe.into(),
                        capture: marker.into(),
                    }
                }
                _ => {
                    return Err(failure(
                        "typed item capture plan differs from parser or reservation",
                    ))
                }
            };
            consumed.push(ordinal);
            items.push(CheckedTypedSegmentItem {
                ordinal,
                entry: entry.into(),
                generation,
                body,
            });
        }
        drop(frame);
        if items
            .last()
            .is_some_and(|last| expected.get(consumed.len()) == Some(&(last.ordinal + 1)))
        {
            return Err(failure(
                "typed inference segment was split without a declaration boundary",
            ));
        }
        let expected_digest: [u8; 32] = hash.finalize().into();
        if items.is_empty() || digest != hex(&expected_digest) {
            return Err(failure("typed segment normalization digest differs"));
        }
        plans.push(CheckedTypedSegmentPlan {
            digest: digest.into(),
            root: root.into(),
            items,
        });
    }
    if consumed != expected {
        return Err(failure("typed segment inventory omits parser items"));
    }
    Ok(plans)
}

/// Complete immutable output of one admitted compiler transaction. Preparing
/// interfaces never asserts that the corresponding native binding is live.
#[derive(Debug)]
pub struct CellProgram {
    pub(crate) checked: Arc<ExactCheckedCell>,
    pub(crate) basis: CellProgramBacking,
    pub(crate) slots: Vec<CheckedPlannedCellSlot>,
    pub(crate) items: Vec<CellProgramItem>,
}

/// Physical plan owner of an executable program. World evidence retains the
/// checked output that issued its reservation and item successor descriptions.
pub(crate) enum CellProgramBacking {
    Finite(Arc<crate::cell_plan::ParsedCellPlan>),
    World(Arc<crate::compiler_world::CheckedWorldCellProgram>),
}

impl std::fmt::Debug for CellProgramBacking {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Finite(plan) => formatter.debug_tuple("Finite").field(plan).finish(),
            Self::World(program) => formatter
                .debug_tuple("World")
                .field(program.certificate_sha256())
                .finish(),
        }
    }
}

pub enum CellProgramBasis<'a> {
    Finite(&'a Arc<crate::cell_plan::ParsedCellPlan>),
    World(&'a Arc<crate::compiler_world::CheckedWorldCellProgram>),
}

#[derive(Debug)]
pub struct CellProgramItem {
    pub(crate) context: CellProgramItemContext,
    pub(crate) checked: ExactCheckedItem,
    pub(crate) native: Option<Arc<ExactCompiledItem>>,
    pub(crate) native_observations: Option<CellProgramObservations>,
}

#[derive(Debug)]
pub(crate) struct CellProgramItemContext {
    pub(crate) context: Arc<crate::declaration_context::ExactDeclarationContext>,
    pub(crate) source_lexical: Vec<crate::declaration_join::ExactLexicalNode>,
}

#[derive(Debug)]
pub(crate) struct CellProgramObservations {
    pub(crate) turn: Arc<[u8]>,
    pub(crate) products: Arc<crate::artifacts::SealedTurnProducts>,
}

impl CellProgram {
    pub fn checked_cell(&self) -> &Arc<ExactCheckedCell> {
        &self.checked
    }
    pub fn basis(&self) -> CellProgramBasis<'_> {
        match &self.basis {
            CellProgramBacking::Finite(plan) => CellProgramBasis::Finite(plan),
            CellProgramBacking::World(program) => CellProgramBasis::World(program),
        }
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
    pub fn typed_segments(&self) -> &[CheckedTypedSegmentPlan] {
        &self.checked.typed_segments
    }
    /// Validate the exact native entry issued for every normalized item before
    /// the runtime opens an ordered execution prefix.
    pub fn validate_typed_entries(&self) -> Result<(), CompileError> {
        for plan in self.typed_segments() {
            for planned in plan.items() {
                let native = self
                    .items
                    .get(planned.ordinal())
                    .and_then(CellProgramItem::native)
                    .ok_or_else(|| failure("normalized item has no native entry"))?;
                let proof = native
                    .typed_entry()
                    .ok_or_else(|| failure("native entry lacks its typed origin"))?;
                if proof.plan_digest() != plan.digest()
                    || proof.origin().occurrence != plan.root()
                    || proof.entry().occurrence != planned.entry()
                    || native.generation() != planned.generation()
                    || native.item().index() != planned.ordinal()
                    || !Arc::ptr_eq(&native.item().cell, &self.checked)
                {
                    return Err(failure(
                        "native typed entry differs from the normalized ordered item",
                    ));
                }
            }
        }
        Ok(())
    }
}

impl CellProgramItem {
    pub(crate) fn compiler_context(
        &self,
    ) -> &Arc<crate::declaration_context::ExactDeclarationContext> {
        &self.context.context
    }
    pub(crate) fn source_lexical(&self) -> &[crate::declaration_join::ExactLexicalNode] {
        &self.context.source_lexical
    }
    pub fn checked_item(&self) -> &ExactCheckedItem {
        &self.checked
    }
    pub fn native(&self) -> Option<&Arc<ExactCompiledItem>> {
        self.native.as_ref()
    }
    pub fn native_turn_bytes(&self) -> Option<&[u8]> {
        self.native_observations
            .as_ref()
            .map(|value| value.turn.as_ref())
    }
    pub fn native_products(&self) -> Option<&Arc<crate::artifacts::SealedTurnProducts>> {
        self.native_observations
            .as_ref()
            .map(|value| &value.products)
    }
}

/// Checked baselines borrow the issuing value rows. This directory contains
/// only fresh outputs; membership never establishes input authority.
#[derive(Debug)]
pub(crate) struct CheckedValueInputs {
    directory: Arc<ValueInterfaceDirectory>,
    baseline: Vec<CapturedValueInterface>,
    initial_bytes: u64,
    output_files_hashed: AtomicU64,
    output_bytes_hashed: AtomicU64,
}

/// Same-request type outputs captured from the reserved checked-value owner.
/// These are not submitted inputs and grant no live native values.
#[derive(Clone, Debug)]
pub(crate) struct ProducedValueTypeInterfaces {
    interfaces: Arc<[ProducedValueTypeInterface]>,
    lookup: Arc<ProducedValueTypeLookup>,
    selection: ProducedValueTypeSelection,
}

#[derive(Debug)]
struct ProducedValueTypeLookup {
    artifacts: BTreeMap<crate::artifact_inventory::ArtifactId, usize>,
    generations: BTreeMap<u64, usize>,
}

#[derive(Clone, Debug)]
struct ProducedValueTypeSelection(std::ops::Range<usize>);

impl ProducedValueTypeSelection {
    fn includes(&self, ordinal: usize) -> bool {
        ordinal < self.0.end
    }

    fn requires(&self, ordinal: usize) -> bool {
        self.0.contains(&ordinal)
    }

    fn for_item(&self, ordinal: usize) -> Result<Self, CompileError> {
        if !self.requires(ordinal) {
            return Err(failure(
                "produced type item differs from its reserved segment",
            ));
        }
        Ok(Self(ordinal..ordinal + 1))
    }
}

#[cfg(test)]
mod produced_type_selection_tests {
    use super::ProducedValueTypeSelection;

    #[test]
    fn item_prefix_uses_ordinals_with_sparse_generations_and_empty_slots() {
        for generations in [[901, 40, 900, 3, 1000], [2, 9900, 1, 550, 7]] {
            // Prior binding, declaration barrier, current segment binding,
            // discard binding, observation, binding, and next-segment binding.
            // These are ordinal policy facts, not compiler certificates.
            let slots = [
                Some(generations[0]),
                None,
                Some(generations[1]),
                None,
                Some(generations[2]),
                Some(generations[3]),
                Some(generations[4]),
            ];
            let captured = slots
                .iter()
                .enumerate()
                .filter_map(|(ordinal, generation)| generation.map(|value| (ordinal, value)))
                .collect::<Vec<_>>();
            let segment = ProducedValueTypeSelection(2..6);
            let mut expected = vec![generations[0]];
            for ordinal in 2..6 {
                if let Some(value) = slots[ordinal] {
                    expected.push(value);
                }
                let item = segment.for_item(ordinal).unwrap();
                let visible = captured
                    .iter()
                    .filter(|(index, _)| item.includes(*index))
                    .map(|(_, generation)| *generation)
                    .collect::<Vec<_>>();
                let required = captured
                    .iter()
                    .filter(|(index, _)| item.requires(*index))
                    .map(|(_, generation)| *generation)
                    .collect::<Vec<_>>();
                assert_eq!(visible, expected);
                assert_eq!(required, slots[ordinal].into_iter().collect::<Vec<_>>());
                assert!(
                    item.for_item(ordinal + 1).is_err(),
                    "a selected view cannot widen"
                );
                assert!(
                    item.for_item(ordinal - 1).is_err(),
                    "a selected view cannot change its current output"
                );
            }
            assert!(segment.for_item(1).is_err());
            assert!(segment.for_item(6).is_err());
            let declaration = ProducedValueTypeSelection(2..2);
            assert!(declaration.includes(0));
            assert!(!declaration.includes(2));
            assert!((0..slots.len()).all(|ordinal| !declaration.requires(ordinal)));
            assert!(declaration.for_item(2).is_err());
        }
    }
}

#[derive(Debug)]
struct ProducedValueTypeInterface {
    ordinal: usize,
    interface: Arc<crate::recovery_artifacts::CertifiedValueInterface>,
    input: ValueInterfaceBytes,
    // One produced output may be consumed through Ordinary and Prepared views.
    // Physical promotion belongs to this issuer, independently of either view.
    row: crate::declaration_context::CheckedValueRowIssuer,
}

impl ProducedValueTypeInterfaces {
    fn from_outputs(
        interfaces: Vec<ProducedValueTypeInterface>,
        segment: std::ops::Range<usize>,
    ) -> Result<Self, CompileError> {
        let mut lookup = ProducedValueTypeLookup {
            artifacts: BTreeMap::new(),
            generations: BTreeMap::new(),
        };
        for (index, output) in interfaces.iter().enumerate() {
            if output.ordinal >= segment.end
                || (index > 0 && interfaces[index - 1].ordinal >= output.ordinal)
                || output.input.owner.kind != tidepool_repr::SessionModuleKind::Val
                || lookup
                    .artifacts
                    .insert(output.interface.artifact_id(), index)
                    .is_some()
                || lookup
                    .generations
                    .insert(output.input.owner.gen.0, index)
                    .is_some()
            {
                return Err(failure(
                    "produced type inventory repeats or reorders an output",
                ));
            }
        }
        Ok(Self {
            interfaces: interfaces.into(),
            lookup: Arc::new(lookup),
            selection: ProducedValueTypeSelection(segment),
        })
    }

    /// Keep complete physical output custody private while selecting only the
    /// current segment's mandatory outputs and preceding type evidence.
    pub(crate) fn for_segment(
        &self,
        segment: std::ops::Range<usize>,
    ) -> Result<Self, CompileError> {
        if segment.start < self.selection.0.start
            || segment.end > self.selection.0.end
            || segment.start > segment.end
        {
            return Err(failure(
                "produced type segment widens physical output selection",
            ));
        }
        Ok(Self {
            interfaces: self.interfaces.clone(),
            lookup: self.lookup.clone(),
            selection: ProducedValueTypeSelection(segment),
        })
    }

    fn output(
        &self,
        owner: tidepool_repr::SessionModule,
    ) -> Result<&ProducedValueTypeInterface, CompileError> {
        self.lookup
            .generations
            .get(&owner.gen.0)
            .map(|index| &self.interfaces[*index])
            .filter(|output| self.selection.requires(output.ordinal) && output.input.owner == owner)
            .ok_or_else(|| failure("current item lacks its captured value interface"))
    }

    pub(crate) fn current_interface(
        &self,
        generation: u64,
    ) -> Result<&Arc<crate::recovery_artifacts::CertifiedValueInterface>, CompileError> {
        Ok(&self
            .output(tidepool_repr::SessionModule::val(
                tidepool_repr::Generation(generation),
            ))?
            .interface)
    }

    pub(crate) fn owns_artifact(&self, artifact: crate::artifact_inventory::ArtifactId) -> bool {
        self.lookup.artifacts.contains_key(&artifact)
    }

    pub(crate) fn selects_artifact(&self, artifact: crate::artifact_inventory::ArtifactId) -> bool {
        self.lookup
            .artifacts
            .get(&artifact)
            .is_some_and(|index| self.selection.includes(self.interfaces[*index].ordinal))
    }

    /// Select one actual item from the once-captured segment inventory. Earlier
    /// outputs may supply type dependencies; its own output is mandatory and
    /// later outputs cannot authorize any row in this item's packet.
    pub(crate) fn for_item(&self, ordinal: usize) -> Result<Self, CompileError> {
        Ok(Self {
            interfaces: self.interfaces.clone(),
            lookup: self.lookup.clone(),
            selection: self.selection.for_item(ordinal)?,
        })
    }

    /// A byte-equal recapture is not a view of this physical output owner.
    /// Every view preserves the paired immutable entries and lookup; only its
    /// ordinal selection changes.
    pub(crate) fn is_item_of(&self, segment: &Self) -> bool {
        Arc::ptr_eq(&self.lookup, &segment.lookup)
            && self.selection.0.start.checked_add(1) == Some(self.selection.0.end)
            && segment.selection.requires(self.selection.0.start)
    }

    pub(crate) fn before_segment(&self) -> Self {
        Self {
            interfaces: Arc::clone(&self.interfaces),
            lookup: Arc::clone(&self.lookup),
            selection: ProducedValueTypeSelection(self.selection.0.start..self.selection.0.start),
        }
    }

    pub(crate) fn follows(&self, previous: &Self) -> bool {
        Arc::ptr_eq(&self.lookup, &previous.lookup)
            && self.selection.0.start == previous.selection.0.end
            && self.selection.0.start.checked_add(1) == Some(self.selection.0.end)
    }

    pub(crate) fn matches_artifact(
        &self,
        entry: &crate::artifact_inventory::ArtifactEntry,
    ) -> bool {
        matches!(
            &entry.payload,
            crate::artifact_inventory::ArtifactPayload::Interface(
                _,
                crate::artifact_inventory::JoinedInterfaceRole::ValueInterface
            )
        ) && self.selects_artifact(entry.descriptor.id)
    }

    pub(crate) fn interfaces(
        &self,
    ) -> impl Iterator<Item = &Arc<crate::recovery_artifacts::CertifiedValueInterface>> {
        let end = self
            .interfaces
            .partition_point(|output| self.selection.includes(output.ordinal));
        self.interfaces[..end]
            .iter()
            .map(|output| &output.interface)
    }

    pub(crate) fn required(
        &self,
    ) -> impl Iterator<Item = &Arc<crate::recovery_artifacts::CertifiedValueInterface>> {
        let start = self
            .interfaces
            .partition_point(|output| output.ordinal < self.selection.0.start);
        let end = self
            .interfaces
            .partition_point(|output| self.selection.includes(output.ordinal));
        self.interfaces[start..end]
            .iter()
            .map(|output| &output.interface)
    }
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

/// Exact interface bytes are distinct from compiler-certified output authority.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ValueInterfaceBytes {
    owner: tidepool_repr::SessionModule,
    module: String,
    bytes: Arc<[u8]>,
    path: std::path::PathBuf,
    digest: String,
}

#[derive(Debug)]
struct ValueInterfaceDirectory(tempfile::TempDir);

impl PartialEq for ValueInterfaceDirectory {
    fn eq(&self, other: &Self) -> bool {
        self.0.path() == other.0.path()
    }
}
impl Eq for ValueInterfaceDirectory {}

#[derive(Clone, Debug)]
enum CapturedValueInterface {
    #[cfg(test)]
    Raw(ValueInterfaceBytes),
    Certified {
        input: ValueInterfaceBytes,
        original: Arc<CheckedValueArtifact>,
    },
}

impl CapturedValueInterface {
    fn input(&self) -> &ValueInterfaceBytes {
        match self {
            #[cfg(test)]
            Self::Raw(input) => input,
            Self::Certified { input, .. } => input,
        }
    }
}

/// An original checked output owns its certification and selected compiler
/// support, including the exact native byte custody needed by later consumers.
/// Its value-sized immutable row survives independently of producing staging.
#[derive(Debug)]
pub struct CheckedValueArtifact {
    interface: ValueInterfaceBytes,
    authority: ([u8; 32], [u8; 32]),
    certified_interface: Arc<crate::recovery_artifacts::CertifiedValueInterface>,
    compiler_support: crate::artifact_inventory::CompilerSourceSupport,
    source_lexical: Vec<crate::declaration_join::ExactLexicalNode>,
    template_imports: Option<Arc<crate::declaration_context::TemplateImportEvidence>>,
    row: Arc<crate::declaration_context::RetainedArtifactRow>,
    original_interface_prototype: Option<Arc<ExactHostBindingPrototype>>,
}

impl PartialEq for CheckedValueArtifact {
    fn eq(&self, other: &Self) -> bool {
        self.interface == other.interface
            && Arc::ptr_eq(&self.row, &other.row)
            && self.authority == other.authority
            && self.certified_interface == other.certified_interface
            && self.compiler_support == other.compiler_support
            && self.source_lexical == other.source_lexical
            && self.template_imports == other.template_imports
            && self.original_interface_prototype == other.original_interface_prototype
    }
}
impl Eq for CheckedValueArtifact {}

/// Type-only issuance purpose. Neither purpose grants runtime value transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingInterfacePurpose {
    HostBuilt,
    OriginalLiveInput,
}

#[derive(Debug)]
enum BindingInterfaceEvidence {
    HostBuilt { original_binder: Value },
    OriginalLiveInput { witness: CanonicalInputTypeWitness },
}

/// Reusable native type authority extracted from one original compiler proof.
/// It retains only the type's original interface closure, never a new binder's
/// runtime admission or an authored execution claim.
#[derive(Debug)]
pub struct ExactHostBindingPrototype {
    signature: ExactCheckedSignature,
    evidence: BindingInterfaceEvidence,
    type_interfaces: Arc<crate::declaration_context::SelectedTypeInterfaces>,
    producer: [u8; 32],
    digest: [u8; 32],
}

impl PartialEq for ExactHostBindingPrototype {
    fn eq(&self, other: &Self) -> bool {
        self.digest == other.digest
    }
}
impl Eq for ExactHostBindingPrototype {}

impl ExactHostBindingPrototype {
    /// The retained native TYPE witness; this read grants no binding or execution.
    pub fn signature(&self) -> &ExactCheckedSignature {
        &self.signature
    }

    pub fn type_interfaces(&self) -> &Arc<crate::declaration_context::SelectedTypeInterfaces> {
        &self.type_interfaces
    }
    pub fn producer(&self) -> [u8; 32] {
        self.producer
    }
    pub fn from_checked(execution: &ExactCompiledItem) -> Result<Arc<Self>, CompileError> {
        let [signature] = execution.item().signatures() else {
            return Err(failure(
                "host prototype requires one original native signature",
            ));
        };
        let [binder] = execution.bound_binders.as_slice() else {
            return Err(failure("host prototype requires one original binder"));
        };
        let fields = row(binder, 7)?;
        let expected_key = match execution.typed_entry() {
            Some(entry) => {
                typed_capture_signature_key(&entry.entry().occurrence, string(&fields[0])?)
            }
            None => format!(
                "__tidepool_cell_pin_{}_{}",
                execution.item().index(),
                string(&fields[0])?
            ),
        };
        if execution.item().kind() != CheckedItemKind::Bind
            || !matches!(string(&fields[6])?, "JsonValue" | "Text" | "CommandJob")
            || signature.key() != expected_key
        {
            return Err(failure("host prototype lacks original host type authority"));
        }
        let interface = execution
            .value_interface_certificate()
            .ok_or_else(|| failure("host prototype lacks its original value interface"))?;
        let context = Arc::new(
            crate::declaration_context::ExactDeclarationContext::checked_value_type_interfaces(
                &interface, signature,
            )?,
        );
        let producer = context.toolchain_identity_sha256();
        if producer != execution.item.cell.producer {
            return Err(failure(
                "host prototype value interface differs from its checked producer",
            ));
        }
        let mut encoded = Vec::new();
        ciborium::ser::into_writer(&encode_signature(signature), &mut encoded)
            .expect("signature encodes to memory");
        let mut hash = Sha256::new();
        hash.update(b"tidepool-host-binding-prototype-2");
        hash.update(producer);
        hash.update(context.semantic_sha256());
        hash.update(&encoded);
        Ok(Arc::new(Self {
            signature: signature.clone(),
            evidence: BindingInterfaceEvidence::HostBuilt {
                original_binder: binder.clone(),
            },
            type_interfaces: context,
            producer,
            digest: hash.finalize().into(),
        }))
    }

    /// Derive type authority from an authenticated original site context. The
    /// runtime pairs this proof with that site's separately owned live payload.
    pub fn from_original_input(
        witness: CanonicalInputTypeWitness,
        original_type_interfaces: Arc<crate::declaration_context::SelectedTypeInterfaces>,
    ) -> Result<Arc<Self>, CompileError> {
        let producer = original_interfaces.toolchain_identity_sha256();
        if producer == [0; 32] {
            return Err(failure(
                "original input context lacks its compiler producer",
            ));
        }
        let context = Arc::new(original_interfaces.select(&[&witness], &[])?);
        let mut encoded = Vec::new();
        ciborium::ser::into_writer(&encode_signature(witness.signature()), &mut encoded)
            .expect("signature encodes to memory");
        let mut digest = Sha256::new();
        digest.update(b"tidepool-original-live-input-prototype-2");
        digest.update(producer);
        digest.update(context.semantic_sha256());
        digest.update(witness.metadata_digest());
        digest.update(witness.commitment());
        digest.update(&encoded);
        Ok(Arc::new(Self {
            signature: witness.signature().clone(),
            evidence: BindingInterfaceEvidence::OriginalLiveInput { witness },
            type_interfaces: context,
            producer,
            digest: digest.finalize().into(),
        }))
    }

    pub fn purpose(&self) -> BindingInterfacePurpose {
        match self.evidence {
            BindingInterfaceEvidence::HostBuilt { .. } => BindingInterfacePurpose::HostBuilt,
            BindingInterfaceEvidence::OriginalLiveInput { .. } => {
                BindingInterfacePurpose::OriginalLiveInput
            }
        }
    }

    pub fn original_input_type(&self) -> Option<&CanonicalInputTypeWitness> {
        match &self.evidence {
            BindingInterfaceEvidence::OriginalLiveInput { witness } => Some(witness),
            BindingInterfaceEvidence::HostBuilt { .. } => None,
        }
    }

    fn purpose_offer(&self) -> Value {
        match &self.evidence {
            BindingInterfaceEvidence::HostBuilt { .. } => array([text("host-built")]),
            BindingInterfaceEvidence::OriginalLiveInput { witness } => array([
                text("original-live-input"),
                Value::Bytes(witness.original_bytes().to_vec()),
            ]),
        }
    }

    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }

    pub(crate) fn prepare_interface_offer(
        self: &Arc<Self>,
        producer: &[u8],
        admission: [u8; 32],
        generation: u64,
        binding: &str,
    ) -> Result<HostBindingInterfaceOffer, CompileError> {
        if admission == [0; 32]
            || generation == 0
            || binding.is_empty()
            || crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(producer)
                .sha256()
                != self.producer
        {
            return Err(failure(
                "host interface offer differs from its original producer or reservation",
            ));
        }
        let directory = Arc::new(ValueInterfaceDirectory(
            tempfile::Builder::new()
                .prefix("tidepool-host-interface-")
                .tempdir()?,
        ));
        let request = self
            .type_interfaces
            .prepare_compilation(&directory.0.path().join("inputs"), producer)?;
        let value = array([
            text("TPHOSTBINDINGINTERFACE"),
            text("2"),
            text(hex(&self.producer)),
            text(hex(&admission)),
            Value::Integer(generation.into()),
            text(binding),
            encode_signature(&self.signature),
            text(request.manifest.to_string_lossy()),
            text(directory.0.path().to_string_lossy()),
            self.purpose_offer(),
        ]);
        let mut encoded = Vec::new();
        ciborium::ser::into_writer(&value, &mut encoded).expect("host offer encodes to memory");
        if encoded.len() > 4 << 20 {
            return Err(failure("host interface offer exceeds byte bound"));
        }
        Ok(HostBindingInterfaceOffer {
            prototype: self.clone(),
            directory,
            request,
            encoded,
            admission,
            generation,
            binding: binding.to_owned(),
        })
    }
}

pub(crate) struct HostBindingInterfaceOffer {
    prototype: Arc<ExactHostBindingPrototype>,
    directory: Arc<ValueInterfaceDirectory>,
    pub(crate) request: crate::declaration_context::ExactCompilationRequest,
    pub(crate) encoded: Vec<u8>,
    admission: [u8; 32],
    generation: u64,
    binding: String,
}

/// One compiler-issued fresh interface. This certifies host binding identity,
/// not execution of an authored Haskell cell or its placeholder.
#[derive(Debug)]
pub struct ExactHostBindingInterface {
    prototype: Arc<ExactHostBindingPrototype>,
    admission: [u8; 32],
    generation: u64,
    binder: Value,
    interface: Arc<CheckedValueArtifact>,
    original_input_type: Option<CanonicalInputTypeWitness>,
}

impl ExactHostBindingInterface {
    pub fn purpose(&self) -> BindingInterfacePurpose {
        self.prototype.purpose()
    }
    pub fn original_input_type(&self) -> Option<&CanonicalInputTypeWitness> {
        self.original_input_type.as_ref()
    }
    pub fn prototype(&self) -> &Arc<ExactHostBindingPrototype> {
        &self.prototype
    }
    pub fn admission_digest(&self) -> [u8; 32] {
        self.admission
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn binder(&self) -> &Value {
        &self.binder
    }
    pub fn value_interface_certificate(&self) -> Arc<CheckedValueArtifact> {
        self.interface.clone()
    }
}

impl HostBindingInterfaceOffer {
    pub(crate) fn seal(
        self,
        receipt: &[u8],
    ) -> Result<Arc<ExactHostBindingInterface>, CompileError> {
        self.request.validate_artifacts()?;
        if receipt.len() > 4 << 20 {
            return Err(failure("binding interface receipt exceeds byte bound"));
        }
        let decoded = decode(receipt)?;
        let mut canonical = Vec::new();
        ciborium::ser::into_writer(&decoded, &mut canonical).expect("receipt encodes to memory");
        if canonical != receipt {
            return Err(failure("binding interface receipt uses noncanonical CBOR"));
        }
        let fields = row(&decoded, 12)?;
        let mut signature = Vec::new();
        ciborium::ser::into_writer(&encode_signature(&self.prototype.signature), &mut signature)
            .expect("signature encodes to memory");
        if string(&fields[0])? != "TPHOSTBINDINGINTERFACERECEIPT"
            || string(&fields[1])? != "2"
            || string(&fields[2])? != hash(&self.encoded)
            || string(&fields[3])? != hex(&self.prototype.producer)
            || string(&fields[4])? != hex(&self.admission)
            || fields[5] != Value::Integer(self.generation.into())
            || string(&fields[8])? != hash(&signature)
        {
            return Err(failure(
                "host interface receipt differs from its exact offer",
            ));
        }
        let binder = row(&fields[6], 7)?;
        let owner = tidepool_repr::SessionModule::val(tidepool_repr::Generation(self.generation));
        if string(&binder[0])? != self.binding || string(&binder[2])? != owner.module_name() {
            return Err(failure("binding interface has another reserved binder"));
        }
        let original_input_type = match &self.prototype.evidence {
            BindingInterfaceEvidence::HostBuilt { original_binder } => {
                let original = row(original_binder, 7)?;
                if fields[11] != array([text("host-built")])
                    || [3, 5, 6]
                        .into_iter()
                        .any(|index| binder[index] != original[index])
                {
                    return Err(failure(
                        "host interface has another purpose or original nominal type",
                    ));
                }
                None
            }
            BindingInterfaceEvidence::OriginalLiveInput { witness } => {
                let purpose = row(&fields[11], 2)?;
                let Value::Bytes(bytes) = &purpose[1] else {
                    return Err(failure(
                        "original input receipt lacks its canonical witness",
                    ));
                };
                let issued = CanonicalInputTypeWitness::from_bytes(bytes)?;
                if string(&purpose[0])? != "original-live-input"
                    || &issued != witness
                    || issued.signature.names() != witness.signature.names()
                    || !matches!(string(&binder[3])?, "ForceData" | "RetainOpaque")
                    || binder[6] != Value::Null
                {
                    return Err(failure(
                        "original input receipt differs in type, purpose or host authority",
                    ));
                }
                Some(issued)
            }
        };
        let path = self.directory.0.path().join(owner.relative_hi_path());
        let output = CapturedValueInterfaceOutput::capture(&path)?;
        let bytes: Arc<[u8]> = output.interface.clone().into();
        let digest = hash(&bytes);
        let certificate = output.certify(
            self.prototype.producer,
            owner,
            Some([
                string(&fields[7])?,
                string(&fields[9])?,
                string(&fields[10])?,
            ]),
        )?;
        let context = self.prototype.type_interfaces
            .with_host_value_interfaces(std::slice::from_ref(&certificate))?;
        let row =
            crate::declaration_context::RetainedArtifactMaterialization::capture_checked_value(
                context.compiler_source_support(),
                crate::declaration_context::CheckedValueRowSource::Fresh(&certificate),
            )?;
        let compiler_support = context
            .compiler_source_support()
            .select_value_interface(&certificate)?;
        let path = row.checked_value_interface().path.clone();
        let interface = Arc::new(CheckedValueArtifact {
            interface: ValueInterfaceBytes {
                owner,
                module: owner.module_name(),
                digest,
                bytes,
                path,
            },
            authority: (self.prototype.producer, Sha256::digest(receipt).into()),
            certified_interface: certificate,
            compiler_support,
            source_lexical: Vec::new(),
            template_imports: None,
            row,
            original_interface_prototype: Some(self.prototype.clone()),
        });
        Ok(Arc::new(ExactHostBindingInterface {
            prototype: self.prototype,
            admission: self.admission,
            generation: self.generation,
            binder: fields[6].clone(),
            interface,
            original_input_type,
        }))
    }
}

/// The exact checked interface bytes named by one compiler authorization.
/// This permits their imports without making other hydrated owners lexical.
#[derive(Clone, Default)]
pub(crate) struct CheckedValueImportAuthority {
    values: Arc<BTreeMap<String, CapturedValueInterface>>,
}

impl CheckedValueImportAuthority {
    /// Borrow only the exact rows actually selected by this preparation. The
    /// checked program owner has already correlated each input reference with
    /// its current occurrence, runtime identity and original issuing row, and
    /// a raw preparation completion cannot reach this constructor.
    pub(crate) fn from_world_stage(
        program: &crate::compiler_world::CheckedWorldCellProgram,
        stage: &Arc<crate::compiler_world::CheckedWorldCellStage>,
    ) -> Result<Self, CompileError> {
        if !program
            .stages()
            .get(stage.index())
            .is_some_and(|issued| Arc::ptr_eq(issued, stage))
        {
            return Err(failure(
                "world value imports belong to another prepared stage",
            ));
        }
        let operation = program.validation().inventory.as_ref();
        let producer = program.reservation().plan().producer_sha256();
        let mut values = BTreeMap::<String, CapturedValueInterface>::new();
        // The operation's actual operand is compiler availability even when
        // this stage has no runtime demand for it. This does not alter the
        // stage's selected runtime witnesses or native root custody.
        let operand = program.reservation().plan().operand();
        for original in stage
            .demanded_rows()
            .iter()
            .chain(operand.map(|operand| operand.checked_row()))
        {
            crate::host_work::checkpoint()?;
            operation
                .charge(1)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            if &original.authority.0 != producer {
                return Err(failure("world value import has another compiler producer"));
            }
            if let Some(CapturedValueInterface::Certified {
                original: previous, ..
            }) = values.get(&original.interface.module)
            {
                if !previous.same_world_type_row(original) {
                    return Err(failure("world value import has competing issuing rows"));
                }
                continue;
            }
            operation
                .reserve::<(String, CapturedValueInterface, [usize; 8])>(1)
                .and_then(|()| {
                    operation.charge(
                        original
                            .interface
                            .module
                            .len()
                            .saturating_mul(2)
                            .saturating_add(original.interface.path.as_os_str().len())
                            .saturating_add(original.interface.digest.len()),
                    )
                })
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            values.insert(
                original.interface.module.clone(),
                CapturedValueInterface::Certified {
                    input: original.interface.clone(),
                    original: Arc::clone(original),
                },
            );
        }
        Ok(Self {
            values: Arc::new(values),
        })
    }

    fn capture(values: impl Iterator<Item = CapturedValueInterface>) -> Self {
        Self {
            values: Arc::new(
                values
                    .map(|value| (value.input().module.clone(), value))
                    .collect(),
            ),
        }
    }

    pub(crate) fn owners(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values.keys().map(|module| ("main", module.as_str()))
    }

    pub(crate) fn value_owners(
        &self,
    ) -> impl ExactSizeIterator<Item = tidepool_repr::SessionModule> + '_ {
        self.values.values().map(|value| value.input().owner)
    }

    /// Borrow the selected values' exact thin TYPE rows and their dependencies.
    /// Their historical source and native support stays with the checked value.
    pub(crate) fn selected_type_interfaces(
        &self,
        operation: &tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<Vec<Arc<crate::declaration_context::SelectedTypeInterfaces>>, CompileError> {
        operation
            .reserve::<Arc<crate::declaration_context::SelectedTypeInterfaces>>(self.values.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let mut contexts = Vec::with_capacity(self.values.len());
        for value in self.values.values() {
            crate::host_work::checkpoint()?;
            operation
                .charge(1)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            let original = match value {
                CapturedValueInterface::Certified { original, .. } => original,
                #[cfg(test)]
                CapturedValueInterface::Raw(_) => {
                    return Err(failure("World TYPE inputs require an issued checked value"));
                }
            };
            contexts.push(Arc::new(
                crate::declaration_context::ExactDeclarationContext::checked_value_interface_context(
                    original,
                )?,
            ));
        }
        Ok(contexts)
    }

    pub(crate) fn matches_interface(
        &self,
        interface: &crate::recovery_artifacts::CertifiedJoinedInterface,
    ) -> bool {
        interface.unit() == "main"
            && self
                .values
                .get(interface.module())
                .is_some_and(|value| match value {
                    CapturedValueInterface::Certified { original, .. } => {
                        original.certified_interface.interface() == interface
                    }
                    #[cfg(test)]
                    CapturedValueInterface::Raw(input) => {
                        interface.interface_bytes() == input.bytes.as_ref()
                            && hash(interface.interface_bytes()) == input.digest
                    }
                })
    }

    pub(crate) fn validate(&self) -> Result<(), CompileError> {
        for value in self.values.values() {
            match value {
                // The selected request borrows the already authenticated row,
                // including its immutable transport and actual descriptor lease.
                CapturedValueInterface::Certified { .. } => {}
                #[cfg(test)]
                CapturedValueInterface::Raw(input) => {
                    let observed = read(&input.path, 32 * 1024 * 1024)?;
                    if hash(&observed) != input.digest
                        || observed.as_slice() != input.bytes.as_ref()
                    {
                        return Err(failure("checked value import interface changed"));
                    }
                }
            }
        }
        Ok(())
    }
}

pub(crate) enum CheckedValueCaptureInput<'a> {
    Ordinary {
        produced: Option<&'a ProducedValueTypeInterfaces>,
    },
    Prepared {
        support: &'a crate::declaration_context::PreparedValueSourceSupport,
        produced: &'a ProducedValueTypeInterfaces,
    },
}

impl CheckedValueInputs {
    pub(crate) fn capture_produced_types(
        &self,
        producer: [u8; 32],
        planned: &CheckedPlannedCellSpecification,
        segment: std::ops::Range<usize>,
        validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<ProducedValueTypeInterfaces, CompileError> {
        use crate::cell_plan::ParsedCellPlanKind;
        if producer != planned.parsed_plan.producer_sha256()
            || segment.start > segment.end
            || segment.end > planned.slots.len()
            || planned.slots.len() != planned.parsed_plan.items().len()
        {
            return Err(failure(
                "produced type outputs differ from the reserved segment",
            ));
        }
        let mut interfaces = Vec::new();
        for (ordinal, (item, slot)) in planned.parsed_plan.items()[..segment.end]
            .iter()
            .zip(&planned.slots)
            .enumerate()
        {
            let generation = match (item.kind(), slot) {
                (ParsedCellPlanKind::Bind, CheckedPlannedCellSlot::Bind { value })
                    if !item.binders().is_empty() =>
                {
                    *value
                }
                (
                    ParsedCellPlanKind::Expression,
                    CheckedPlannedCellSlot::Expression { capture, .. },
                ) => *capture,
                _ => continue,
            };
            let owner = tidepool_repr::SessionModule::val(tidepool_repr::Generation(generation));
            if self
                .baseline
                .iter()
                .any(|value| value.input().owner == owner)
            {
                return Err(failure("produced type output replaces a submitted value"));
            }
            let path = self.root().join(owner.relative_hi_path());
            let captured = CapturedValueInterfaceOutput::capture(&path)?;
            validation
                .inventory
                .reserve::<ProducedValueTypeInterface>(1)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            validation
                .inventory
                .charge(
                    captured
                        .interface
                        .len()
                        .checked_add(captured.packages.len())
                        .and_then(|bytes| bytes.checked_add(captured.requirements.len()))
                        .ok_or_else(|| failure("produced type output accounting overflow"))?,
                )
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            let interface = captured.certify(producer, owner, None)?;
            let bytes = interface.interface_bytes_owned();
            let digest = hash(&bytes);
            self.output_files_hashed.fetch_add(1, Ordering::Relaxed);
            self.output_bytes_hashed
                .fetch_add(bytes.len() as u64, Ordering::Relaxed);
            interfaces.push(ProducedValueTypeInterface {
                row: crate::declaration_context::CheckedValueRowIssuer::new(interface.clone()),
                ordinal,
                input: ValueInterfaceBytes {
                    owner,
                    module: owner.module_name(),
                    bytes,
                    path,
                    digest,
                },
                interface,
            });
        }
        validation
            .inventory
            .reserve::<(crate::artifact_inventory::ArtifactId, usize)>(interfaces.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        validation
            .inventory
            .reserve::<(u64, usize)>(interfaces.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        ProducedValueTypeInterfaces::from_outputs(interfaces, segment)
    }

    pub(crate) fn capture_world_produced_types(
        program: &crate::compiler_world::CheckedWorldCellProgram,
        validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<ProducedValueTypeInterfaces, CompileError> {
        let mut interfaces = Vec::new();
        for item in program.items() {
            if item.binders().is_empty() {
                continue;
            }
            let ordinal = item.ordinal();
            let owner =
                tidepool_repr::SessionModule::val(tidepool_repr::Generation(item.generation()));
            let relative = std::path::PathBuf::from(format!("item-{ordinal}/value.hi"));
            let captured = CapturedValueInterfaceOutput {
                interface: world_output_bytes(program, &relative)?.to_vec(),
                packages: world_output_bytes(program, &relative.with_extension("hi.packages"))?
                    .to_vec(),
                requirements: world_output_bytes(
                    program,
                    &relative.with_extension("hi.requirements"),
                )?
                .to_vec(),
            };
            validation
                .inventory
                .reserve::<ProducedValueTypeInterface>(1)
                .and_then(|()| {
                    validation.inventory.charge(
                        captured.interface.len()
                            + captured.packages.len()
                            + captured.requirements.len(),
                    )
                })
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            let interface = captured.certify(*program.producer_sha256(), owner, None)?;
            let bytes = interface.interface_bytes_owned();
            interfaces.push(ProducedValueTypeInterface {
                ordinal,
                row: crate::declaration_context::CheckedValueRowIssuer::new(interface.clone()),
                input: ValueInterfaceBytes {
                    owner,
                    module: owner.module_name(),
                    digest: hash(&bytes),
                    bytes,
                    path: program.directory().path().join(relative),
                },
                interface,
            });
        }
        ProducedValueTypeInterfaces::from_outputs(interfaces, 0..program.items().len())
    }

    pub(crate) fn capture_checked(
        values: Vec<(tidepool_repr::SessionModule, Arc<[u8]>)>,
        retained: &[Arc<CheckedValueArtifact>],
    ) -> Result<Arc<Self>, CompileError> {
        let mut originals = BTreeMap::new();
        for artifact in retained {
            if originals
                .insert(artifact.owner().module_name(), artifact.clone())
                .is_some()
            {
                return Err(failure("duplicate retained value certificate"));
            }
        }
        let directory = Arc::new(ValueInterfaceDirectory(
            tempfile::Builder::new()
                .prefix("tidepool-checked-values-")
                .tempdir()?,
        ));
        let mut baseline = Vec::with_capacity(values.len());
        let initial_bytes = 0;
        let mut unshared_comparison_bytes = 0u64;
        for (owner, bytes) in values {
            let original = originals
                .remove(&owner.module_name())
                .ok_or_else(|| failure("checked input lacks its original value certificate"))?;
            if !Arc::ptr_eq(&bytes, original.bytes_owned()) {
                unshared_comparison_bytes += bytes.len() as u64;
                if bytes.as_ref() != original.bytes_owned().as_ref() {
                    return Err(failure(
                        "retained value certificate differs from selected interface bytes",
                    ));
                }
            }
            let input = original.interface.clone();
            baseline.push(CapturedValueInterface::Certified { input, original });
        }
        if !originals.is_empty() {
            return Err(failure(
                "retained value certificates exceed selected input inventory",
            ));
        }
        tracing::debug!(target: "tidepool_toolchain::artifacts", phase = "checked_baseline_capture",
            selected_references = baseline.len(), unshared_comparison_bytes, baseline_files_written = 0,
            baseline_bytes_written = 0, baseline_reauthentication_bytes = 0,
            "borrowed exact checked-value row owners");
        Ok(Arc::new(Self {
            directory,
            baseline,
            initial_bytes,
            output_files_hashed: AtomicU64::new(0),
            output_bytes_hashed: AtomicU64::new(0),
        }))
    }

    #[cfg(test)]
    pub(crate) fn capture_raw(
        values: Vec<(tidepool_repr::SessionModule, Arc<[u8]>)>,
    ) -> Result<Arc<Self>, CompileError> {
        let directory = Arc::new(ValueInterfaceDirectory(
            tempfile::Builder::new()
                .prefix("tidepool-checked-values-")
                .tempdir()?,
        ));
        let mut baseline = Vec::with_capacity(values.len());
        let mut initial_bytes = 0;
        for (owner, bytes) in values {
            let input = Self::write_input(&directory, owner, bytes)?;
            initial_bytes += input.bytes.len() as u64;
            baseline.push(CapturedValueInterface::Raw(input));
        }
        Ok(Arc::new(Self {
            directory,
            baseline,
            initial_bytes,
            output_files_hashed: AtomicU64::new(0),
            output_bytes_hashed: AtomicU64::new(0),
        }))
    }

    #[cfg(test)]
    fn write_input(
        directory: &ValueInterfaceDirectory,
        owner: tidepool_repr::SessionModule,
        bytes: Arc<[u8]>,
    ) -> Result<ValueInterfaceBytes, CompileError> {
        let path = directory.0.path().join(owner.relative_hi_path());
        std::fs::create_dir_all(path.parent().expect("generated interface parent"))?;
        std::fs::write(&path, &bytes)?;
        Ok(ValueInterfaceBytes {
            owner,
            module: owner.module_name(),
            digest: hash(&bytes),
            bytes,
            path,
        })
    }

    pub(crate) fn certified_artifacts(&self) -> Vec<Arc<CheckedValueArtifact>> {
        self.baseline
            .iter()
            .filter_map(|value| match value {
                CapturedValueInterface::Certified { original, .. } => Some(original.clone()),
                #[cfg(test)]
                CapturedValueInterface::Raw(_) => None,
            })
            .collect()
    }

    fn work(&self) -> CheckedInputWork {
        CheckedInputWork {
            initial_files_written: self
                .baseline
                .iter()
                .filter(|value| match value {
                    #[cfg(test)]
                    CapturedValueInterface::Raw(_) => true,
                    CapturedValueInterface::Certified { .. } => false,
                })
                .count() as u64,
            initial_bytes_written_and_hashed: self.initial_bytes,
            output_files_hashed: self.output_files_hashed.load(Ordering::Relaxed),
            output_bytes_hashed: self.output_bytes_hashed.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn root(&self) -> &Path {
        self.directory.0.path()
    }

    pub(crate) fn baseline_authorization(&self) -> Value {
        Value::Array(
            self.baseline
                .iter()
                .map(|artifact| artifact.input().authorization())
                .collect(),
        )
    }

    pub(crate) fn import_authority(&self) -> CheckedValueImportAuthority {
        CheckedValueImportAuthority::capture(self.baseline.iter().cloned())
    }

    pub(crate) fn retain_diagnostics(&self, destination: &Path) -> std::io::Result<()> {
        let inputs = self
            .baseline
            .iter()
            .map(|artifact| (artifact.input().module.as_str(), artifact))
            .collect::<BTreeMap<_, _>>();
        let destination = destination.join("checked-value-inputs");
        std::fs::create_dir(&destination)?;
        let mut captured = Vec::with_capacity(inputs.len());
        for value in inputs.into_values() {
            let artifact = value.input();
            let selected_transport = match value {
                CapturedValueInterface::Certified { original, .. } => {
                    let transport = original.row.checked_value_transport();
                    Some(serde_json::json!({
                        "endpoint": transport.endpoint(), "offset": transport.offset(),
                        "length": transport.len(), "arena_bytes": transport.arena_len(),
                        "sha256_bytes": transport.sha256(),
                    }))
                }
                #[cfg(test)]
                CapturedValueInterface::Raw(_) => None,
            };
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
                "selected_transport": selected_transport,
                "observed_source": "alias_file",
                "observed_sha256": observed_digest,
                "observation_error": observation_error,
            }));
        }
        std::fs::write(
            destination.join("inputs.json"),
            serde_json::to_vec(&captured).map_err(std::io::Error::other)?,
        )
    }

    #[tracing::instrument(
        target = "exomonad_harness::timing",
        name = "products.value_capture",
        level = "debug",
        skip_all,
        fields(inclusive = true, generation = generation)
    )]
    fn capture_output(
        &self,
        generation: u64,
        cell: &ExactCheckedCell,
        context: &Arc<crate::declaration_context::ExactDeclarationContext>,
        source_lexical: &[crate::declaration_join::ExactLexicalNode],
        original_execution: &Arc<crate::declaration_context::OriginalExecutionEvidence>,
        capture: CheckedValueCaptureInput<'_>,
    ) -> Result<Arc<CheckedValueArtifact>, CompileError> {
        let templates = cell.specification.template_sources();
        let template_imports = crate::declaration_context::CapturedTemplateImports::capture(
            original_execution,
            &templates,
        )?;
        self.capture_value_data(
            generation,
            cell,
            context,
            source_lexical,
            template_imports,
            capture,
        )
    }

    /// Materialize already admitted value and template support. This operation
    /// does not authenticate compiler execution or select template imports.
    fn capture_value_data(
        &self,
        generation: u64,
        cell: &ExactCheckedCell,
        context: &Arc<crate::declaration_context::ExactDeclarationContext>,
        source_lexical: &[crate::declaration_join::ExactLexicalNode],
        template_imports: Option<crate::declaration_context::CapturedTemplateImports>,
        capture: CheckedValueCaptureInput<'_>,
    ) -> Result<Arc<CheckedValueArtifact>, CompileError> {
        let mut timing = crate::timing::InvocationTiming::start_scoped(
            "products.value_capture",
            "resolve_interface",
        );
        let (produced, support) = match capture {
            CheckedValueCaptureInput::Ordinary { produced } => (produced, None),
            CheckedValueCaptureInput::Prepared { support, produced } => {
                (Some(produced), Some(support))
            }
        };
        let owner = tidepool_repr::SessionModule::val(tidepool_repr::Generation(generation));
        let (input, certificate) = match produced {
            Some(produced) => {
                let output = produced.output(owner)?;
                if output.interface.interface().toolchain_identity_sha256() != cell.producer {
                    return Err(failure("captured output has another producer"));
                }
                (output.input.clone(), output.interface.clone())
            }
            None => {
                let path = self.root().join(owner.relative_hi_path());
                let certificate = CapturedValueInterfaceOutput::capture(&path)?.certify(
                    cell.producer,
                    owner,
                    None,
                )?;
                let bytes = certificate.interface_bytes_owned();
                let digest = hash(&bytes);
                self.output_files_hashed.fetch_add(1, Ordering::Relaxed);
                self.output_bytes_hashed
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                (
                    ValueInterfaceBytes {
                        owner,
                        module: owner.module_name(),
                        bytes,
                        path,
                        digest,
                    },
                    certificate,
                )
            }
        };
        timing.advance("capture_context");
        let context = match support {
            Some(support) => {
                if !Arc::ptr_eq(context, support.context()) {
                    return Err(failure("prepared value support has another completed cell"));
                }
                context.clone()
            }
            None => Arc::new(
                (**context)
                    .clone()
                    .extend_with_value_interfaces(std::slice::from_ref(&certificate))?,
            ),
        };
        // Promote into the completed issuer before its existing physical
        // projection chooses the value's independently retained support.
        let row =
            crate::declaration_context::RetainedArtifactMaterialization::capture_checked_value(
                context.compiler_source_support(),
                match produced {
                    Some(outputs) => crate::declaration_context::CheckedValueRowSource::Produced(
                        &outputs.output(owner)?.row,
                    ),
                    None => crate::declaration_context::CheckedValueRowSource::Fresh(&certificate),
                },
            )?;
        timing.advance("retain_output_surface");
        let crate::declaration_context::RetainedValueSourceSurface {
            compiler_support: retained_support,
            lexical: source_lexical,
        } = match support {
            Some(support) => support.retain_output(&certificate)?,
            None => crate::declaration_context::PreparedValueSourceSupport::new(
                context.clone(),
                source_lexical,
            )?
            .retain_output(&certificate)?,
        };
        timing.advance("admit_template_support");
        let (compiler_support, template_imports) = match template_imports {
            Some(captured) => {
                let (support, evidence) = captured.into_value_support(retained_support)?;
                (support, Some(evidence))
            }
            None => (retained_support, None),
        };
        timing.advance("assemble_value_artifact");
        let mut input = input;
        input.path = row.checked_value_interface().path.clone();
        let artifact = Arc::new(CheckedValueArtifact {
            interface: input,
            authority: (cell.producer, cell.receipt_digest),
            certified_interface: certificate,
            compiler_support,
            source_lexical,
            template_imports,
            row,
            original_interface_prototype: None,
        });
        timing.finish(true);
        Ok(artifact)
    }
}

impl CheckedValueArtifact {
    pub(crate) fn world_type_parts(
        &self,
    ) -> Result<
        (
            Arc<crate::artifact_inventory::ArtifactEntry>,
            Vec<crate::declaration_context::original_inputs::OriginalInputPart>,
        ),
        CompileError,
    > {
        self.row.world_type_parts()
    }

    pub(crate) fn world_issuer(&self) -> ([u8; 32], [u8; 32]) {
        self.authority
    }

    /// Type transport can share aliases only when their issuing row is the
    /// same immutable owner. Equal payloads or receipts alone do not prove that.
    pub(crate) fn same_world_type_row(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.row, &other.row)
            && self.authority == other.authority
            && self.certified_interface.artifact_id() == other.certified_interface.artifact_id()
    }

    pub(crate) fn compiler_source_support(
        &self,
    ) -> &crate::artifact_inventory::CompilerSourceSupport {
        &self.compiler_support
    }
    pub(crate) fn compiler_input_projection(
        &self,
    ) -> &crate::artifact_inventory::CompilerInputProjection {
        &self.compiler_support.projection()
    }
    pub fn owner(&self) -> tidepool_repr::SessionModule {
        self.interface.owner
    }
    pub fn bytes_owned(&self) -> &Arc<[u8]> {
        &self.interface.bytes
    }
    pub fn certified_interface(&self) -> &Arc<crate::recovery_artifacts::CertifiedValueInterface> {
        &self.certified_interface
    }
    #[cfg(test)]
    pub(crate) fn compiler_artifact_view(&self) -> &crate::artifact_inventory::ArtifactView {
        &self.compiler_support.artifacts()
    }
    pub(crate) fn source_lexical(&self) -> &[crate::declaration_join::ExactLexicalNode] {
        &self.source_lexical
    }
    pub(crate) fn template_imports(
        &self,
    ) -> Option<&Arc<crate::declaration_context::TemplateImportEvidence>> {
        self.template_imports.as_ref()
    }
}

impl ValueInterfaceBytes {
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
    CapturedValueInterfaceOutput::with_interface(path, bytes.to_vec())?
        .certify(producer, owner, None)
}

/// Capture the complete compiler output before interpreting or sealing any
/// member. Certification consumes these exact owned bytes and never rereads
/// the output paths after checking their receipt seals.
pub(crate) fn world_output_bytes<'a>(
    program: &'a crate::compiler_world::CheckedWorldCellProgram,
    relative: &Path,
) -> Result<&'a Arc<[u8]>, CompileError> {
    program
        .captured_files()
        .get(relative)
        .map(|file| file.bytes())
        .ok_or_else(|| {
            failure(format!(
                "checked World output is missing {}",
                relative.display()
            ))
        })
}

struct CapturedValueInterfaceOutput {
    interface: Vec<u8>,
    packages: Vec<u8>,
    requirements: Vec<u8>,
}

impl CapturedValueInterfaceOutput {
    fn capture(path: &Path) -> Result<Self, CompileError> {
        Self::with_interface(path, read(path, 32 << 20)?)
    }

    fn with_interface(path: &Path, interface: Vec<u8>) -> Result<Self, CompileError> {
        Ok(Self {
            interface,
            packages: read(path.with_extension("hi.packages"), 4 << 20)?,
            requirements: read(path.with_extension("hi.requirements"), 4 << 20)?,
        })
    }

    fn certify(
        self,
        producer: [u8; 32],
        owner: tidepool_repr::SessionModule,
        receipt_seals: Option<[&str; 3]>,
    ) -> Result<Arc<crate::recovery_artifacts::CertifiedValueInterface>, CompileError> {
        if receipt_seals.is_some_and(|expected| {
            expected
                .into_iter()
                .zip([
                    hash(&self.interface),
                    hash(&self.packages),
                    hash(&self.requirements),
                ])
                .any(|(expected, actual)| expected != actual)
        }) {
            return Err(failure("host interface output differs from receipt"));
        }
        let requirements = decode(&self.requirements)?;
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
                self.interface,
                self.packages,
                requirements,
            )
            .map_err(failure)?,
        ))
    }
}

#[derive(Debug)]
pub(crate) struct PlannedCheckedDeclaration {
    pub(crate) source: String,
    pub(crate) certificate: Arc<crate::declaration_join::CertifiedAuthoredDeclaration>,
    pub(crate) receipt_digest: [u8; 32],
    // Complete same-offer compiler inputs belong to this checked receipt;
    // the public authored certificate retains only publication roles.
    pub(crate) compiler_input: crate::declaration_context::OriginalCompilerInputs,
}

impl ExactCheckedCell {
    pub fn item_count(&self) -> usize {
        self.items.len()
    }
    pub fn admission_digest(&self) -> [u8; 32] {
        self.specification.admission_digest()
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
                .any(|(source, evidence)| evidence.revalidate(source).is_err())
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

/// Compiler proofs for an ordered prefix. Runtime completion retains this
/// value only after the corresponding native installation and execution.
#[derive(Clone, Debug)]
pub struct ExactCompiledPrefix {
    cell: Arc<ExactCheckedCell>,
    completed: CheckedPrefixSequence<CompletedCheckedItem>,
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
    fn validate_required<'a, 'required>(
        &self,
        required: impl IntoIterator<Item = &'required tidepool_repr::execution_schema::SymbolIdentity>,
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
        for required in required {
            let Some((name, identity, generation, identifier)) = self
                .rows
                .iter()
                .find(|(_, identity, _, _)| identity == required)
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
    RuntimeItem([u8; 32]),
    CellProgram([u8; 32]),
}

/// A checked recipe and its exact prepared target, issued together by the
/// product-sealing entry point. Compiling alone makes no execution claim.
#[derive(Debug)]
pub struct ExactCompiledItem {
    original_execution: Arc<crate::declaration_context::OriginalExecutionEvidence>,
    item: ExactCheckedItem,
    target: Arc<tidepool_repr::execution_schema::PreparedProgram>,
    table: tidepool_repr::DataConTable,
    warnings: tidepool_repr::serial::MetaWarnings,
    yield_sites_digest: [u8; 32],
    value_interface: Option<Arc<CheckedValueArtifact>>,
    generation: u64,
    bound_binders: Vec<Value>,
    observation_name: Option<String>,
    admission: CheckedExecutionAdmission,
    settled_values: CheckedSettledValues,
    typed_entry: Option<SealedTypedEntry>,
    source_originals: Vec<Arc<crate::artifacts::PublishedSourceOriginalSelection>>,
}

#[derive(Debug)]
struct SealedTypedEntry {
    entry: CheckedTypedEntry,
    native_sites: SelectedNativeSites,
    native_requirements: crate::artifact_inventory::NativeRequirements,
}

/// Sites from the checked target's exact original native dependency closure.
/// Construction stays with the original-group issuer; composition cannot add
/// observations or promote unselected available native groups.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelectedNativeSites {
    sites: BTreeMap<u64, IssuedNativeSite>,
}

#[derive(Clone, Debug)]
struct IssuedNativeSite {
    row: tidepool_repr::execution_schema::SiteRow,
    types: Arc<tidepool_repr::type_graph::TypeGraph>,
}

impl PartialEq for IssuedNativeSite {
    fn eq(&self, other: &Self) -> bool {
        // Equality preserves issuer custody identity. Cross-original semantic
        // compatibility is fallible and belongs to composition below.
        self.row == other.row && Arc::ptr_eq(&self.types, &other.types)
    }
}
impl Eq for IssuedNativeSite {}

impl IssuedNativeSite {
    fn matches(&self, other: &Self) -> Result<bool, CompileError> {
        if Arc::ptr_eq(&self.types, &other.types) && self.row == other.row {
            return Ok(true);
        }
        if self.row.origin != other.row.origin
            || self.row.ordinal != other.row.ordinal
            || self.row.delivery != other.row.delivery
            || self.row.inputs.len() != other.row.inputs.len()
        {
            return Ok(false);
        }
        let mut budget = tidepool_repr::type_graph::TypeWorkBudget::new(
            tidepool_repr::type_graph::GraphLimits::default().max_work,
        );
        for (left, right) in std::iter::once((&self.row.wire, &other.row.wire))
            .chain(self.row.inputs.iter().zip(&other.row.inputs))
        {
            if !self
                .types
                .rooted_wire_identity_eq(*left, &other.types, *right, &mut budget)
                .map_err(failure)?
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

impl SelectedNativeSites {
    pub(crate) fn insert_issued(
        &mut self,
        row: tidepool_repr::execution_schema::SiteRow,
        types: Arc<tidepool_repr::type_graph::TypeGraph>,
    ) -> Result<(), CompileError> {
        let incoming = IssuedNativeSite { row, types };
        if let Some(previous) = self.sites.get(&incoming.row.site) {
            if !previous.matches(&incoming)? {
                return Err(failure(format!(
                    "selected native site metadata conflict at {}",
                    incoming.row.site
                )));
            }
        } else {
            self.sites.insert(incoming.row.site, incoming);
        }
        Ok(())
    }

    /// Retain issued authority atomically when live programs are composed.
    pub fn merge(&mut self, other: &Self) -> Result<(), CompileError> {
        for (id, incoming) in &other.sites {
            if let Some(previous) = self.sites.get(id) {
                if !previous.matches(incoming)? {
                    return Err(failure(format!(
                        "selected native site metadata conflict at {id}"
                    )));
                }
            }
        }
        for (id, incoming) in &other.sites {
            self.sites.entry(*id).or_insert_with(|| incoming.clone());
        }
        Ok(())
    }

    pub fn ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.sites.keys().copied()
    }

    pub fn has_completion_site(&self, site: u64) -> bool {
        self.sites
            .get(&site)
            .is_some_and(|issued| issued.row.inputs.is_empty())
    }

    pub fn validate_observations<'a>(
        &self,
        sites: impl IntoIterator<Item = &'a crate::YieldSite>,
    ) -> Result<(), CompileError> {
        for observed in sites {
            if let Some(issued) = self.sites.get(&observed.site) {
                if issued.row.origin != observed.origin
                    || issued.row.ordinal != observed.ordinal
                    || issued.row.inputs.len() != observed.inputs.len()
                {
                    return Err(failure(format!(
                        "selected native site differs from observation at {}",
                        observed.site
                    )));
                }
            }
        }
        Ok(())
    }
}

/// Read-only identity of a native entry sealed against its canonical original.
#[derive(Clone, Debug)]
pub struct CheckedTypedEntry {
    target: Arc<tidepool_repr::execution_schema::PreparedProgram>,
    plan_digest: String,
    origin: tidepool_repr::execution_schema::SymbolIdentity,
    entry: tidepool_repr::execution_schema::SymbolIdentity,
    original_ordinal: u32,
    artifact: crate::artifact_inventory::ArtifactId,
}

impl PartialEq for CheckedTypedEntry {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.target, &other.target)
            && self.plan_digest == other.plan_digest
            && self.origin == other.origin
            && self.entry == other.entry
            && self.original_ordinal == other.original_ordinal
            && self.artifact == other.artifact
    }
}
impl Eq for CheckedTypedEntry {}

fn validate_item_receipt(
    fields: &[Value],
    request: &str,
    admission_digest: [u8; 32],
    receipt_digest: [u8; 32],
    index: usize,
    source: &str,
    is_program: bool,
) -> Result<(), CompileError> {
    if string(&fields[0])? != "TPEXACTITEM"
        || string(&fields[1])? != if is_program { "2" } else { "1" }
        || string(&fields[2])? != request
        || string(&fields[3])? != hex(&admission_digest)
        || string(&fields[4])?
            != hex(&if is_program {
                admission_digest
            } else {
                receipt_digest
            })
        || fields[5] != Value::Integer((index as u64).into())
        || string(&fields[6])? != hash(source.as_bytes())
        || string(&fields[7])? != "tidepool-checked-recipe-2"
    {
        return Err(failure(
            "checked-item recipe receipt differs from its same compiler offer",
        ));
    }
    Ok(())
}
fn typed_entry_failure() -> CompileError {
    CompileError::CompilerEvidence(Box::new(
        crate::certified_products::CertificationError::Mismatch("typed native entry"),
    ))
}
fn typed_entry_identity(
    value: &Value,
) -> Result<tidepool_repr::execution_schema::SymbolIdentity, CompileError> {
    let fields = row(value, 3)?;
    let unit = string(&fields[0])?;
    let module = string(&fields[1])?;
    let occurrence = string(&fields[2])?;
    if [unit, module, occurrence]
        .iter()
        .any(|value| value.is_empty() || value.len() > 65536)
    {
        return Err(failure("typed native identity is incomplete"));
    }
    Ok(tidepool_repr::execution_schema::SymbolIdentity {
        unit: unit.into(),
        module: module.into(),
        namespace: "value".into(),
        occurrence: occurrence.into(),
        record_parent: None,
    })
}
impl CheckedTypedEntry {
    fn matches_receipt(&self, proof: &[Value]) -> Result<bool, CompileError> {
        Ok(string(&proof[0])? == self.plan_digest
            && typed_entry_identity(&proof[1])? == self.origin
            && typed_entry_identity(&proof[2])? == self.entry
            && proof[3] == Value::Integer(self.original_ordinal.into()))
    }
    /// Issued once before inventory admission from the same request, normalized
    /// plan, exact source admission and authenticated original product census.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn issue_program_item(
        directory: &Path,
        request: &str,
        admission_digest: [u8; 32],
        plans: &[CheckedTypedSegmentPlan],
        index: usize,
        source: &str,
        target: &Arc<tidepool_repr::execution_schema::PreparedProgram>,
        owner: &crate::declaration_join::ExactModuleIdentity,
        products: &[crate::recovery_artifacts::CertifiedRecoveryProduct],
        producer: [u8; 32],
    ) -> Result<Self, CompileError> {
        let receipt = decode(&read(directory.join("checked-item.cbor"), 4 << 20)?)?;
        let fields = row(&receipt, 9)?;
        validate_item_receipt(
            fields,
            request,
            admission_digest,
            admission_digest,
            index,
            source,
            true,
        )?;
        let (plan, item) = plans
            .iter()
            .find_map(|plan| {
                plan.items
                    .iter()
                    .find(|item| item.ordinal == index)
                    .map(|item| (plan, item))
            })
            .ok_or_else(|| failure("native item lacks its normalized typed segment"))?;
        let proof = row(&fields[8], 4)?;
        let origin = typed_entry_identity(&proof[1])?;
        let entry = typed_entry_identity(&proof[2])?;
        let original_ordinal = match proof[3] {
            Value::Integer(value) => u32::try_from(value).map_err(failure)?,
            _ => return Err(failure("typed native original ordinal is not integer")),
        };
        if string(&proof[0])? != plan.digest
            || origin.occurrence != plan.root
            || entry.occurrence != item.entry
        {
            return Err(typed_entry_failure());
        }
        Self::from_original_entry(
            plan.digest.clone(),
            origin,
            entry,
            original_ordinal,
            target,
            owner,
            products,
            producer,
        )
    }

    pub(crate) fn issue_world_item(
        program: &crate::compiler_world::CheckedWorldCellProgram,
        ordinal: usize,
        target: &Arc<tidepool_repr::execution_schema::PreparedProgram>,
        owner: &crate::declaration_join::ExactModuleIdentity,
        products: &[crate::recovery_artifacts::CertifiedRecoveryProduct],
    ) -> Result<Self, CompileError> {
        let item = program.item(ordinal).ok_or_else(typed_entry_failure)?;
        let executable = item.executable().ok_or_else(typed_entry_failure)?;
        let plan = program
            .segments()
            .iter()
            .find(|plan| plan.digest() == executable.segment_digest())
            .ok_or_else(typed_entry_failure)?;
        let planned = plan
            .items()
            .iter()
            .find(|planned| planned.ordinal() == ordinal)
            .ok_or_else(typed_entry_failure)?;
        if planned.generation() != item.generation() || planned.entry() != executable.entry() {
            return Err(typed_entry_failure());
        }
        Self::from_original_entry(
            plan.digest().to_owned(),
            tidepool_repr::execution_schema::SymbolIdentity {
                unit: owner.unit.clone(),
                module: owner.module.clone(),
                namespace: "value".into(),
                occurrence: plan.root().to_owned(),
                record_parent: None,
            },
            executable.symbol().clone(),
            executable.native_ordinal(),
            target,
            owner,
            products,
            *program.producer_sha256(),
        )
    }

    // Native membership is independent of the physical receipt format. The
    // owning operation must authenticate its plan before reaching this check.
    // A receipt ordinal or SymbolIdentity alone cannot issue a typed entry.
    #[allow(clippy::too_many_arguments)]
    fn from_original_entry(
        plan_digest: String,
        origin: tidepool_repr::execution_schema::SymbolIdentity,
        entry: tidepool_repr::execution_schema::SymbolIdentity,
        original_ordinal: u32,
        target: &Arc<tidepool_repr::execution_schema::PreparedProgram>,
        owner: &crate::declaration_join::ExactModuleIdentity,
        products: &[crate::recovery_artifacts::CertifiedRecoveryProduct],
        producer: [u8; 32],
    ) -> Result<Self, CompileError> {
        use tidepool_repr::execution_schema::Group;
        let target_entry = target
            .bindings()
            .iter()
            .flat_map(|group| match group {
                Group::NonRecursive(binding) => std::slice::from_ref(binding),
                Group::Recursive(bindings) => bindings.as_slice(),
            })
            .find(|binding| binding.binding.id == target.entry());
        let originals = products
            .iter()
            .filter(|product| {
                crate::certified_products::authenticates_original_native_entry(
                    product,
                    original_ordinal,
                    &entry,
                )
            })
            .collect::<Vec<_>>();
        if origin.unit != owner.unit
            || origin.module != owner.module
            || entry.unit != origin.unit
            || entry.module != origin.module
            || target_entry.is_none_or(|binding| binding.identity != entry)
            || originals.len() != 1
        {
            return Err(typed_entry_failure());
        }
        Ok(Self {
            target: Arc::clone(target),
            plan_digest,
            origin,
            entry,
            original_ordinal,
            artifact: crate::artifact_inventory::ArtifactEntry::original_artifact_id(
                producer,
                originals[0],
            ),
        })
    }

    pub(crate) fn belongs_to_target(
        &self,
        target: &Arc<tidepool_repr::execution_schema::PreparedProgram>,
    ) -> bool {
        Arc::ptr_eq(&self.target, target)
    }

    pub fn plan_digest(&self) -> &str {
        &self.plan_digest
    }
    pub fn origin(&self) -> &tidepool_repr::execution_schema::SymbolIdentity {
        &self.origin
    }
    pub fn entry(&self) -> &tidepool_repr::execution_schema::SymbolIdentity {
        &self.entry
    }
    pub fn original_ordinal(&self) -> u32 {
        self.original_ordinal
    }
    pub(crate) fn native_group_key(&self) -> crate::artifact_inventory::NativeGroupKey {
        crate::artifact_inventory::NativeGroupKey {
            artifact: self.artifact,
            original_ordinal: self.original_ordinal,
        }
    }
    pub fn native_requirement_root(&self) -> crate::artifact_inventory::NativeRequirementRoot {
        crate::artifact_inventory::NativeRequirementRoot::Group {
            artifact: self.artifact,
            original_ordinal: self.original_ordinal,
        }
    }
}

impl ExactCompiledItem {
    pub fn table(&self) -> &tidepool_repr::DataConTable {
        &self.table
    }
    pub fn warnings(&self) -> &tidepool_repr::serial::MetaWarnings {
        &self.warnings
    }

    pub fn typed_entry(&self) -> Option<&CheckedTypedEntry> {
        self.typed_entry.as_ref().map(|issued| &issued.entry)
    }

    /// Existing publications selected by this target's authenticated lexical
    /// imports or exact native entry, independently of its yield-site count.
    pub fn selected_source_originals(
        &self,
    ) -> &[Arc<crate::artifacts::PublishedSourceOriginalSelection>] {
        &self.source_originals
    }

    /// Exact native-site selection sealed with this typed target. The original
    /// execution inventory may also retain earlier entries' installed groups.
    pub fn selected_native_sites(&self) -> Option<&SelectedNativeSites> {
        self.typed_entry.as_ref().map(|issued| &issued.native_sites)
    }

    /// Execution evidence is available only for this exact validated output.
    pub fn original_execution_evidence(
        &self,
        target: &tidepool_repr::execution_schema::PreparedProgram,
        table: &tidepool_repr::DataConTable,
        sites: &[crate::YieldSite],
    ) -> Result<Arc<crate::declaration_context::OriginalExecutionEvidence>, CompileError> {
        self.original_source(target, table, sites)?;
        Ok(Arc::clone(&self.original_execution))
    }

    /// Observe this output's retained facts without instance-completion authority.
    pub fn original_execution_context(
        &self,
        target: &tidepool_repr::execution_schema::PreparedProgram,
        table: &tidepool_repr::DataConTable,
        sites: &[crate::YieldSite],
    ) -> Result<Arc<crate::declaration_context::ExactDeclarationContext>, CompileError> {
        Ok(Arc::clone(
            self.original_execution_evidence(target, table, sites)?
                .context(),
        ))
    }

    /// Validate the complete output before lending its requested type source.
    pub fn original_source(
        &self,
        target: &tidepool_repr::execution_schema::PreparedProgram,
        table: &tidepool_repr::DataConTable,
        sites: &[crate::YieldSite],
    ) -> Result<crate::declaration_context::ValidatedOriginalSource<'_>, CompileError> {
        if !self.matches_target(target) {
            return Err(failure(
                "original source belongs to another prepared target",
            ));
        }
        self.validate_table(table)?;
        self.validate_yield_sites(sites)?;
        Ok(
            crate::declaration_context::ValidatedOriginalSource::inputs_from(
                &self.original_execution,
            ),
        )
    }
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
            CheckedExecutionAdmission::RuntimeItem(digest) => digest == item_digest,
            CheckedExecutionAdmission::CellProgram(digest) => digest == cell_digest,
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
        if let Some(proof) = &self.typed_entry {
            let requirements = &proof.native_requirements;
            self.settled_values.validate_required(
                requirements
                    .bindings
                    .iter()
                    .map(|requirement| &requirement.identity)
                    .chain(self.target.globals().iter().map(|global| &global.identity)),
                actual,
            )
        } else {
            self.settled_values.validate(actual)
        }
    }
    /// Borrow the exact captured value imports demanded by this
    /// issued native entry. Observing demand does not grant runtime leases.
    pub fn required_native_imports(
        &self,
    ) -> impl Iterator<Item = (&tidepool_repr::execution_schema::SymbolIdentity, u64)> {
        self.typed_entry.iter().flat_map(|proof| {
            proof
                .native_requirements
                .bindings
                .iter()
                .map(|requirement| (&requirement.identity, requirement.generation))
        })
    }
    /// Validate the selected original group against the admitted baseline and
    /// completed native imports. Same-cell field identities are checked
    /// separately by `validate_settled_native_bindings`.
    pub fn validate_required_native_imports<'a>(
        &self,
        actual: impl IntoIterator<Item = (&'a tidepool_repr::execution_schema::SymbolIdentity, u64)>,
    ) -> Result<(), CompileError> {
        let actual = actual.into_iter().collect::<BTreeSet<_>>();
        for (identity, generation) in self.required_native_imports() {
            if !actual.contains(&(identity, generation)) {
                return Err(CompileError::CompilerEvidence(Box::new(
                    crate::certified_products::CertificationError::Mismatch("typed native import"),
                )));
            }
        }
        Ok(())
    }
    pub fn target_definition_identities(
        &self,
    ) -> impl Iterator<Item = &tidepool_repr::execution_schema::SymbolIdentity> {
        target_definition_identities(&self.target)
    }
    pub fn bound_binder_identities(&self) -> impl Iterator<Item = (&str, u64)> {
        bound_binder_identities(&self.bound_binders)
    }
    pub(crate) fn checked_bound_binders(&self) -> impl Iterator<Item = &Value> {
        self.bound_binders.iter()
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
        self.value_interface.as_ref().map(|artifact| {
            (
                artifact.interface.module.as_str(),
                artifact.interface.bytes.as_ref(),
            )
        })
    }
    pub fn value_interface_owned(&self) -> Option<(&str, &Arc<[u8]>)> {
        self.value_interface.as_ref().map(|artifact| {
            (
                artifact.interface.module.as_str(),
                &artifact.interface.bytes,
            )
        })
    }
    pub fn value_interface_certificate(&self) -> Option<Arc<CheckedValueArtifact>> {
        self.value_interface.clone()
    }
}

impl ExactCompiledPrefix {
    /// During same-request sealing, original declaration authority belongs to
    /// its checked item. It cannot authorize later source compilation.
    pub(crate) fn append_program_original(
        &self,
        item: ExactCheckedItem,
    ) -> Result<Self, CompileError> {
        if item.index != self.next_item()
            || !Arc::ptr_eq(&item.cell, &self.cell)
            || item.planned_declaration().is_none()
        {
            return Err(failure(
                "program original is not the next checked declaration",
            ));
        }
        let mut next = self.clone();
        next.completed.push(CompletedCheckedItem::Declaration(item));
        Ok(next)
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
    /// Validate the compiler-issued cumulative interface before advancing the
    /// original native declaration's completed prefix.
    pub fn append_declaration_with_projection(
        &self,
        item: ExactCheckedItem,
        publication: &crate::declaration_join::PublicationComposition,
    ) -> Result<Self, CompileError> {
        let projection = publication.join();
        if item.index != self.next_item() || !Arc::ptr_eq(&item.cell, &self.cell) {
            return Err(failure(
                "declaration is not the next original certified item of this cell",
            ));
        }
        let certificate = item
            .planned_declaration()
            .ok_or_else(|| failure("completed declaration has no original certificate"))?;
        let original = certificate.product();
        if projection.toolchain_identity_sha256() != certificate.toolchain_identity_sha256()
            || projection
                .input()
                .private_tip
                .as_ref()
                .is_none_or(|tip| tip.module != original.owner().module)
            || !projection
                .recovery_products()
                .iter()
                .any(|product| product == original)
        {
            return Err(failure(
                "declaration projection has another original implementation",
            ));
        }
        let retained_projections = &self.cell.retained_projections;
        let initial = &self.cell.declaration_context;
        let initial_lexical = initial
            .lexical_graph()
            .iter()
            .map(|node| (node.owner.clone(), node))
            .collect::<BTreeMap<_, _>>();
        let initial_interfaces = initial.joined_interfaces();
        let mut selected = BTreeSet::new();
        for retained in retained_projections {
            let owner = crate::declaration_join::ExactModuleIdentity {
                unit: retained.reserved().unit.clone(),
                module: retained.reserved().module.clone(),
            };
            if retained.toolchain_identity_sha256() != self.cell.producer
                || !initial_lexical.contains_key(&owner)
                || !initial_interfaces
                    .iter()
                    .any(|interface| interface == retained.interface())
            {
                return Err(failure(
                    "retained projection was not selected by this checked cell",
                ));
            }
            let mut pending = vec![owner];
            while let Some(owner) = pending.pop() {
                if selected.insert(owner.clone()) {
                    let node = initial_lexical.get(&owner).ok_or_else(|| {
                        failure("retained projection leaves the checked cell lexical graph")
                    })?;
                    pending.extend(node.imports.iter().cloned());
                }
            }
        }
        // Each facade arrives paired with the exact selected context issued
        // for it. Retained membranes keep their authenticated initial owner.
        initial.as_ref().clone().admit_publication(publication)?;
        let mut next = self.clone();
        next.completed.push(CompletedCheckedItem::Declaration(item));
        Ok(next)
    }
    pub fn injected_modules(&self) -> Vec<String> {
        self.cell
            .specification
            .injected_modules()
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
                            .map(|artifact| artifact.interface.module.clone())
                    }),
            )
            .collect()
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
                item.value_interface
                    .as_ref()
                    .map(|artifact| (item.generation, artifact, item.bound_binders.as_slice()))
            });
        for (generation, artifact, binders) in native {
            for binder in binders {
                let fields = row(binder, 7)?;
                let name = string(&fields[0])?;
                let id = match &fields[1] {
                    Value::Integer(id) => u64::try_from(*id)
                        .map_err(|_| failure("sealed binder identity is not unsigned"))?,
                    _ => return Err(failure("sealed binder identity is not an integer")),
                };
                if sealed
                    .insert(
                        (artifact.interface.module.as_str(), name),
                        (generation, id, artifact),
                    )
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
                .entry(&artifact.interface.module)
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
                    text(artifact.interface.path.to_string_lossy()),
                    text(&artifact.interface.digest),
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
    pub fn world_selected_runtime_inputs(
        &self,
    ) -> Option<crate::compiler_world::WorldCellRuntimeInputs<'_>> {
        match &self.cell.specification {
            CheckedCellSpecification::World { program, .. } => Some(
                program
                    .item(self.index)
                    .expect("exact checked World item belongs to its program")
                    .selected_runtime_inputs(),
            ),
            CheckedCellSpecification::Finite(_) => None,
        }
    }
    pub fn specification_digest(&self) -> [u8; 32] {
        self.cell.specification.specification_digest()
    }
    pub fn reserved_declaration_modules(&self) -> &[String] {
        self.cell.specification.reserved_declaration_modules()
    }
    pub fn injected_modules(&self) -> &[String] {
        self.cell.specification.injected_modules()
    }
    pub fn include_paths(&self) -> &[std::path::PathBuf] {
        &self.cell.include
    }
    pub fn input_work(&self) -> CheckedInputWork {
        self.cell.value_inputs.work()
    }
    pub fn baseline_value_interfaces(
        &self,
    ) -> impl Iterator<Item = (&tidepool_repr::SessionModule, &Arc<[u8]>)> {
        self.cell
            .value_inputs
            .baseline
            .iter()
            .map(|artifact| (&artifact.input().owner, &artifact.input().bytes))
    }
    /// Exact row owners retained by this checked compilation's demanded inputs.
    pub fn baseline_checked_value_interfaces(
        &self,
    ) -> impl Iterator<Item = &Arc<CheckedValueArtifact>> {
        self.cell
            .value_inputs
            .baseline
            .iter()
            .filter_map(|value| match value {
                CapturedValueInterface::Certified { original, .. } => Some(original),
                #[cfg(test)]
                CapturedValueInterface::Raw(_) => None,
            })
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
        self.cell.specification.turn_templates()
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
        Ok(Some(decode_expression_lift(expression)?))
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
        })
    }
    /// Compare editable presentation with the admitted source/verdict and the
    /// complete sealed cell inventories. Keys come from the compiler receipt;
    /// planned capture signatures do not imply legacy checking pins.
    pub fn validate_observations(
        &self,
        source: &str,
        kind: CheckedItemKind,
        binders: &[String],
        pins: &[Value],
        expressions: &[Value],
    ) -> Result<(), CompileError> {
        fn matches_inventory(
            observed: &[Value],
            expected: &[&Value],
            width: usize,
        ) -> Result<bool, CompileError> {
            if observed.len() != expected.len() {
                return Ok(false);
            }
            let mut by_key = BTreeMap::new();
            for value in observed {
                let key = string(&row(value, width)?[0])?;
                if by_key.insert(key, value).is_some() {
                    return Ok(false);
                }
            }
            for value in expected {
                let key = string(&row(value, width)?[0])?;
                if by_key.remove(key) != Some(*value) {
                    return Ok(false);
                }
            }
            Ok(by_key.is_empty())
        }
        let expected = &self.cell.items[self.index];
        let expected_pins = self
            .cell
            .items
            .iter()
            .flat_map(|item| &item.pins)
            .collect::<Vec<_>>();
        let expected_expressions = self
            .cell
            .items
            .iter()
            .filter_map(|item| item.expression.as_ref())
            .collect::<Vec<_>>();
        if source != expected.source
            || kind != expected.kind
            || binders != expected.binders
            || !matches_inventory(pins, &expected_pins, 3)?
            || !matches_inventory(expressions, &expected_expressions, 4)?
        {
            return Err(failure(
                "checked item body, verdict, pins or expression plan was edited",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CheckedItemOffer {
    pub(crate) item: ExactCheckedItem,
    pub(crate) prefix: ExactCompiledPrefix,
    pub(crate) runtime_prefix_digest: [u8; 32],
    pub(crate) generation: u64,
    pub(crate) observation_name: Option<String>,
    pub(crate) is_program: bool,
    pub(crate) settled_values: CheckedSettledValues,
}

impl CheckedItemOffer {
    #[tracing::instrument(
        target = "exomonad_harness::timing", name = "products.checked_item.seal",
        level = "debug", skip_all, fields(inclusive = true, generation = self.generation, has_interface = output.has_interface)
    )]
    pub(crate) fn seal_prepared(
        &self,
        output: PreparedCheckedItemOutput,
        artifact_context: &Arc<crate::declaration_context::ExactDeclarationContext>,
        source_lexical: &[crate::declaration_join::ExactLexicalNode],
        capture: CheckedValueCaptureInput<'_>,
    ) -> Result<Arc<ExactCompiledItem>, CompileError> {
        let mut timing = crate::timing::InvocationTiming::start_scoped(
            "products.checked_item.seal",
            "validate_offer",
        );
        if !Arc::ptr_eq(&output.item.cell, &self.item.cell)
            || output.item.index != self.item.index
            || output.generation != self.generation
            || output.observation_name != self.observation_name
            || output.is_program != self.is_program
        {
            return Err(failure("prepared item differs from its checked offer"));
        }
        let PreparedCheckedItemOutput {
            original_execution,
            target,
            table,
            warnings,
            bound_binders,
            authenticated_sites,
            typed_entry,
            source_originals,
            has_interface,
            ..
        } = output;
        timing.advance("capture_value");
        let value_interface = if has_interface {
            Some(self.item.cell.value_inputs.capture_output(
                self.generation,
                &self.item.cell,
                artifact_context,
                source_lexical,
                &original_execution,
                capture,
            )?)
        } else {
            None
        };
        timing.advance("assemble_item");
        let item = Arc::new(ExactCompiledItem {
            original_execution,
            item: self.item.clone(),
            target,
            table,
            warnings,
            yield_sites_digest: authenticated_sites,
            value_interface,
            generation: self.generation,
            bound_binders,
            observation_name: self.observation_name.clone(),
            admission: if self.is_program {
                CheckedExecutionAdmission::CellProgram(self.item.admission_digest())
            } else {
                CheckedExecutionAdmission::RuntimeItem(self.runtime_prefix_digest)
            },
            settled_values: self.settled_values.clone(),
            typed_entry,
            source_originals,
        });
        timing.finish(true);
        Ok(item)
    }
}

/// Receipt, binder and target observations retained before compiler close.
/// Contextual value/source retention is issued only by the sealing phase.
#[derive(Debug)]
pub(crate) struct PreparedCheckedItemOutput {
    item: ExactCheckedItem,
    generation: u64,
    observation_name: Option<String>,
    is_program: bool,
    original_execution: Arc<crate::declaration_context::OriginalExecutionEvidence>,
    target: Arc<tidepool_repr::execution_schema::PreparedProgram>,
    table: tidepool_repr::DataConTable,
    warnings: tidepool_repr::serial::MetaWarnings,
    bound_binders: Vec<Value>,
    authenticated_sites: [u8; 32],
    typed_entry: Option<SealedTypedEntry>,
    source_originals: Vec<Arc<crate::artifacts::PublishedSourceOriginalSelection>>,
    has_interface: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum PreparedItemOrigin<'a> {
    Finite(&'a str),
    World(&'a crate::compiler_world::CheckedWorldCellProgram),
}

impl PreparedCheckedItemOutput {
    #[allow(clippy::too_many_arguments)]
    #[tracing::instrument(
        target = "exomonad_harness::timing", name = "products.checked_item.capture",
        level = "debug", skip_all,
        fields(inclusive = true, ordinal = item.index, generation, is_program)
    )]
    pub(crate) fn capture(
        item: &ExactCheckedItem,
        generation: u64,
        observation_name: Option<&str>,
        is_program: bool,
        output: &crate::artifacts::ProgramNativeOutput,
        origin: PreparedItemOrigin<'_>,
    ) -> Result<Self, CompileError> {
        let mut timing = crate::timing::InvocationTiming::start_scoped(
            "products.checked_item.capture",
            "receipt_read_decode",
        );
        let sealed = output.sealed_products()?;
        let original_execution = Arc::clone(sealed.original_execution()?);
        let program_entry = sealed.typed_entry();
        let root = output.directory();
        let source = output.source();
        let target = output.target();
        let receipt = match origin {
            PreparedItemOrigin::Finite(request) => {
                let receipt = decode(&read(root.join("checked-item.cbor"), 4 * 1024 * 1024)?)?;
                let fields = row(&receipt, if is_program { 9 } else { 8 })?;
                timing.advance("validate_item_receipt");
                validate_item_receipt(
                    fields,
                    request,
                    item.admission_digest(),
                    item.cell.receipt_digest,
                    item.index,
                    source,
                    is_program,
                )?;
                Some(receipt)
            }
            PreparedItemOrigin::World(program) => {
                if !is_program
                    || !matches!(&item.cell.specification,
                    CheckedCellSpecification::World { program: owner, .. }
                        if std::ptr::eq(owner.as_ref(), program))
                {
                    return Err(failure("World item differs from its captured output owner"));
                }
                None
            }
        };
        timing.advance("validate_typed_entry");
        let item_index = item.index;
        let typed_entry = if is_program {
            let issued =
                program_entry.ok_or_else(|| failure("typed native entry was not admitted"))?;
            let (plan, item) = item
                .cell
                .typed_segments
                .iter()
                .find_map(|plan| {
                    plan.items
                        .iter()
                        .find(|item| item.ordinal == item_index)
                        .map(|item| (plan, item))
                })
                .ok_or_else(|| failure("native item lacks its normalized typed segment"))?;
            let proof = receipt
                .as_ref()
                .map(|receipt| row(row(receipt, 9)?.get(8).ok_or_else(typed_entry_failure)?, 4))
                .transpose()?;
            // The entry issuer already authenticated its original owner and
            // native membership. Consume that exact target proof; a complete
            // instance environment is required for display, not item execution.
            if !issued.belongs_to_target(target)
                || issued.plan_digest != plan.digest
                || issued.origin.occurrence != plan.root
                || issued.entry.occurrence != item.entry
                || match proof {
                    Some(proof) => !issued.matches_receipt(proof)?,
                    None => false,
                }
            {
                return Err(typed_entry_failure());
            }
            Some(issued.clone())
        } else {
            if program_entry.is_some() {
                return Err(failure("ordinary item was given typed program authority"));
            }
            None
        };
        timing.advance("turn_read_decode");
        let turn = decode(output.turn_bytes())?;
        let turn = row(&turn, 2)?;
        timing.advance("binder_and_site_observations");
        let expected_binders = if let Some(observation) = observation_name {
            vec![observation.to_owned()]
        } else {
            item.binders().to_vec()
        };
        let (bound_binders, authenticated_sites) = match (item.kind(), string(&turn[0])?) {
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
                if let PreparedItemOrigin::World(program) = origin {
                    let expected = program
                        .reservation()
                        .expected_bindings()
                        .get(item.index)
                        .ok_or_else(|| failure("World turn has no reserved binding identities"))?;
                    let bound = decode_world_item_binders(&fields[2], expected)?;
                    if bound.as_slice()
                        != program
                            .item(item.index)
                            .ok_or_else(typed_entry_failure)?
                            .binders()
                    {
                        return Err(failure("World turn differs from its original binder proof"));
                    }
                }
                let bound = list(&fields[2], 65536)?.to_vec();
                if bound.len() != expected_binders.len() {
                    return Err(failure("compiled binder inventory differs"));
                }
                for (value, binder) in bound.iter().zip(&expected_binders) {
                    let fields = row(value, 7)?;
                    if string(&fields[0])? != binder
                        || string(&fields[2])?
                            != tidepool_repr::SessionModule::val(tidepool_repr::Generation(
                                generation,
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
        timing.advance("resolve_target_and_original_custody");
        let resolved = sealed.resolve_native_execution()?;
        let groups = resolved.groups();
        timing.advance("select_source_publications");
        let source_originals = original_execution
            .context()
            .published_source_original_selections_for_target(groups, resolved.imports())?;
        timing.advance("select_native_sites");
        let typed_entry = typed_entry
            .map(|entry| {
                let native_sites = original_execution
                    .context()
                    .artifact_view()
                    .native_sites_for_groups(&groups)?;
                timing.advance("entry_native_requirements");
                let native_requirements = resolved.requirements_for_entry()?;
                Ok::<_, CompileError>(SealedTypedEntry {
                    entry,
                    native_sites,
                    native_requirements,
                })
            })
            .transpose()?;
        timing.advance("decode_metadata");
        let (table, warnings) = output.decode_metadata()?;
        timing.advance("assemble_prepared_item");
        let prepared = Self {
            item: item.clone(),
            generation,
            observation_name: observation_name.map(str::to_owned),
            is_program,
            target: target.clone(),
            table,
            warnings,
            original_execution,
            bound_binders,
            authenticated_sites,
            typed_entry,
            source_originals,
            has_interface: !expected_binders.is_empty(),
        };
        timing.finish(true);
        Ok(prepared)
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

pub(crate) fn read_table(
    root: &Path,
    target: &tidepool_repr::execution_schema::PreparedProgram,
) -> Result<tidepool_repr::DataConTable, CompileError> {
    let (table, _) = tidepool_repr::serial::read_metadata_for_program(
        &read(root.join("meta.cbor"), 32 * 1024 * 1024)?,
        target,
    )?;
    Ok(table)
}

pub(crate) fn encode_signature(signature: &ExactCheckedSignature) -> Value {
    array([
        text("TPCHECKEDSIGNATURE2"),
        text(&signature.key),
        text(&signature.presentation),
        Value::Bytes(signature.iface.to_vec()),
        Value::Array(
            signature
                .names
                .iter()
                .map(|name| {
                    array([
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

/// Independent captured projections are admitted with the initial checked
/// offer. A continuation cannot relabel its replaced parent as such an input.
pub(crate) fn retain_projection_inputs(
    context: &crate::declaration_context::ExactDeclarationContext,
    projections: &[Arc<crate::declaration_join::AcceptedJoin>],
) -> Result<Vec<Arc<crate::declaration_join::AcceptedJoin>>, CompileError> {
    let interfaces = context.joined_interfaces();
    let selected = context
        .lexical_graph()
        .iter()
        .map(|node| &node.owner)
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    let selected_originals = context
        .compiler_input_projection()
        .snapshot_roles()
        .iter()
        .filter(|(_, role)| role.original().is_some())
        .map(|(owner, _)| owner.clone())
        .collect();
    for projection in projections {
        let owner = crate::declaration_join::ExactModuleIdentity {
            unit: projection.reserved().unit.clone(),
            module: projection.reserved().module.clone(),
        };
        if !seen.insert(owner.clone())
            || !selected.contains(&owner)
            || !interfaces
                .iter()
                .any(|interface| interface == projection.interface())
        {
            return Err(failure(
                "captured projection lacks one exact selected initial interface",
            ));
        }
        if context.toolchain_identity_sha256() != projection.toolchain_identity_sha256() {
            return Err(failure(
                "captured projection belongs to another compiler producer",
            ));
        }
        // Check the owning merge policy, including published-source policy and
        // exact native decisions. Retaining a projection does not compose its
        // SOURCE scope or change the receiving context's lexical permissions.
        let incoming = projection.publication_compiler_support(&selected_originals)?;
        context.compiler_source_support().merge(&incoming)?;
    }
    Ok(projections.to_vec())
}

fn validate_cell_receipt_header(
    header: &[Value],
    output: &[Value],
    observations: &[u8],
    request_digest: &str,
    specification: &CheckedCellRequest,
    program: Option<&CheckedPlannedCellSpecification>,
) -> Result<(), CompileError> {
    let checked_source = string(&output[2])?;
    if string(&header[0])?
        != if program.is_some() {
            "TPEXACTPROGRAM"
        } else {
            "TPEXACTCHECK"
        }
        || string(&header[1])? != if program.is_some() { "4" } else { "3" }
        || string(&header[2])? != request_digest
        || string(&header[3])? != hex(&specification.admission_digest())
        || string(&header[4])? != hash(specification.recipe().cell_source().as_bytes())
        || string(&header[5])? != hash(specification.recipe().template_source().as_bytes())
        || string(&header[6])? != hash(observations)
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
    }
    Ok(())
}
pub(crate) fn read_program_typed_segments(
    root: &Path,
    request: &str,
    specification: &CheckedCellRequest,
    program: &CheckedPlannedCellSpecification,
) -> Result<Vec<CheckedTypedSegmentPlan>, CompileError> {
    let receipt = decode(&read(root.join("checked-cell.cbor"), 8 << 20)?)?;
    let header = row(&receipt, 12)?;
    let observations = read(root.join("cell.cbor"), 32 << 20)?;
    let decoded = decode(&observations)?;
    let output = cell_observations(&decoded)?;
    validate_cell_receipt_header(
        header,
        output,
        &observations,
        request,
        specification,
        Some(program),
    )?;
    decode_typed_segment_plans(&header[11], program)
}
/// Normalize the cell observations once at the checked-item owner. Both compiler
/// routes consume this representation; this does not grant executable authority.
fn decode_checked_items<'a>(
    output: &[Value],
    typed_segments: impl Clone + Iterator<Item = &'a CheckedTypedSegmentPlan>,
    signatures: impl Clone + Iterator<Item = &'a ExactCheckedSignature>,
    ordered: bool,
) -> Result<Vec<CheckedItem>, CompileError> {
    let mut signature_keys = BTreeSet::new();
    if signatures
        .clone()
        .any(|signature| !signature_keys.insert(&signature.key))
    {
        return Err(if ordered {
            typed_metadata_failure()
        } else {
            failure("duplicate signature authority")
        });
    }
    let pins = list(&output[1], 65536)?;
    let expressions = list(&output[4], 65536)?;
    if ordered && !pins.is_empty() {
        return Err(typed_metadata_failure());
    }
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
            let typed_item = if ordered && kind != CheckedItemKind::Declaration {
                Some(
                    typed_segments
                        .clone()
                        .flat_map(|plan| &plan.items)
                        .find(|item| item.ordinal == index)
                        .ok_or_else(typed_metadata_failure)?,
                )
            } else {
                None
            };
            let item_pins = if !ordered && kind == CheckedItemKind::Bind {
                binders
                    .iter()
                    .map(|binder| {
                        unique_key(pins, &format!("__tidepool_cell_pin_{index}_{binder}"), 3)
                    })
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                Vec::new()
            };
            let expression = if kind == CheckedItemKind::Expression {
                let key = typed_item.map_or_else(
                    || format!("__tidepool_cell_expr_{index}"),
                    |item| item.entry.clone(),
                );
                Some(unique_key(expressions, &key, 4).map_err(|error| {
                    if ordered {
                        typed_metadata_failure()
                    } else {
                        error
                    }
                })?)
            } else {
                None
            };
            let keys = match typed_item {
                Some(item) => {
                    if let Some(expression) = &expression {
                        decode_expression_lift(expression).map_err(|_| typed_metadata_failure())?;
                    }
                    match &item.body {
                        CheckedTypedSegmentBody::Action { captures, .. }
                        | CheckedTypedSegmentBody::Let { captures, .. } => captures
                            .iter()
                            .map(|capture| typed_capture_signature_key(&item.entry, capture))
                            .collect(),
                        CheckedTypedSegmentBody::Observation { capture, .. } => {
                            vec![typed_capture_signature_key(&item.entry, capture)]
                        }
                    }
                }
                None => {
                    let mut keys = item_pins
                        .iter()
                        .map(|value| {
                            row(value, 3)
                                .and_then(|row| string(&row[0]))
                                .map(str::to_owned)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    if let Some(expression) = &expression {
                        keys.push(string(&row(expression, 4)?[0])?.to_owned());
                    }
                    keys
                }
            };
            let item_signatures = keys
                .iter()
                .map(|key| {
                    signatures
                        .clone()
                        .find(|signature| &signature.key == key)
                        .cloned()
                        .ok_or_else(|| {
                            if ordered {
                                typed_metadata_failure()
                            } else {
                                failure("checked item lacks complete signature Name authority")
                            }
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
    if used.len() != signatures.clone().count()
        || pins.len() != items.iter().map(|item| item.pins.len()).sum::<usize>()
        || expressions.len()
            != items
                .iter()
                .filter(|item| item.expression.is_some())
                .count()
    {
        return Err(if ordered {
            typed_metadata_failure()
        } else {
            failure("whole-cell authority contains an unowned pin, plan or signature")
        });
    }
    Ok(items)
}

impl CheckedItem {
    pub(crate) fn signatures(&self) -> &[ExactCheckedSignature] {
        &self.signatures
    }
}

/// Consume the captured checking source and correlated items from one decoded
/// World observation. Authored item source remains in the separate reservation.
pub(crate) fn admit_world_checked_items(
    observations: &[u8],
    reservation: &crate::cell_plan::CompilerWorldCellReservation,
    segments: &[Arc<CheckedTypedSegmentPlan>],
    proofs: &[Arc<crate::compiler_world::WorldItemProof>],
    operation: &tidepool_repr::execution_schema::InventoryOperation,
) -> Result<(Arc<str>, Vec<CheckedItem>), CompileError> {
    let value = operation
        .decode_value(observations, 32 << 20)
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
    let output = cell_observations(&value)?;
    operation
        .charge_value_copies(&value, 1)
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
    operation
        .reserve::<CheckedItem>(reservation.slots().len())
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
    for proof in proofs {
        operation
            .reserve::<ExactCheckedSignature>(proof.signatures().len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        for signature in proof.signatures() {
            // Interface payloads are shared Arcs; names and presentation text
            // are the additional owned copies in the normalized item.
            let text_bytes = signature.names.iter().fold(
                signature
                    .key
                    .len()
                    .saturating_add(signature.presentation.len()),
                |bytes, name| {
                    bytes
                        .saturating_add(name.unit.len())
                        .saturating_add(name.module.len())
                        .saturating_add(name.namespace.len())
                        .saturating_add(name.occurrence.len())
                },
            );
            operation
                .reserve::<ExactSignatureName>(signature.names.len())
                .and_then(|()| operation.charge(text_bytes))
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        }
    }
    let signatures = proofs.iter().flat_map(|proof| proof.signatures());
    let items = decode_checked_items(output, segments.iter().map(AsRef::as_ref), signatures, true)?;
    if items.len() != reservation.plan().items().len() || items.len() != proofs.len() {
        return Err(failure(
            "world observations differ from the reserved item inventory",
        ));
    }
    for ((item, parsed), proof) in items.iter().zip(reservation.plan().items()).zip(proofs) {
        let kind = match parsed.kind() {
            crate::cell_plan::ParsedCellPlanKind::Prologue
            | crate::cell_plan::ParsedCellPlanKind::Declaration => CheckedItemKind::Declaration,
            crate::cell_plan::ParsedCellPlanKind::Bind => CheckedItemKind::Bind,
            crate::cell_plan::ParsedCellPlanKind::Expression => CheckedItemKind::Expression,
        };
        if item.kind != kind
            || item.source != parsed.source()
            || item.binders != parsed.binders()
            || item.signatures != proof.signatures()
        {
            return Err(failure(
                "world observations differ from their checked item proof",
            ));
        }
    }
    Ok((Arc::from(string(&output[2])?), items))
}

/// Adapt the already checked World items into the common checked-cell owner.
/// The reservation and original captured output remain retained in its basis.
pub(crate) fn admit_world_cell(
    program: Arc<crate::compiler_world::CheckedWorldCellProgram>,
    sources: &[(&crate::declaration_context::ExactSourceAdmission, &str)],
    aliases: &[std::path::PathBuf],
    planned_declarations: BTreeMap<usize, PlannedCheckedDeclaration>,
) -> Result<Arc<ExactCheckedCell>, CompileError> {
    if sources.len() != program.stages().len() {
        return Err(failure(
            "World checked cell lacks its exact stage source admissions",
        ));
    }
    for (index, (source, text)) in sources.iter().enumerate() {
        if !source.matches_world_stage(&program, index, text)? {
            return Err(failure(
                "World checked cell source belongs to another stage",
            ));
        }
    }
    let mut originals = 0;
    for item in program.items() {
        match item.kind() {
            crate::compiler_world::WorldItemProofKind::Executable(_) => {
                if planned_declarations.contains_key(&item.ordinal()) {
                    return Err(failure("World executable has a declaration certificate"));
                }
            }
            crate::compiler_world::WorldItemProofKind::Declaration(proof)
            | crate::compiler_world::WorldItemProofKind::Prologue(proof) => {
                originals += 1;
                let planned = planned_declarations.get(&item.ordinal()).ok_or_else(|| {
                    failure("World original lacks its planned declaration certificate")
                })?;
                let (stage_source, stage_text) = sources[item.proof().stage().index()];
                let product = planned.certificate.product();
                if product.owner().unit != proof.owner().unit
                    || product.owner().module != proof.owner().module
                    || product.owner().skinny_iface_sha256 != *proof.iface_sha256()
                    || planned.certificate.toolchain_identity_sha256() != *program.producer_sha256()
                    || planned.receipt_digest != *program.certificate_sha256()
                    || planned.source != stage_text
                    || planned.certificate.source_sha256()
                        != *stage_source.witness().source_sha256()
                {
                    return Err(failure(
                        "World original certificate belongs to another owner or producer",
                    ));
                }
            }
        }
    }
    if planned_declarations.len() != originals {
        return Err(failure(
            "World declaration certificates exceed reserved originals",
        ));
    }
    let value_inputs = CheckedValueInputs::capture_checked(
        program
            .demanded_rows()
            .iter()
            .map(|row| (row.owner(), row.bytes_owned().clone()))
            .collect(),
        program.demanded_rows(),
    )?;
    let injected = program
        .demanded_rows()
        .iter()
        .map(|row| row.owner().module_name())
        .collect();
    let reserved = program
        .reservation()
        .slots()
        .iter()
        .filter_map(|slot| match slot {
            CheckedPlannedCellSlot::Prologue { declaration }
            | CheckedPlannedCellSlot::Declaration { declaration } => Some(
                tidepool_repr::SessionModule::lib(tidepool_repr::Generation(*declaration))
                    .module_name(),
            ),
            _ => None,
        })
        .collect();
    Ok(Arc::new(ExactCheckedCell {
        producer: *program.producer_sha256(),
        context: program.primary_context().semantic_sha256(),
        declaration_context: program.primary_context().clone(),
        retained_projections: Vec::new(),
        receipt_digest: *program.certificate_sha256(),
        checked_source: program.checked_source().clone(),
        evidence: sources
            .iter()
            .map(|(source, text)| ((*text).to_owned(), source.evidence().clone()))
            .collect(),
        observations: program.observations().clone(),
        items: CheckedCellItems::World(program.clone()),
        include: aliases.to_vec(),
        planned_declaration: None,
        planned_declarations,
        typed_segments: program
            .segments()
            .iter()
            .map(|segment| segment.as_ref().clone())
            .collect(),
        value_inputs,
        specification: CheckedCellSpecification::World {
            program,
            injected,
            reserved,
        },
    }))
}

pub(crate) fn admit_checked_cell<'a>(
    root: &Path,
    producer: &[u8],
    context: [u8; 32],
    declaration_context: Arc<crate::declaration_context::ExactDeclarationContext>,
    retained_projections: Vec<Arc<crate::declaration_join::AcceptedJoin>>,
    request_digest: &str,
    specification: Arc<CheckedCellRequest>,
    admissions: impl IntoIterator<Item = &'a ExactSourceAdmission>,
    include: &[std::path::PathBuf],
    planned_declaration: Option<PlannedCheckedDeclaration>,
    value_inputs: Arc<CheckedValueInputs>,
    planned_declarations: BTreeMap<usize, PlannedCheckedDeclaration>,
    program: Option<&CheckedPlannedCellSpecification>,
) -> Result<Arc<ExactCheckedCell>, CompileError> {
    let mut admission_timing = crate::timing::InvocationTiming::start_scoped(
        "checked_cell.admit",
        "read_decode_and_receipt_header",
    );
    let receipt = read(root.join("checked-cell.cbor"), 8 * 1024 * 1024)?;
    let value = decode(&receipt)?;
    let header = row(&value, if program.is_some() { 12 } else { 10 })?;
    let observations = read(root.join("cell.cbor"), 32 * 1024 * 1024)?;
    let output_value = decode(&observations)?;
    let output = cell_observations(&output_value)?;
    let checked_source = string(&output[2])?.to_owned();
    validate_cell_receipt_header(
        header,
        output,
        &observations,
        request_digest,
        &specification,
        program,
    )?;
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
    let typed_segments = match program {
        Some(program) => decode_typed_segment_plans(&header[11], program)?,
        None => Vec::new(),
    };
    admission_timing.advance("late_source_revalidation_and_evidence_capture");
    let evidence = if program.is_some() {
        admissions
            .into_iter()
            .map(|admitted| {
                let source = std::fs::read_to_string(admitted.witness().source_path())?;
                if !admitted
                    .witness()
                    .matches_source(admitted.witness().source_path(), &source)
                    || admitted.evidence().revalidate(&source).is_err()
                {
                    return Err(failure("compiled program source evidence changed"));
                }
                Ok((source, admitted.evidence().clone()))
            })
            .collect::<Result<Vec<_>, CompileError>>()?
    } else {
        let source = admissions
            .into_iter()
            .find(|source| {
                source.witness().source_sha256()
                    == &<[u8; 32]>::from(Sha256::digest(checked_source.as_bytes()))
            })
            .ok_or_else(|| failure("cell has no exact final source witness"))?;
        vec![(checked_source.clone(), source.evidence().clone())]
    };
    admission_timing.advance("signatures_and_item_correspondence");
    let signatures = list(&header[8], 65536)?
        .iter()
        .map(decode_signature)
        .collect::<Result<Vec<_>, _>>()?;
    let items = decode_checked_items(
        output,
        typed_segments.iter(),
        signatures.iter(),
        program.is_some(),
    )?;
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
    admission_timing.advance("assemble_checked_owner");
    let cell = Arc::new(ExactCheckedCell {
        specification: CheckedCellSpecification::Finite(specification),
        producer: crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
            producer,
        )
        .sha256(),
        context,
        declaration_context,
        retained_projections,
        receipt_digest: Sha256::digest(&receipt).into(),
        evidence,
        checked_source: checked_source.into(),
        observations: observations.into(),
        items: items.into(),
        include: include.to_vec(),
        planned_declaration,
        planned_declarations,
        typed_segments,
        value_inputs,
    });
    admission_timing.advance("temporary_receipt_values_drop");
    drop(output_value);
    drop(value);
    drop(receipt);
    admission_timing.finish(true);
    Ok(cell)
}

/// Opaque exact key joining a planned entry with its compiler-issued capture.
/// Identity comes from the normalized plan and native proof, never key parsing.
fn typed_capture_signature_key(entry: &str, capture: &str) -> String {
    format!("{entry}:{capture}")
}

fn typed_metadata_failure() -> CompileError {
    CompileError::CompilerEvidence(Box::new(
        crate::certified_products::CertificationError::Mismatch("typed segment metadata"),
    ))
}

fn decode_signature(value: &Value) -> Result<ExactCheckedSignature, CompileError> {
    let fields = row(value, 5)?;
    if string(&fields[0])? != "TPCHECKEDSIGNATURE2" {
        return Err(failure("checked signature version"));
    }
    let key = string(&fields[1])?.to_owned();
    let Value::Bytes(iface) = &fields[3] else {
        return Err(failure("checked signature IfaceType payload"));
    };
    if iface.is_empty() || iface.len() > 4 * 1024 * 1024 {
        return Err(failure("checked signature IfaceType byte bound"));
    }
    let names = list(&fields[4], 65536)?
        .iter()
        .map(|value| {
            let fields = row(value, 4)?;
            let unit = string(&fields[0])?.to_owned();
            let module = string(&fields[1])?.to_owned();
            let namespace = string(&fields[2])?.to_owned();
            let occurrence = string(&fields[3])?.to_owned();
            if unit.is_empty()
                || module.is_empty()
                || occurrence.is_empty()
                || !matches!(namespace.as_str(), "type" | "data" | "var")
            {
                return Err(failure(
                    "invalid signature Name owner, occurrence or namespace",
                ));
            }
            Ok(ExactSignatureName {
                unit,
                module,
                namespace,
                occurrence,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if names.windows(2).any(|pair| {
        (
            &pair[0].unit,
            &pair[0].module,
            &pair[0].namespace,
            &pair[0].occurrence,
        ) >= (
            &pair[1].unit,
            &pair[1].module,
            &pair[1].namespace,
            &pair[1].occurrence,
        )
    }) {
        return Err(failure(
            "checked signature Names are unsorted or duplicated",
        ));
    }
    Ok(ExactCheckedSignature {
        key,
        presentation: string(&fields[2])?.to_owned(),
        iface: iface.clone().into(),
        names,
    })
}

fn world_binder_word(value: &Value, message: &'static str) -> Result<u64, CompileError> {
    match value {
        Value::Integer(value) => u64::try_from(*value).map_err(|_| failure(message)),
        _ => Err(failure(message)),
    }
}

fn validate_world_binder_symbol(value: &Value) -> Result<(), CompileError> {
    let fields = row(value, 5)?;
    if string(&fields[0])?.is_empty()
        || string(&fields[1])?.is_empty()
        || string(&fields[2])?.is_empty()
        || string(&fields[3])?.is_empty()
    {
        return Err(failure(
            "world binder type identity has an empty symbol field",
        ));
    }
    match &fields[4] {
        Value::Array(option) => match option.as_slice() {
            [Value::Integer(tag)] if u64::try_from(*tag).ok() == Some(0) => Ok(()),
            [Value::Integer(tag), Value::Text(parent)]
                if u64::try_from(*tag).ok() == Some(1) && !parent.is_empty() =>
            {
                Ok(())
            }
            _ => Err(failure(
                "world binder type identity has an invalid record parent",
            )),
        },
        _ => Err(failure(
            "world binder type identity has an invalid record parent",
        )),
    }
}

fn validate_world_binder_root_head(value: &Value) -> Result<(), CompileError> {
    let fields = row(value, 2)?;
    validate_world_binder_symbol(&fields[0])?;
    // `encodeTypeIdentity` uses the execution schema's short constructor
    // arrays: `[0]` for a wired-in declaration and `[1, bytes]` for a GHC ABI.
    // These are not CBOR tags.
    let version = match &fields[1] {
        Value::Array(version) => version.as_slice(),
        _ => return Err(failure("world binder type identity has an invalid version")),
    };
    match version {
        [Value::Integer(tag)] if u64::try_from(*tag).ok() == Some(0) => Ok(()),
        [Value::Integer(tag), Value::Bytes(bytes)]
            if u64::try_from(*tag).ok() == Some(1) && bytes.len() == 16 =>
        {
            Ok(())
        }
        _ => Err(failure("world binder type identity has an invalid version")),
    }
}

/// Decode one world item's original binders (field 10) against the exact
/// reserved binder identity for that item. Binder order, names and stable
/// variable ids must all match; nothing here issues a runtime value.
pub(crate) fn decode_world_item_binders(
    value: &Value,
    expected: &[(String, tidepool_repr::SessionVarId)],
) -> Result<Vec<WorldItemBinder>, CompileError> {
    let entries = list(value, 65536)?;
    if entries.len() != expected.len() {
        return Err(failure(
            "world item binder count differs from its exact reservation",
        ));
    }
    let mut decoded = Vec::with_capacity(entries.len());
    let mut seen = BTreeSet::new();
    for (entry, (expected_name, expected_id)) in entries.iter().zip(expected) {
        let fields = row(entry, 7)?;
        let name = string(&fields[0])?.to_owned();
        let varid = world_binder_word(&fields[1], "world binder id is not unsigned")?;
        let module = string(&fields[2])?.to_owned();
        let tier = match string(&fields[3])? {
            "ForceData" => WorldItemBinderTier::ForceData,
            "RetainOpaque" => WorldItemBinderTier::RetainOpaque,
            _ => return Err(failure("world binder has an unknown closure tier")),
        };
        let type_display = string(&fields[4])?.to_owned();
        let root_head = match &fields[5] {
            Value::Null => None,
            value => {
                validate_world_binder_root_head(value)?;
                Some(value.clone())
            }
        };
        let host_authority = match &fields[6] {
            Value::Null => None,
            Value::Text(authority) => Some(match authority.as_str() {
                "JsonValue" => WorldItemHostAuthority::JsonValue,
                "Text" => WorldItemHostAuthority::Text,
                "CommandJob" => WorldItemHostAuthority::CommandJob,
                _ => return Err(failure("world binder has an unknown host authority")),
            }),
            _ => return Err(failure("world binder has an invalid host authority")),
        };
        if name.is_empty()
            || module.is_empty()
            || name != *expected_name
            || varid != expected_id.raw()
            || !seen.insert(varid)
        {
            return Err(failure(
                "world binder differs from its exact reserved identity",
            ));
        }
        decoded.push(WorldItemBinder {
            name,
            varid,
            module,
            tier,
            type_display,
            root_head,
            host_authority,
        });
    }
    Ok(decoded)
}

/// Decode one world item's checked signatures (field 11) and require each key
/// to name the typed item's entry and its ordered capture.
pub(crate) fn decode_world_item_signatures(
    value: &Value,
    entry: &str,
    captures: &[&str],
) -> Result<Vec<ExactCheckedSignature>, CompileError> {
    let entries = list(value, 65536)?;
    if entries.len() != captures.len() {
        return Err(failure(
            "world item signature count differs from its typed captures",
        ));
    }
    let mut decoded = Vec::with_capacity(entries.len());
    for (value, capture) in entries.iter().zip(captures) {
        let signature = decode_signature(value)?;
        if signature.key() != format!("{entry}:{capture}") {
            return Err(failure(
                "world item signature differs from its typed entry or capture",
            ));
        }
        decoded.push(signature);
    }
    Ok(decoded)
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
    crate::host_work::checkpoint()?;
    let path = path.as_ref();
    let oversized = |actual| {
        failure(format!(
            "checked evidence {} has {actual} bytes, exceeding the {limit}-byte bound",
            path.display()
        ))
    };
    let actual = std::fs::metadata(path)
        .map_err(|error| failure(format!("checked evidence {}: {error}", path.display())))?
        .len();
    if actual > limit {
        return Err(oversized(actual));
    }
    let bytes = crate::host_work::read(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::Interrupted {
            return CompileError::Io(error);
        }
        failure(format!("checked evidence {}: {error}", path.display()))
    })?;
    if bytes.len() as u64 > limit {
        return Err(oversized(bytes.len() as u64));
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
/// Decode the matched observation format; old bare rows cannot satisfy a
/// checked receipt or parser capability under the current compiler protocol.
pub(crate) fn cell_observations(value: &Value) -> Result<&[Value], CompileError> {
    let envelope = row(value, 3)?;
    if string(&envelope[0])? != "TPCELLOBSERVATIONS"
        || envelope[1]
            .as_integer()
            .and_then(|value| u64::try_from(value).ok())
            != Some(3)
    {
        return Err(failure("unsupported cell observations version"));
    }
    let payload = row(&envelope[2], 5)?;
    for pin in list(&payload[1], 65536)? {
        row(pin, 3)?;
    }
    for expression in list(&payload[4], 65536)? {
        row(expression, 4)?;
    }
    Ok(payload)
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
pub(crate) fn array<const N: usize>(values: [Value; N]) -> Value {
    Value::Array(Vec::from(values))
}
pub(crate) fn text(value: impl AsRef<str>) -> Value {
    Value::Text(value.as_ref().to_owned())
}
pub(crate) fn hash(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes).into())
}
pub(crate) fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn failure(error: impl std::fmt::Display) -> CompileError {
    CompileError::ExtractFailed(format!("checked cell: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture_request(
        cell_source: impl Into<String>,
        template_source: impl Into<String>,
        turn_templates: Vec<(String, String)>,
        injected_modules: Vec<String>,
    ) -> Arc<CheckedCellRequest> {
        Arc::new(
            CheckedCellRequest::issue(
                Arc::new(
                    CellRecipe::new(
                        cell_source,
                        template_source,
                        turn_templates,
                        injected_modules,
                    )
                    .unwrap(),
                ),
                AdmissionDigest::finalize([3; 32]).unwrap(),
                vec![],
            )
            .unwrap(),
        )
    }

    #[test]
    fn selected_native_sites_compose_semantically_and_refuse_conflicts_atomically() {
        use super::*;
        use proptest::prelude::*;
        use tidepool_repr::execution_schema::{testing, SiteDelivery, SiteRow, TypeNodeId};
        use tidepool_repr::type_graph::DeclarationForm;
        let shape = (
            testing::identity("Fixture", "Completion"),
            DeclarationForm::Data,
        );
        let first_graph = testing::closed_type_roots(&[shape.clone()]);
        let relocated_graph = testing::closed_type_roots(&[
            (testing::identity("Fixture", "Other"), DeclarationForm::Data),
            shape,
        ]);
        let row = |site, wire| SiteRow {
            site,
            origin: format!("Fixture.completion_{site}"),
            ordinal: 0,
            delivery: SiteDelivery::HostAnswer,
            wire: TypeNodeId(wire),
            inputs: vec![],
        };
        let mut pool = Vec::new();
        for (site, wire, graph) in [
            (7, 0, &first_graph),
            (7, 1, &relocated_graph),
            (9, 0, &first_graph),
        ] {
            let mut issued = SelectedNativeSites::default();
            issued
                .insert_issued(row(site, wire), Arc::clone(graph))
                .unwrap();
            pool.push(issued);
        }
        let mut relocated = pool[0].clone();
        relocated
            .merge(&pool[1])
            .expect("graph-local roots do not define site identity");
        let mut config = proptest::test_runner::Config::default();
        if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
            config.failure_persistence = Some(Box::new(
                proptest::test_runner::FileFailurePersistence::Direct(path),
            ));
        }
        let mut config = proptest::test_runner::contextualize_config(config);
        config.source_file = Some(file!());
        config.test_name = Some(concat!(
            module_path!(),
            "::selected_native_sites_compose_semantically_and_refuse_conflicts_atomically"
        ));
        proptest::test_runner::TestRunner::new(config)
            .run(&prop::collection::vec(0_usize..3, 0..48), |history| {
                let mut issued = SelectedNativeSites::default();
                let mut expected = BTreeSet::new();
                for index in &history {
                    issued.merge(&pool[*index]).unwrap();
                    expected.insert(if *index == 2 { 9 } else { 7 });
                    prop_assert_eq!(issued.ids().collect::<BTreeSet<_>>(), expected.clone());
                    prop_assert!(!issued.has_completion_site(11));
                }
                let mut reversed = SelectedNativeSites::default();
                for index in history.iter().rev() {
                    reversed.merge(&pool[*index]).unwrap();
                }
                prop_assert_eq!(
                    issued.ids().collect::<Vec<_>>(),
                    reversed.ids().collect::<Vec<_>>()
                );
                issued
                    .merge(&reversed)
                    .expect("merge order preserves semantic site metadata");
                let before = issued.clone();
                issued.merge(&before).unwrap();
                prop_assert_eq!(issued, before);
                Ok(())
            })
            .unwrap();
        for mutation in 0..5 {
            let mut incoming = row(7, 0);
            match mutation {
                0 => incoming.origin = "another owner".into(),
                1 => incoming.ordinal = 1,
                2 => incoming.delivery = SiteDelivery::LiveReentry,
                3 => incoming.inputs.push(TypeNodeId(0)),
                4 => incoming.wire = TypeNodeId(0),
                _ => unreachable!(),
            }
            let mut other = pool[2].clone();
            other
                .insert_issued(
                    incoming,
                    if mutation == 4 {
                        Arc::clone(&relocated_graph)
                    } else {
                        Arc::clone(&first_graph)
                    },
                )
                .unwrap();
            let mut issued = pool[0].clone();
            let before = issued.clone();
            assert!(issued.merge(&other).is_err());
            assert_eq!(
                issued, before,
                "failed merge must not add the unrelated site either"
            );
        }
        let observed = crate::YieldSite {
            site: 7,
            origin: "Fixture.completion_7".into(),
            ordinal: 0,
            ty: "Int".into(),
            modules: vec![],
            heads: vec![],
            inputs: vec![crate::SiteType {
                ty: "Int".into(),
                modules: vec![],
                heads: vec![],
            }],
            input_type_witnesses: vec![None],
            reply_declaration: None,
            request_type_signatures: None,
        };
        assert!(
            pool[0].validate_observations(&[observed]).is_err(),
            "an observed input cannot reuse an issued completion id"
        );
    }

    #[test]
    fn checked_target_demands_wrapper_dependency_separately_from_authored_entry() {
        use super::*;
        use crate::artifact_inventory::{
            ArtifactEntry, ArtifactInventory, CompilerInputProjection, NativeArtifactDemand,
            NativeGroupKey,
        };
        use crate::certified_products::{
            self, AcceptedGlobal, AuthenticatedTargetImports, CertifiedSourceSelection,
            ReceiptImportOwner,
        };
        use crate::recovery_artifacts::PackageInterfaceValidation;
        use tidepool_repr::execution_schema::{
            testing, GlobalDecl, Group, ModuleVersion, RuntimeRep,
        };
        let products = ["Authored", "Wrapper"]
            .iter()
            .map(|module| {
                let completion = |ordinal: u32| tidepool_repr::execution_schema::SiteRow {
                    site: u64::from(ordinal) * 10 + u64::from(*module == "Wrapper"),
                    origin: format!("{module}.entry_{ordinal}"),
                    ordinal: 0,
                    delivery: tidepool_repr::execution_schema::SiteDelivery::HostAnswer,
                    wire: tidepool_repr::execution_schema::TypeNodeId(0),
                    inputs: vec![],
                };
                let native = certified_products::fixture_finalized_product(
                    certified_products::tests::original_groups_fixture_with_sites(
                        module,
                        vec![(8, vec![]), (12, vec![])],
                        7,
                        &BTreeMap::new(),
                        vec![0x42],
                        &BTreeMap::from([(8, vec![completion(8)]), (12, vec![completion(12)])]),
                    ),
                    [1; 32],
                );
                certified_products::tests::recovered_witness_fixtures(&[native])
                    .remove(0)
                    .product
            })
            .collect::<Vec<_>>();
        let mut wire = testing::wire_program();
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.identity = testing::identity("Authored", "entry_8");
        let authored_target = Arc::new(testing::prepare(wire.clone()).unwrap());
        let mut changed_wire = wire.clone();
        changed_wire.expressions.nodes[0] =
            tidepool_repr::execution_schema::ExprFrame::Return(vec![
                tidepool_repr::execution_schema::Atom::Scalar(
                    tidepool_repr::execution_schema::ScalarLiteral::Int {
                        bits: 64,
                        bytes: 43_i64.to_be_bytes().to_vec(),
                    },
                ),
            ]);
        let changed_target = Arc::new(testing::prepare(changed_wire).unwrap());
        let wrapper_binder = testing::identity("Wrapper", "entry_8");
        wire.globals = vec![GlobalDecl {
            identity: wrapper_binder.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: None,
        }];
        let target = Arc::new(testing::prepare(wire).unwrap());
        let plans = vec![CheckedTypedSegmentPlan {
            digest: hex(&[2; 32]),
            root: "authored_origin".into(),
            items: vec![CheckedTypedSegmentItem {
                ordinal: 0,
                entry: "entry_8".into(),
                generation: 1,
                body: CheckedTypedSegmentBody::Let {
                    marker: "marker".into(),
                    captures: vec![],
                },
            }],
        }];
        let request = hex(&[3; 32]);
        let admission = [4; 32];
        let source = "authored fixture source";
        let directory = tempfile::tempdir().unwrap();
        let identity =
            |occurrence: &str| array([text("fixture"), text("Authored"), text(occurrence)]);
        let receipt = array([
            text("TPEXACTITEM"),
            text("2"),
            text(&request),
            text(hex(&admission)),
            text(hex(&admission)),
            Value::Integer(0.into()),
            text(hash(source.as_bytes())),
            text("tidepool-checked-recipe-2"),
            array([
                text(&plans[0].digest),
                identity(&plans[0].root),
                identity("entry_8"),
                Value::Integer(8.into()),
            ]),
        ]);
        let mut receipt_bytes = Vec::new();
        ciborium::ser::into_writer(&receipt, &mut receipt_bytes).unwrap();
        std::fs::write(directory.path().join("checked-item.cbor"), receipt_bytes).unwrap();
        let entry = CheckedTypedEntry::issue_program_item(
            directory.path(),
            &request,
            admission,
            &plans,
            0,
            source,
            &target,
            &crate::declaration_join::ExactModuleIdentity {
                unit: "fixture".into(),
                module: "Authored".into(),
            },
            &products,
            [1; 32],
        )
        .unwrap();
        let authored_entry = CheckedTypedEntry::issue_program_item(
            directory.path(),
            &request,
            admission,
            &plans,
            0,
            source,
            &authored_target,
            &crate::declaration_join::ExactModuleIdentity {
                unit: "fixture".into(),
                module: "Authored".into(),
            },
            &products,
            [1; 32],
        )
        .unwrap();
        let changed_entry = CheckedTypedEntry::issue_program_item(
            directory.path(),
            &request,
            admission,
            &plans,
            0,
            source,
            &changed_target,
            &crate::declaration_join::ExactModuleIdentity {
                unit: "fixture".into(),
                module: "Authored".into(),
            },
            &products,
            [1; 32],
        )
        .unwrap();
        for (module, ordinal) in [("WrongOwner", 8), ("Authored", 12), ("Authored", 99)] {
            let wrong_owner = crate::declaration_join::ExactModuleIdentity {
                unit: "fixture".into(),
                module: module.into(),
            };
            assert!(
                CheckedTypedEntry::from_original_entry(
                    plans[0].digest.clone(),
                    entry.origin.clone(),
                    entry.entry.clone(),
                    ordinal,
                    &target,
                    &wrong_owner,
                    &products,
                    [1; 32],
                )
                .is_err(),
                "native entry owner and exact original ordinal are independently authenticated"
            );
        }
        let accepted = vec![AcceptedGlobal {
            identity: wrapper_binder.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            owner: ReceiptImportOwner::Source {
                unit: "fixture".into(),
                module: "Wrapper".into(),
                module_version: Some(products[1].owner().module_version.clone()),
                original_ordinal: 8,
                binder: wrapper_binder,
            },
        }];
        let mut validation = PackageInterfaceValidation::default();
        let entries = products
            .iter()
            .cloned()
            .map(|product| {
                Arc::new(
                    ArtifactEntry::original_with_validation([1; 32], product, &mut validation)
                        .unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let inventory = ArtifactInventory::default();
        let baseline = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                entries.clone(),
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
        let selection = CertifiedSourceSelection::from_compiler_projection(
            &projection,
            &baseline.metadata_snapshot(),
            &validation.inventory,
        )
        .unwrap();
        let fixed = inventory
            .bind_segment(&baseline, entries.clone(), &selection, &mut validation)
            .unwrap();
        let physical = fixed.captured_stage().physical_cohort();
        let observed =
            AuthenticatedTargetImports::issue(&authored_target, &[], &validation.inventory)
                .unwrap();
        let authored_admission = inventory
            .select_native_target(
                physical,
                observed,
                Some(authored_entry.clone()),
                None,
                &[],
                &BTreeMap::new(),
                &mut validation,
            )
            .unwrap();
        let before = inventory.metrics();
        let observed =
            AuthenticatedTargetImports::issue(&authored_target, &[], &validation.inventory)
                .unwrap();
        let wrong_body = inventory
            .select_native_target(
                physical,
                observed,
                Some(changed_entry),
                None,
                &[],
                &BTreeMap::new(),
                &mut validation,
            )
            .map(|_| ());
        assert!(
            matches!(&wrong_body, Err(CompileError::ExtractFailed(detail))
                if detail == "artifact inventory: typed target entry belongs to another prepared target"),
            "same entry symbol cannot borrow a different target body; actual={wrong_body:?}",
        );
        assert_eq!(
            inventory.metrics().issued_scope_nodes,
            before.issued_scope_nodes
        );
        let authored_only = authored_admission.artifacts();
        assert_eq!(
            authored_only.selected_native_groups(),
            BTreeSet::from([entry.native_group_key()]),
        );
        assert!(
            AuthenticatedTargetImports::issue(&target, &[], &validation.inventory).is_err(),
            "the wrapper's declared import cannot disappear from its observation"
        );
        let observed =
            AuthenticatedTargetImports::issue(&target, &accepted, &validation.inventory).unwrap();
        let complete_admission = inventory
            .select_native_target(
                physical,
                observed,
                Some(entry.clone()),
                None,
                &[],
                &BTreeMap::new(),
                &mut validation,
            )
            .unwrap();
        let complete = complete_admission.artifacts().clone();
        assert_eq!(
            complete.selected_native_groups(),
            BTreeSet::from([
                entry.native_group_key(),
                NativeGroupKey {
                    artifact: entries[1].descriptor.id,
                    original_ordinal: 8
                },
            ])
        );
        assert_eq!(complete.artifact_ids(), baseline.artifact_ids());
        let authored_resolved = authored_admission.resolve().unwrap();
        let complete_resolved = complete_admission.resolve().unwrap();
        let authored_sites = authored_resolved.native_sites().unwrap();
        assert_eq!(authored_sites.ids().collect::<Vec<_>>(), vec![80]);
        let complete_sites = complete_resolved.native_sites().unwrap();
        assert_eq!(
            authored_resolved.requirements_for_entry().unwrap(),
            complete_resolved.requirements_for_entry().unwrap()
        );
        assert_eq!(complete_sites.ids().collect::<Vec<_>>(), vec![80, 81]);
        for unselected in [120, 121] {
            assert!(
                !complete_sites.has_completion_site(unselected),
                "available sibling groups cannot issue completion authority"
            );
        }
        let scopes_before_mutations = inventory.metrics().issued_scope_nodes;
        for mutation in 0..4 {
            let mut invalid = accepted.clone();
            let ReceiptImportOwner::Source {
                module,
                module_version,
                original_ordinal,
                binder,
                ..
            } = &mut invalid[0].owner
            else {
                unreachable!()
            };
            match mutation {
                0 => *module_version = Some(ModuleVersion([9; 32])),
                1 => *module = "WrongOwner".into(),
                2 => *original_ordinal = 99,
                3 => {
                    *original_ordinal = 12;
                    *binder = testing::identity("Wrapper", "entry_12");
                }
                _ => unreachable!(),
            }
            let observed =
                AuthenticatedTargetImports::issue(&target, &invalid, &validation.inventory);
            if mutation == 3 {
                assert!(matches!(
                    observed,
                    Err(certified_products::CertificationError::Mismatch(
                        "global owner"
                    ))
                ));
                continue;
            }
            let denied = inventory
                .select_native_target(
                    physical,
                    observed.unwrap(),
                    Some(entry.clone()),
                    None,
                    &[],
                    &BTreeMap::new(),
                    &mut validation,
                )
                .map(|_| ());
            let refused = match mutation {
                1 => matches!(&denied, Err(CompileError::CompilerEvidence(error))
                    if matches!(error.as_ref(), certified_products::CertificationError::SourceOutsideCompilerSelection {
                        owner, requested_version, original_ordinal: None,
                    } if owner.unit == "fixture" && owner.module == "WrongOwner"
                        && requested_version == &products[1].owner().module_version)),
                0 | 2 => {
                    let expected = if mutation == 0 {
                        "source module version"
                    } else {
                        "target Source exact native membership"
                    };
                    matches!(&denied, Err(CompileError::CompilerEvidence(error))
                        if matches!(error.as_ref(), certified_products::CertificationError::Mismatch(actual)
                            if *actual == expected))
                }
                _ => unreachable!(),
            };
            assert!(refused, "mutation={mutation}, actual={denied:?}");
        }
        assert_eq!(
            inventory.metrics().issued_scope_nodes,
            scopes_before_mutations,
        );
        let missing_inventory = ArtifactInventory::default();
        let missing_view = missing_inventory
            .admit_shared_with_demand(
                &missing_inventory.empty_view(),
                vec![entries[0].clone()],
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let missing_projection =
            CompilerInputProjection::from_issued_entries(&entries[..1]).unwrap();
        let missing_selection = CertifiedSourceSelection::from_compiler_projection(
            &missing_projection,
            &missing_view.metadata_snapshot(),
            &validation.inventory,
        )
        .unwrap();
        let missing_fixed = missing_inventory
            .bind_segment(
                &missing_view,
                vec![entries[0].clone()],
                &missing_selection,
                &mut validation,
            )
            .unwrap();
        let observed =
            AuthenticatedTargetImports::issue(&target, &accepted, &validation.inventory).unwrap();
        let missing_admission = missing_inventory
            .select_native_target(
                missing_fixed.captured_stage().physical_cohort(),
                observed,
                Some(entry),
                None,
                &[],
                &BTreeMap::new(),
                &mut validation,
            )
            .map(|_| ());
        assert!(
            matches!(&missing_admission, Err(CompileError::CompilerEvidence(error))
                if matches!(error.as_ref(), certified_products::CertificationError::SourceOutsideCompilerSelection {
                    owner, requested_version, original_ordinal: None,
                } if owner.unit == products[1].owner().unit
                    && owner.module == products[1].owner().module
                    && requested_version == &products[1].owner().module_version)),
            "the omitted Wrapper must refuse at source selection: {missing_admission:?}",
        );
    }

    #[test]
    fn observation_migration_refuses_unversioned_and_import_bearing_rows() {
        use super::*;
        let payload = Value::Array(vec![
            Value::Array(vec![]),
            Value::Array(vec![]),
            text(""),
            Value::Array(vec![Value::Array(vec![]), Value::Array(vec![])]),
            Value::Array(vec![]),
        ]);
        let envelope = Value::Array(vec![text("TPCELLOBSERVATIONS"), 3.into(), payload.clone()]);
        assert!(cell_observations(&envelope).is_ok());
        assert!(cell_observations(&payload).is_err());
        let mut old_version = envelope.clone();
        old_version.as_array_mut().unwrap()[1] = 2.into();
        assert!(cell_observations(&old_version).is_err());
        for (section, count) in [(1, 4), (4, 5)] {
            let mut old_row = envelope.clone();
            old_row.as_array_mut().unwrap()[2].as_array_mut().unwrap()[section] =
                Value::Array(vec![Value::Array(vec![Value::Null; count])]);
            assert!(cell_observations(&old_row).is_err());
        }
    }

    use super::{CheckedPrefixSequence, CheckedValueInputs, RequestTypeSignatures};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    fn capture_test_value(
        original: &Arc<crate::declaration_context::ExactDeclarationContext>,
        demands_epoch: bool,
    ) -> Arc<super::CheckedValueArtifact> {
        capture_test_value_from_output(original, demands_epoch, false)
    }

    fn capture_test_value_from_output(
        original: &Arc<crate::declaration_context::ExactDeclarationContext>,
        demands_epoch: bool,
        reuse: bool,
    ) -> Arc<super::CheckedValueArtifact> {
        capture_test_value_from_output_for_generation(original, demands_epoch, reuse, 1)
    }

    fn prepare_test_value_interface_files(
        generation: u64,
        demands_epoch: bool,
    ) -> (
        Arc<CheckedValueInputs>,
        tidepool_repr::SessionModule,
        std::path::PathBuf,
    ) {
        use super::*;
        let inputs = CheckedValueInputs::capture_raw(vec![]).unwrap();
        let owner = tidepool_repr::SessionModule::val(tidepool_repr::Generation(generation));
        let path = inputs.root().join(owner.relative_hi_path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes = b"opaque checked package-only value interface";
        std::fs::write(&path, bytes).unwrap();
        let packages = array([
            text("TPPKGROOTS"),
            text("2"),
            array([text("main"), text(owner.module_name()), text(hash(bytes))]),
            array([]),
            array([]),
        ]);
        let mut encoded = vec![];
        ciborium::ser::into_writer(&packages, &mut encoded).unwrap();
        std::fs::write(path.with_extension("hi.packages"), encoded).unwrap();
        let mut encoded = vec![];
        let requirements = if demands_epoch {
            array([array([text("fixture"), text("Epoch")])])
        } else {
            array([])
        };
        ciborium::ser::into_writer(&requirements, &mut encoded).unwrap();
        std::fs::write(path.with_extension("hi.requirements"), encoded).unwrap();
        (inputs, owner, path)
    }

    fn empty_test_checked_cell(
        producer: [u8; 32],
        original: Arc<crate::declaration_context::ExactDeclarationContext>,
        inputs: Arc<CheckedValueInputs>,
    ) -> super::ExactCheckedCell {
        use super::*;
        ExactCheckedCell {
            specification: CheckedCellSpecification::Finite(fixture_request(
                String::new(),
                String::new(),
                vec![],
                vec![],
            )),
            producer,
            context: original.semantic_sha256(),
            declaration_context: original,
            retained_projections: vec![],
            receipt_digest: [4; 32],
            checked_source: "".into(),
            evidence: vec![],
            observations: vec![].into(),
            items: vec![].into(),
            include: vec![],
            planned_declaration: None,
            planned_declarations: BTreeMap::new(),
            typed_segments: vec![],
            value_inputs: inputs,
        }
    }

    fn capture_test_produced_value_interface(
        producer: [u8; 32],
        owner: tidepool_repr::SessionModule,
        path: &std::path::Path,
        ordinal: usize,
    ) -> super::ProducedValueTypeInterfaces {
        use super::*;
        let interface = CapturedValueInterfaceOutput::capture(path)
            .unwrap()
            .certify(producer, owner, None)
            .unwrap();
        let bytes = interface.interface_bytes_owned();
        let output = ProducedValueTypeInterface {
            row: crate::declaration_context::CheckedValueRowIssuer::new(interface.clone()),
            ordinal,
            input: ValueInterfaceBytes {
                owner,
                module: owner.module_name(),
                digest: hash(&bytes),
                bytes,
                path: path.to_owned(),
            },
            interface,
        };
        ProducedValueTypeInterfaces::from_outputs(vec![output], ordinal..ordinal + 1).unwrap()
    }

    fn capture_test_value_from_output_for_generation(
        original: &Arc<crate::declaration_context::ExactDeclarationContext>,
        demands_epoch: bool,
        reuse: bool,
        generation: u64,
    ) -> Arc<super::CheckedValueArtifact> {
        capture_test_value_data(original, demands_epoch, reuse, generation)
    }

    fn capture_test_value_data(
        original: &Arc<crate::declaration_context::ExactDeclarationContext>,
        demands_epoch: bool,
        reuse: bool,
        generation: u64,
    ) -> Arc<super::CheckedValueArtifact> {
        use super::*;
        let producer = [2; 32];
        let (inputs, owner, path) = prepare_test_value_interface_files(generation, demands_epoch);
        let cell = ExactCheckedCell {
            specification: CheckedCellSpecification::Finite(fixture_request(
                String::new(),
                "",
                vec![],
                vec![],
            )),
            producer,
            context: original.semantic_sha256(),
            declaration_context: Arc::clone(original),
            retained_projections: vec![],
            receipt_digest: [4; 32],
            checked_source: "".into(),
            evidence: vec![],
            observations: vec![].into(),
            items: vec![].into(),
            include: vec![],
            planned_declaration: None,
            planned_declarations: BTreeMap::new(),
            typed_segments: vec![],
            value_inputs: inputs.clone(),
        };
        let produced = reuse.then(|| {
            let produced = capture_test_produced_value_interface(producer, owner, &path, 4);
            for output_path in [
                &path,
                &path.with_extension("hi.packages"),
                &path.with_extension("hi.requirements"),
            ] {
                std::fs::remove_file(output_path).unwrap();
            }
            produced
        });
        let captured = inputs
            .capture_value_data(
                generation,
                &cell,
                original,
                &[],
                None,
                super::CheckedValueCaptureInput::Ordinary {
                    produced: produced.as_ref(),
                },
            )
            .unwrap();
        if let Some(produced) = produced {
            let output = produced.output(owner).unwrap();
            let prepared_context = Arc::new(
                (**original)
                    .clone()
                    .extend_program_value_interface(output.interface.clone())
                    .unwrap(),
            );
            let support = crate::declaration_context::PreparedValueSourceSupport::new(
                prepared_context.clone(),
                &[],
            )
            .unwrap();
            let prepared = inputs
                .capture_value_data(
                    1,
                    &cell,
                    &prepared_context,
                    &[],
                    None,
                    super::CheckedValueCaptureInput::Prepared {
                        support: &support,
                        produced: &produced,
                    },
                )
                .unwrap();
            assert_eq!(
                prepared.certified_interface(),
                captured.certified_interface()
            );
            assert_eq!(
                prepared.compiler_input_projection(),
                captured.compiler_input_projection()
            );
            assert_eq!(prepared.source_lexical(), captured.source_lexical());
            assert_eq!(
                prepared, captured,
                "ordinary and prepared retain the same issued value authority"
            );

            assert_eq!(
                prepared.compiler_artifact_view().capture_graph_selection(),
                captured.compiler_artifact_view().capture_graph_selection()
            );
            assert!(
                inputs
                    .capture_value_data(
                        generation,
                        &cell,
                        original,
                        &[],
                        None,
                        super::CheckedValueCaptureInput::Prepared {
                            support: &support,
                            produced: &produced
                        },
                    )
                    .is_err(),
                "another issuer cannot borrow the prepared support"
            );
            let empty_support =
                crate::declaration_context::PreparedValueSourceSupport::new(original.clone(), &[])
                    .unwrap();
            assert!(
                inputs
                    .capture_value_data(
                        generation,
                        &cell,
                        original,
                        &[],
                        None,
                        super::CheckedValueCaptureInput::Prepared {
                            support: &empty_support,
                            produced: &produced
                        },
                    )
                    .is_err(),
                "prepared capture never readmits an absent value interface"
            );
            let artifact = output.interface.artifact_id();
            let hidden = produced.for_segment(4..4).unwrap();
            assert!(Arc::ptr_eq(&hidden.lookup, &produced.lookup));
            assert!(hidden.owns_artifact(artifact));
            assert!(!hidden.selects_artifact(artifact));
            assert!(hidden.current_interface(owner.gen.0).is_err());
            assert_eq!(hidden.interfaces().count(), 0);
            assert_eq!(hidden.required().count(), 0);
            let item = produced.for_item(4).unwrap();
            assert!(item.selects_artifact(artifact));
            assert!(Arc::ptr_eq(
                item.current_interface(owner.gen.0).unwrap(),
                &output.interface
            ));
            assert!(Arc::ptr_eq(captured.bytes_owned(), &output.input.bytes));
            assert!(Arc::ptr_eq(
                captured.certified_interface(),
                &output.interface
            ));
            assert_eq!(
                inputs.work().output_files_hashed,
                0,
                "final capture must add only contextual authority"
            );
        }
        captured
    }

    #[test]
    fn final_value_capture_reuses_private_output_bytes_and_certificate_after_paths_are_removed() {
        let context = Arc::new(
            crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![]).unwrap(),
        );
        let ordinary = capture_test_value(&context, false);
        let retained = capture_test_value_from_output(&context, false, true);
        assert_eq!(
            ordinary.certified_interface(),
            retained.certified_interface()
        );
        assert_eq!(ordinary.bytes_owned(), retained.bytes_owned());
        assert_eq!(
            ordinary.compiler_artifact_view().capture_graph_selection(),
            retained.compiler_artifact_view().capture_graph_selection()
        );
    }

    fn check_captured_value_epoch_reattachment(demands_epoch: bool) {
        use super::*;
        let producer = [2; 32];
        let epoch = |version| {
            let product = crate::certified_products::fixture_finalized_product(
                crate::certified_products::tests::original_groups_fixture_with_interface(
                    "Epoch",
                    vec![(7, vec![])],
                    version,
                    &BTreeMap::new(),
                    vec![version; 16],
                ),
                producer,
            );
            let body = product.product_bytes().to_vec();
            let interface = product.interface_bytes().to_vec();
            let authenticated =
                crate::certified_products::tests::recovered_witness_fixtures(&[product])
                    .remove(0)
                    .product;
            assert_eq!(authenticated.product_bytes(), body);
            assert_eq!(authenticated.interface_bytes(), interface);
            assert_eq!(
                crate::certified_products::original_available_groups(&authenticated)
                    .unwrap()
                    .count(),
                1
            );
            authenticated
        };
        let context = |version| {
            use crate::artifact_inventory::{
                ArtifactEntry, ArtifactInventory, CompilerInputProjection, CompilerSourceSupport,
                NativeArtifactDemand,
            };
            let entry = Arc::new(ArtifactEntry::original(producer, epoch(version)).unwrap());
            let inventory = ArtifactInventory::default();
            let artifacts = inventory
                .admit_shared_with_demand(
                    &inventory.empty_view(),
                    vec![entry.clone()],
                    NativeArtifactDemand::ScopeInterfaces,
                )
                .unwrap();
            let projection = CompilerInputProjection::from_issued_entries(&[entry]).unwrap();
            let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
            let mut selection =
                crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                    &projection,
                    &artifacts.metadata_snapshot(),
                    &validation.inventory,
                )
                .unwrap();
            let stage = crate::declaration_context::CapturedCompilerStage::issue(
                &mut selection,
                &artifacts,
                Vec::new(),
                &mut validation,
            )
            .unwrap();
            let output = stage.compiler_inputs();
            let support = CompilerSourceSupport::close_complete_output(output).unwrap();
            Arc::new(
                crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![])
                    .unwrap()
                    .extend_issued_compiler_support(&support)
                    .unwrap(),
            )
        };
        let original = context(1);
        let child = context(2);
        let captured = capture_test_value(&original, demands_epoch);
        let owner = tidepool_repr::SessionModule::val(tidepool_repr::Generation(1));
        let epoch_ids = original.artifact_view().artifact_ids();
        if !demands_epoch {
            assert!(epoch_ids.iter().all(|id| {
                !captured
                    .compiler_artifact_view()
                    .artifact_ids()
                    .contains(id)
            }));
        }
        drop(original);
        let attached = child
            .as_ref()
            .clone()
            .extend_retained_value_artifacts(std::slice::from_ref(&captured));
        if demands_epoch {
            assert!(
                matches!(attached, Err(CompileError::ArtifactInventory(error))
                if matches!(error.failure, crate::artifact_inventory::ArtifactInventoryFailure::OwnerConflict { .. }))
            );
        } else {
            let attached = attached.unwrap();
            assert!(attached
                .artifact_view()
                .descriptors()
                .iter()
                .any(|row| row.owner.module == owner.module_name()));
            assert!(captured
                .compiler_input_projection()
                .roles()
                .iter()
                .all(|role| !epoch_ids.contains(&role.interface())));
        }
    }

    #[test]
    fn checked_value_capture_preserves_selected_child_over_relevant_historical_offer() {
        checked_value_custody_histories(|_, _, _| Ok(()));
    }

    #[test]
    fn checked_baseline_borrows_issued_rows_and_releases_actual_backing() {
        let context = Arc::new(
            crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![]).unwrap(),
        );
        let value = capture_test_value(&context, false);
        let sibling = capture_test_value_from_output_for_generation(&context, false, false, 2);
        let sibling_path = sibling.interface.path.clone();
        let path = value.interface.path.clone();
        let endpoint = value.row.checked_value_transport().endpoint().to_owned();
        let row = Arc::downgrade(&value.row);
        assert_eq!(
            value.row.checked_value_transport().arena_len(),
            (value.bytes_owned().len()
                + value
                    .certified_interface()
                    .interface()
                    .package_imports_bytes()
                    .len()) as u64
        );
        let inputs = CheckedValueInputs::capture_checked(
            vec![(value.owner(), value.bytes_owned().clone())],
            &[value.clone()],
        )
        .unwrap();
        assert_eq!(inputs.baseline[0].input().path, path);
        assert!(Arc::ptr_eq(
            &inputs.certified_artifacts()[0].row,
            &value.row
        ));
        assert!(!inputs
            .root()
            .join(value.owner().relative_hi_path())
            .exists());
        assert_eq!(inputs.work().initial_files_written, 0);
        assert_eq!(inputs.work().initial_bytes_written_and_hashed, 0);
        let authority = inputs.import_authority();
        let expected = value.bytes_owned().clone();
        std::fs::write(&path, b"changed diagnostic alias").unwrap();
        authority.validate().unwrap();
        assert!(std::fs::read(&endpoint).unwrap().starts_with(&expected));
        let diagnostics = tempfile::tempdir().unwrap();
        inputs.retain_diagnostics(diagnostics.path()).unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(diagnostics.path().join("checked-value-inputs/inputs.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest[0]["observed_source"], "alias_file");
        assert!(manifest[0]["selected_transport"].is_object());
        assert_ne!(
            manifest[0]["expected_sha256"],
            manifest[0]["observed_sha256"]
        );
        drop(context);
        drop(sibling);
        assert!(
            !sibling_path.exists(),
            "an unrelated value has independent physical backing"
        );
        drop(value);
        assert!(row.upgrade().is_some());
        assert!(path.exists());
        assert!(!std::fs::read(&endpoint).unwrap().is_empty());
        authority.validate().unwrap();
        drop(inputs);
        assert!(path.exists(), "the import borrower retains its exact row");
        drop(authority);
        assert!(row.upgrade().is_none());
        assert!(!path.exists());
        assert!(
            !endpoint.exists(),
            "last borrower closes the actual arena FD"
        );
    }

    #[test]
    fn original_value_materialization_is_dedicated_and_reused_after_sibling_drop() {
        use super::*;
        let producer_bytes = b"checked value broad sibling";
        let producer = Sha256::digest(producer_bytes).into();
        let original = Arc::new(
            crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![]).unwrap(),
        );
        let (inputs, owner, path) = prepare_test_value_interface_files(1, false);
        let produced = capture_test_produced_value_interface(producer, owner, &path, 0);
        let (sibling_inputs, sibling_owner, sibling_path) =
            prepare_test_value_interface_files(2, false);
        let sibling_bytes = vec![b'S'; 1024 * 1024];
        std::fs::write(&sibling_path, &sibling_bytes).unwrap();
        let packages = array([
            text("TPPKGROOTS"),
            text("2"),
            array([
                text("main"),
                text(sibling_owner.module_name()),
                text(hash(&sibling_bytes)),
            ]),
            array([]),
            array([]),
        ]);
        let mut encoded = Vec::new();
        ciborium::ser::into_writer(&packages, &mut encoded).unwrap();
        std::fs::write(sibling_path.with_extension("hi.packages"), encoded).unwrap();
        let sibling =
            capture_test_produced_value_interface(producer, sibling_owner, &sibling_path, 1);
        let context = Arc::new(
            (*original)
                .clone()
                .extend_program_value_interface(produced.output(owner).unwrap().interface.clone())
                .unwrap()
                .extend_program_value_interface(
                    sibling.output(sibling_owner).unwrap().interface.clone(),
                )
                .unwrap(),
        );
        let scratch = tempfile::tempdir().unwrap();
        let broad = context
            .prepare_compilation(&scratch.path().join("broad"), producer_bytes)
            .unwrap();
        let broad_value = broad
            .artifacts
            .iter()
            .find(|row| row.interface.module == owner.module_name())
            .unwrap()
            .interface
            .path
            .clone();
        let broad_sibling = broad
            .artifacts
            .iter()
            .find(|row| row.interface.module == sibling_owner.module_name())
            .unwrap()
            .interface
            .path
            .clone();
        let broad_files = {
            let files = broad.input_transport_files();
            files.iter().map(Arc::downgrade).collect::<Vec<_>>()
        };
        assert_eq!(
            broad_files.len(),
            2,
            "two value rows have independent backing descriptors"
        );
        let full_context = context;
        let selected = full_context
            .compiler_source_support()
            .select_value_interface(produced.output(owner).unwrap().interface.as_ref())
            .unwrap();
        let context = Arc::new(
            crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_issued_compiler_support(&selected)
                .unwrap(),
        );
        drop(selected);
        let before = context
            .prepare_compilation(&scratch.path().join("narrow-before"), producer_bytes)
            .unwrap();
        assert_eq!(
            before.input_transport_files().len(),
            1,
            "V-only source owns only its own descriptor"
        );
        let support =
            crate::declaration_context::PreparedValueSourceSupport::new(context.clone(), &[])
                .unwrap();
        let cell = ExactCheckedCell {
            specification: CheckedCellSpecification::Finite(fixture_request(
                String::new(),
                String::new(),
                vec![],
                vec![],
            )),
            producer,
            context: original.semantic_sha256(),
            declaration_context: original.clone(),
            retained_projections: vec![],
            receipt_digest: [4; 32],
            checked_source: "".into(),
            evidence: vec![],
            observations: vec![].into(),
            items: vec![].into(),
            include: vec![],
            planned_declaration: None,
            planned_declarations: BTreeMap::new(),
            typed_segments: vec![],
            value_inputs: inputs.clone(),
        };
        let value = inputs
            .capture_value_data(
                1,
                &cell,
                &context,
                &[],
                None,
                CheckedValueCaptureInput::Prepared {
                    support: &support,
                    produced: &produced,
                },
            )
            .unwrap();
        assert_eq!(
            value.interface.path, broad_value,
            "P6 borrows the original dedicated row"
        );
        assert_eq!(produced.output(owner).unwrap().row.promotion_count(), 0);
        let leaf_path = value.interface.path.clone();
        let overlaid = context
            .prepare_compilation(&scratch.path().join("same-view"), producer_bytes)
            .unwrap();
        assert_eq!(
            overlaid
                .artifacts
                .iter()
                .find(|row| row.interface.module == owner.module_name())
                .unwrap()
                .interface
                .path,
            leaf_path,
            "existing complete cache must overlay its own new leaf"
        );
        assert_eq!(
            broad
                .artifacts
                .iter()
                .find(|row| row.interface.module == owner.module_name())
                .unwrap()
                .interface
                .path,
            broad_value,
            "already issued requests keep their original complete owner"
        );
        drop(broad);
        drop(full_context);
        drop(support);
        drop(produced);
        drop(sibling);
        drop(sibling_inputs);
        drop(cell);
        drop(inputs);
        drop(original);
        assert!(leaf_path.exists());
        assert!(broad_value.exists());
        assert!(!broad_sibling.exists());
        let survivor_file = value.row.checked_value_transport().compiler_file_lease();
        assert!(
            broad_files.iter().all(|file| file
                .upgrade()
                .is_none_or(|file| Arc::ptr_eq(&file, &survivor_file))),
            "only the original dedicated V descriptor survives sibling drop"
        );
        drop(survivor_file);
        let next = Arc::new(
            crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_retained_value_artifacts(&[value.clone()])
                .unwrap(),
        );
        let request = next
            .prepare_compilation(&scratch.path().join("next"), producer_bytes)
            .unwrap();
        assert_eq!(
            request
                .artifacts
                .iter()
                .find(|row| row.interface.module == owner.module_name())
                .unwrap()
                .interface
                .path,
            leaf_path
        );
        drop(request);
        drop(next);
        drop(before);
        drop(overlaid);
        drop(context);
        drop(value);
        assert!(!leaf_path.exists());
    }

    #[test]
    fn direct_complete_promotion_reuses_owner_without_fact_detach() {
        use super::*;
        use crate::declaration_context::RetainedArtifactMaterialization;
        let producer_bytes = b"direct generic value row";
        let producer = Sha256::digest(producer_bytes).into();
        let original = Arc::new(
            crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![]).unwrap(),
        );
        let (inputs, owner, path) = prepare_test_value_interface_files(1, false);
        let produced = capture_test_produced_value_interface(producer, owner, &path, 0);
        let context = Arc::new(
            (*original)
                .clone()
                .extend_program_value_interface(produced.output(owner).unwrap().interface.clone())
                .unwrap(),
        );
        let scratch = tempfile::tempdir().unwrap();
        let before_request = context
            .prepare_compilation(&scratch.path().join("before"), producer_bytes)
            .unwrap();
        let metadata = context.compiler_metadata_snapshot().unwrap();
        let before = context
            .artifact_view()
            .retained_materialization(&metadata)
            .unwrap()
            .unwrap();
        let detaches = RetainedArtifactMaterialization::test_fact_detach_count();
        let support =
            crate::declaration_context::PreparedValueSourceSupport::new(context.clone(), &[])
                .unwrap();
        let cell = empty_test_checked_cell(producer, original.clone(), inputs.clone());
        let value = inputs
            .capture_value_data(
                1,
                &cell,
                &context,
                &[],
                None,
                CheckedValueCaptureInput::Prepared {
                    support: &support,
                    produced: &produced,
                },
            )
            .unwrap();
        assert_eq!(
            value.interface.path,
            before_request.artifacts[0].interface.path
        );
        assert_eq!(produced.output(owner).unwrap().row.promotion_count(), 0);
        for index in 0..4 {
            let retained = context
                .artifact_view()
                .retained_materialization(&metadata)
                .unwrap()
                .unwrap();
            assert!(Arc::ptr_eq(&before, &retained));
            let request = context
                .prepare_compilation(
                    &scratch.path().join(format!("after-{index}")),
                    producer_bytes,
                )
                .unwrap();
            assert_eq!(request.artifacts[0].interface.path, value.interface.path);
            let after = context
                .artifact_view()
                .retained_materialization(&metadata)
                .unwrap()
                .unwrap();
            assert!(Arc::ptr_eq(&before, &after));
            assert_eq!(
                RetainedArtifactMaterialization::test_fact_detach_count(),
                detaches
            );
        }
    }

    #[test]
    fn same_view_cold_materialization_borrows_its_own_produced_leaf() {
        use super::*;
        let producer_bytes = b"same view cold value";
        let producer = Sha256::digest(producer_bytes).into();
        let original = Arc::new(
            crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![]).unwrap(),
        );
        let (inputs, owner, path) = prepare_test_value_interface_files(1, false);
        let produced = capture_test_produced_value_interface(producer, owner, &path, 0);
        let context = Arc::new(
            (*original)
                .clone()
                .extend_program_value_interface(produced.output(owner).unwrap().interface.clone())
                .unwrap(),
        );
        let support =
            crate::declaration_context::PreparedValueSourceSupport::new(context.clone(), &[])
                .unwrap();
        let cell = empty_test_checked_cell(producer, original.clone(), inputs.clone());
        let value = inputs
            .capture_value_data(
                1,
                &cell,
                &context,
                &[],
                None,
                CheckedValueCaptureInput::Prepared {
                    support: &support,
                    produced: &produced,
                },
            )
            .unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let first = context
            .prepare_compilation(&scratch.path().join("first"), producer_bytes)
            .unwrap();
        let second = context
            .prepare_compilation(&scratch.path().join("second"), producer_bytes)
            .unwrap();
        for request in [&first, &second] {
            assert_eq!(
                request
                    .artifacts
                    .iter()
                    .find(|row| row.interface.module == owner.module_name())
                    .unwrap()
                    .interface
                    .path,
                value.interface.path
            );
        }
        assert_eq!(produced.output(owner).unwrap().row.promotion_count(), 1);
    }

    #[test]
    fn same_view_many_promotions_keep_one_current_physical_frontier() {
        use super::*;
        let producer_bytes = b"many same-view promoted values";
        let producer = Sha256::digest(producer_bytes).into();
        let original = Arc::new(
            crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![]).unwrap(),
        );
        let mut staged = Vec::new();
        let mut selected = (*original).clone();
        for generation in 1..=16 {
            let (inputs, owner, path) = prepare_test_value_interface_files(generation, false);
            let produced = capture_test_produced_value_interface(
                producer,
                owner,
                &path,
                generation as usize - 1,
            );
            selected = selected
                .extend_program_value_interface(produced.output(owner).unwrap().interface.clone())
                .unwrap();
            staged.push((inputs, owner, produced));
        }
        let context = Arc::new(selected);
        let support =
            crate::declaration_context::PreparedValueSourceSupport::new(context.clone(), &[])
                .unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let escaped = staged
            .iter()
            .enumerate()
            .map(|(index, (inputs, _, produced))| {
                let cell = empty_test_checked_cell(producer, original.clone(), inputs.clone());
                inputs
                    .capture_value_data(
                        index as u64 + 1,
                        &cell,
                        &original,
                        &[],
                        None,
                        CheckedValueCaptureInput::Ordinary {
                            produced: Some(produced),
                        },
                    )
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let base = context
            .prepare_compilation(&scratch.path().join("base"), producer_bytes)
            .unwrap();
        let old_path = base.artifacts[0].interface.path.clone();
        let old_files = {
            let files = base.input_transport_files();
            files.iter().map(Arc::downgrade).collect::<Vec<_>>()
        };
        let mut current = None;
        let mut values = Vec::new();
        for (index, (inputs, owner, produced)) in staged.iter().enumerate() {
            let cell = empty_test_checked_cell(producer, original.clone(), inputs.clone());
            let value = inputs
                .capture_value_data(
                    index as u64 + 1,
                    &cell,
                    &context,
                    &[],
                    None,
                    CheckedValueCaptureInput::Prepared {
                        support: &support,
                        produced,
                    },
                )
                .unwrap();
            let request = context
                .prepare_compilation(
                    &scratch.path().join(format!("step-{index}")),
                    producer_bytes,
                )
                .unwrap();
            let physical = context
                .artifact_view()
                .retained_materialization(&context.compiler_metadata_snapshot().unwrap())
                .unwrap()
                .unwrap();
            assert_eq!(
                physical.test_ownership_shape(),
                (1, 16),
                "current overlay owns linear facts with no predecessor graph"
            );
            assert_eq!(
                request
                    .artifacts
                    .iter()
                    .find(|row| row.interface.module == owner.module_name())
                    .unwrap()
                    .interface
                    .path,
                value.interface.path
            );
            assert_eq!(base.artifacts[0].interface.path, old_path);
            assert!(
                old_path.exists(),
                "original in-flight request retains its original bytes"
            );
            current = Some(request);
            values.push(value);
        }
        drop(base);
        assert!(!old_path.exists());
        assert!(old_files.iter().all(|file| file.upgrade().is_none()));
        assert!(current
            .as_ref()
            .unwrap()
            .artifacts
            .iter()
            .all(|row| row.interface.path.exists()));
        assert_eq!(values.len(), 16);
        assert_eq!(escaped.len(), 16);
    }

    #[test]
    fn concurrent_produced_value_capture_shares_one_promoted_row() {
        use super::*;
        let original = Arc::new(
            crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![]).unwrap(),
        );
        let (inputs, owner, path) = prepare_test_value_interface_files(1, false);
        let produced = capture_test_produced_value_interface([2; 32], owner, &path, 0);
        let context = Arc::new(
            (*original)
                .clone()
                .extend_program_value_interface(produced.output(owner).unwrap().interface.clone())
                .unwrap(),
        );
        let prepared =
            crate::declaration_context::PreparedValueSourceSupport::new(context.clone(), &[])
                .unwrap();
        let cell = empty_test_checked_cell([2; 32], original.clone(), inputs.clone());
        let barrier = std::sync::Barrier::new(2);
        let values = std::thread::scope(|scope| {
            let ordinary = scope.spawn(|| {
                barrier.wait();
                inputs
                    .capture_value_data(
                        1,
                        &cell,
                        &original,
                        &[],
                        None,
                        CheckedValueCaptureInput::Ordinary {
                            produced: Some(&produced),
                        },
                    )
                    .unwrap()
            });
            let typed = scope.spawn(|| {
                barrier.wait();
                inputs
                    .capture_value_data(
                        1,
                        &cell,
                        &context,
                        &[],
                        None,
                        CheckedValueCaptureInput::Prepared {
                            support: &prepared,
                            produced: &produced,
                        },
                    )
                    .unwrap()
            });
            [ordinary.join().unwrap(), typed.join().unwrap()]
        });
        assert!(Arc::ptr_eq(&values[0].row, &values[1].row));
        assert_eq!(values[0], values[1]);
        assert_eq!(produced.output(owner).unwrap().row.promotion_count(), 1);
    }

    #[test]
    fn produced_value_promotion_refuses_a_context_selecting_another_output() {
        use super::*;
        let original = Arc::new(
            crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![]).unwrap(),
        );
        let (inputs, owner, path) = prepare_test_value_interface_files(1, false);
        let produced = capture_test_produced_value_interface([2; 32], owner, &path, 0);
        let (_other_inputs, other_owner, other_path) = prepare_test_value_interface_files(2, false);
        let other = capture_test_produced_value_interface([2; 32], other_owner, &other_path, 0);
        let context = Arc::new(
            (*original)
                .clone()
                .extend_program_value_interface(
                    other.output(other_owner).unwrap().interface.clone(),
                )
                .unwrap(),
        );
        let support =
            crate::declaration_context::PreparedValueSourceSupport::new(context.clone(), &[])
                .unwrap();
        let cell = empty_test_checked_cell([2; 32], original.clone(), inputs.clone());
        assert!(inputs
            .capture_value_data(
                1,
                &cell,
                &context,
                &[],
                None,
                CheckedValueCaptureInput::Prepared {
                    support: &support,
                    produced: &produced
                }
            )
            .is_err());
        assert_eq!(produced.output(owner).unwrap().row.promotion_count(), 0);
        let value = inputs
            .capture_value_data(
                1,
                &cell,
                &original,
                &[],
                None,
                CheckedValueCaptureInput::Ordinary {
                    produced: Some(&produced),
                },
            )
            .unwrap();
        assert_eq!(value.owner(), owner);
        assert_eq!(produced.output(owner).unwrap().row.promotion_count(), 1);
    }

    #[test]
    fn cancelled_produced_value_capture_leaves_promotion_empty_and_retries() {
        use super::*;
        use tidepool_extract_cmd::{
            with_compiler_transaction_cancellable, CompilerTransactionCancellation,
        };
        let original = Arc::new(
            crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![]).unwrap(),
        );
        let (inputs, owner, path) = prepare_test_value_interface_files(1, false);
        let produced = capture_test_produced_value_interface([2; 32], owner, &path, 0);
        let cell = empty_test_checked_cell([2; 32], original.clone(), inputs.clone());
        let cancellation = CompilerTransactionCancellation::new();
        cancellation.cancel();
        let refused = with_compiler_transaction_cancellable(
            cancellation,
            |_| {},
            || {
                inputs.capture_value_data(
                    1,
                    &cell,
                    &original,
                    &[],
                    None,
                    CheckedValueCaptureInput::Ordinary {
                        produced: Some(&produced),
                    },
                )
            },
        );
        assert!(refused.action.is_err());
        assert_eq!(produced.output(owner).unwrap().row.promotion_count(), 0);
        let value = inputs
            .capture_value_data(
                1,
                &cell,
                &original,
                &[],
                None,
                CheckedValueCaptureInput::Ordinary {
                    produced: Some(&produced),
                },
            )
            .unwrap();
        assert_eq!(produced.output(owner).unwrap().row.promotion_count(), 1);
        assert!(value.interface.path.exists());
    }

    #[test]
    fn checked_baseline_keeps_distinct_equal_byte_issuers_and_refuses_mismatch() {
        let context = Arc::new(
            crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![]).unwrap(),
        );
        let first = capture_test_value(&context, false);
        let second = capture_test_value(&context, false);
        assert_eq!(first.bytes_owned(), second.bytes_owned());
        assert_ne!(
            first, second,
            "independent physical issuers cannot substitute for a retained value"
        );
        assert!(CheckedValueInputs::capture_checked(
            vec![(first.owner(), first.bytes_owned().clone())],
            &[first.clone(), second]
        )
        .is_err());
        assert!(CheckedValueInputs::capture_checked(
            vec![(first.owner(), Arc::from(&b"wrong"[..]))],
            &[first]
        )
        .is_err());
    }

    #[test]
    fn checked_value_support_materializes_with_one_survivor_after_issuer_drop() {
        let mut histories = capture_value_custody_histories();
        // The final matrix case retains the changed child issuer.
        // Sibling captures must not supply the survivor's physical byte custody.
        let (captured, expected_native, expected_child) = histories.pop().unwrap();
        assert!(captured.template_imports().is_none());
        drop(histories);
        let receiving = Arc::new(
            crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_retained_value_artifacts(&[captured.clone()])
                .unwrap(),
        );
        assert!(receiving
            .artifact_view()
            .capture_graph_selection()
            .namespace
            .contains(&expected_child));
        let scratch = tempfile::tempdir().unwrap();
        let request = receiving
            .prepare_compilation(&scratch.path().join("next"), VALUE_CUSTODY_PRODUCER)
            .unwrap();
        let epoch = request
            .artifacts
            .iter()
            .find(|artifact| {
                artifact.interface.unit == "fixture" && artifact.interface.module == "Epoch"
            })
            .expect("next compiler request owns its required original");
        let body = epoch
            .product
            .as_ref()
            .expect("required original has native bytes");
        assert_eq!(std::fs::read(&body.path).unwrap(), expected_native);
        let owned_path = body.path.clone();
        drop(request);
        drop(receiving);
        drop(captured);
        assert!(
            !owned_path.try_exists().unwrap(),
            "the last selected value/request owner releases its private materialization"
        );
    }

    #[test]
    fn program_value_admission_preserves_unselected_native_offer_custody() {
        use super::*;
        use crate::artifact_inventory::ArtifactBindingNode;
        use crate::declaration_context::ExactDeclarationContext;
        use crate::declaration_join::ExactModuleIdentity;

        checked_value_custody_histories(|captured, _, _| {
            let certificate = captured.certified_interface().clone();
            let value_id = certificate.artifact_id();
            let value_owner = ExactModuleIdentity {
                unit: certificate.interface().unit().into(),
                module: certificate.interface().module().into(),
            };
            let source = captured.compiler_source_support();
            let issued_offers = source.native_offer_observations().unwrap();
            assert!(!issued_offers.is_empty());
            assert!(source.native_offers().iter().any(|offer| offer.is_full()));
            let support = source
                .without_value_roots(&BTreeSet::from([value_id]))
                .map_err(|error| format!("remove captured value premise: {error}"))?
                .for_source_owners(&BTreeSet::new())
                .map_err(|error| format!("type-only source premise: {error}"))?
                .finalize_namespace()
                .map_err(|error| format!("selected type namespace premise: {error}"))?;
            assert_eq!(support.native_offer_observations().unwrap(), issued_offers);
            assert!(support
                .projection()
                .roles()
                .iter()
                .all(|role| role.original().is_none()));
            assert!(!support
                .projection()
                .snapshot_roles()
                .contains_key(&value_owner));
            let context = ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_issued_compiler_support(&support)
                .map_err(|error| format!("paired receiving context premise: {error}"))?;
            let before = context.artifact_view().capture_graph_selection();
            let before_edges = context.artifact_view().binding_dependencies();
            let before_roles = context.compiler_input_projection().snapshot_roles().clone();
            let before_offers = context
                .compiler_source_support()
                .native_offer_observations()
                .unwrap();
            assert_eq!(before_offers, issued_offers);
            assert!(context.lexical_graph().is_empty());
            assert!(context.artifact_view().selected_native_groups().is_empty());
            assert!(!context.artifact_view().artifact_ids().contains(&value_id));
            for offer in context.compiler_source_support().native_offers() {
                assert!(
                    !context
                        .compiler_input_projection()
                        .roles()
                        .iter()
                        .any(|role| role.original() == Some(offer.original())),
                    "retained offers do not require active original roles"
                );
                assert!(context
                    .artifact_view()
                    .artifact_ids()
                    .contains(&offer.original()));
            }

            let admitted = context
                .clone()
                .extend_program_value_interface(certificate)
                .map_err(|error| format!("actual program value admission: {error}"))?;
            let after = admitted.artifact_view().capture_graph_selection();
            let after_roles = admitted.compiler_input_projection().snapshot_roles();
            for (owner, role) in &before_roles {
                assert_eq!(after_roles.get(owner), Some(role));
            }
            assert_eq!(after_roles.len(), before_roles.len() + 1);
            assert_eq!(after_roles[&value_owner].interface(), value_id);
            assert!(after_roles.values().all(|role| role.original().is_none()));
            for binding in &before.namespace {
                assert!(
                    after.namespace.contains(binding),
                    "selected canonical bindings retain their issuers"
                );
            }
            assert_eq!(after.native_groups, before.native_groups);
            assert!(before
                .bindings
                .iter()
                .all(|binding| after.bindings.contains(binding)));
            assert!(before_edges.iter().all(|edge| admitted
                .artifact_view()
                .binding_dependencies()
                .contains(edge)));
            assert_eq!(
                admitted
                    .compiler_source_support()
                    .native_offer_observations()
                    .unwrap(),
                before_offers
            );
            assert!(admitted.artifact_view().selected_native_groups().is_empty());
            let old_roots = before.roots.iter().copied().collect::<BTreeSet<_>>();
            let new_roots = after.roots.iter().copied().collect::<BTreeSet<_>>();
            assert!(old_roots.is_subset(&new_roots));
            let value_binding = *after
                .namespace
                .iter()
                .find(|binding| binding.artifact == value_id)
                .unwrap();
            assert_eq!(
                new_roots
                    .difference(&old_roots)
                    .copied()
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from([ArtifactBindingNode::Artifact(value_binding)])
            );
            assert_eq!(admitted.lexical_graph().len(), 1);
            assert_eq!(admitted.lexical_graph()[0].owner, value_owner);
            assert!(admitted.lexical_graph()[0].imports.is_empty());
            assert_eq!(context.artifact_view().capture_graph_selection(), before);
            assert_eq!(source.native_offer_observations().unwrap(), issued_offers);
            Ok(())
        });
    }

    #[test]
    fn authored_lifetime_query_ignores_unrelated_historical_owner_conflicts() {
        checked_value_custody_histories(|_, context, _| {
            let before = context.artifact_view().capture_graph_selection();
            let roles = context.compiler_input_projection().snapshot_roles();
            let requirements = context
                .authored_native_binding_custody_requirements(&std::collections::BTreeSet::from([
                    crate::declaration_join::ExactModuleIdentity {
                        unit: "fixture".into(),
                        module: "Epoch".into(),
                    },
                ]))
                .map_err(|error| error.to_string())?;
            assert!(
                requirements.is_empty(),
                "source originals issue no authored lifetime edges"
            );
            assert_eq!(context.artifact_view().capture_graph_selection(), before);
            assert_eq!(context.compiler_input_projection().snapshot_roles(), roles);
            Ok(())
        });
    }

    #[test]
    fn authored_lifetime_query_follows_issued_unselected_value_edges() {
        checked_value_custody_histories(|captured, context, _| {
            check_authored_lifetime_graph(captured, context, false)
        });
    }

    #[test]
    fn authored_lifetime_query_traverses_issued_cycles_and_shared_groups() {
        checked_value_custody_histories(|captured, context, _| {
            check_authored_lifetime_graph(captured, context, true)
        });
    }

    fn check_authored_lifetime_graph(
        captured: &Arc<super::CheckedValueArtifact>,
        context: &crate::declaration_context::ExactDeclarationContext,
        cyclic: bool,
    ) -> Result<(), String> {
        use super::*;
        use crate::artifact_inventory::{
            ArtifactBindingNode, ArtifactDependency, ArtifactEntry, ArtifactInventory,
            CompilerInputProjection, IssuedNativeOffer, NativeBindingRequirement,
            NativeOriginalRequirement,
        };
        use crate::certified_products::PendingImportOwner;
        use crate::declaration_join::ExactModuleIdentity;

        let producer = captured
            .certified_interface()
            .interface()
            .toolchain_identity_sha256();
        let value_id = captured.certified_interface().artifact_id();
        let identity = tidepool_repr::execution_schema::SymbolIdentity {
            unit: "main".into(),
            module: captured.owner().module_name(),
            namespace: "value".into(),
            occurrence: "captured".into(),
            record_parent: None,
        };
        let retained = || PendingImportOwner::Retained {
            identity: identity.clone(),
            generation: captured.owner().gen.0,
        };
        let authored_module =
            tidepool_repr::SessionModule::lib(tidepool_repr::Generation(2)).module_name();
        let raw = |module: &str, groups| {
            crate::certified_products::tests::original_groups_fixture_in_unit_with_sites(
                "main",
                module,
                groups,
                81,
                &BTreeMap::new(),
                format!("{module} interface").into_bytes(),
                &BTreeMap::new(),
                &[],
            )
        };
        let source = |owner: tidepool_repr::execution_schema::CachedHomeOwner, ordinal| {
            PendingImportOwner::Source {
                binder: tidepool_repr::execution_schema::SymbolIdentity {
                    unit: owner.unit.clone(),
                    module: owner.module.clone(),
                    namespace: "value".into(),
                    occurrence: format!("entry_{ordinal}"),
                    record_parent: None,
                },
                owner,
                original_ordinal: ordinal,
            }
        };
        let (product, helper) = if cyclic {
            // The owner's existing cyclic-certification fixture uses this same
            // provisional-owner construction. Final certificates authenticate
            // both complete products together; no inventory edge is fabricated.
            let placeholder = raw("LifetimeHelper", vec![(7, vec![]), (29, vec![])]);
            let authored_groups = |helper: tidepool_repr::execution_schema::CachedHomeOwner| {
                vec![
                    (7, vec![]),
                    (11, vec![source(helper.clone(), 7)]),
                    (13, vec![source(helper, 7)]),
                ]
            };
            let first = raw(
                &authored_module,
                authored_groups(placeholder.owner().clone()),
            );
            let helper = raw(
                "LifetimeHelper",
                vec![
                    (7, vec![source(first.owner().clone(), 11), retained()]),
                    (29, vec![]),
                ],
            );
            let final_authored = raw(&authored_module, authored_groups(helper.owner().clone()));
            assert_eq!(
                first.owner(),
                final_authored.owner(),
                "the final owner records the same immutable authored product"
            );
            (
                final_authored,
                Some(crate::certified_products::fixture_finalized_product(
                    helper, producer,
                )),
            )
        } else {
            (
                raw(
                    &authored_module,
                    vec![(7, vec![]), (11, vec![retained()]), (13, vec![retained()])],
                ),
                None,
            )
        };
        let product =
            crate::certified_products::fixture_native_declaration_product(product, producer, 2);
        let mut products = vec![product];
        products.extend(helper);
        let recovered = crate::certified_products::tests::recovered_witness_fixtures(&products);
        let product = recovered
            .iter()
            .find(|row| row.product.owner().module == authored_module)
            .unwrap()
            .product
            .clone();
        let helper = recovered
            .iter()
            .find(|row| row.product.owner().module == "LifetimeHelper")
            .map(|row| Arc::new(ArtifactEntry::original(producer, row.product.clone()).unwrap()));
        let canonical = ArtifactEntry::canonical(product.module_interface().unwrap().clone())
            .descriptor
            .id;
        let authored = Arc::new(ArtifactEntry::original(producer, product).unwrap());
        let authored_id = authored.descriptor.id;
        let accepted = BTreeSet::from([ExactModuleIdentity {
            unit: "main".into(),
            module: authored_module,
        }]);
        // A fresh issuance consumes the captured owner's selected interface rows.
        // Historical custody does not choose any of the new issuer's dependencies.
        let metadata = captured.compiler_artifact_view().metadata_snapshot();
        let mut entries = captured
            .compiler_input_projection()
            .roles()
            .iter()
            .map(|role| metadata.artifacts[&role.interface()].clone())
            .collect::<Vec<_>>();
        assert!(entries.iter().any(|entry| entry.descriptor.id == value_id));
        entries.push(authored.clone());
        entries.extend(helper.iter().cloned());
        let inventory = ArtifactInventory::default();
        let full = inventory
            .admit_shared(&inventory.empty_view(), entries.clone())
            .unwrap();
        let proof =
            NativeOriginalRequirement::from_issued_group_custody(authored_id, canonical, &full)
                .unwrap()
                .expect("the owner issued all authored groups");
        let full_offer = IssuedNativeOffer::issue(authored_id, &full, Some(proof)).unwrap();
        assert!(full_offer.is_full());
        let empty_offer = IssuedNativeOffer::issue(authored_id, &full, None).unwrap();
        let authored_role = CompilerInputProjection::from_issued_entries(&[authored]).unwrap();
        let partial_offer = full_offer
            .within_projection(&authored_role, context.artifact_view())
            .unwrap()
            .expect("the same original role preserves its issued offer");
        assert!(!partial_offer.is_full());
        assert!(matches!(
            partial_offer.snapshot_facts().body,
            crate::exact_context::NativeOfferSnapshotBody::Partial(_)
        ));

        let sparse = inventory
            .admit_recovery_selection(
                &inventory.empty_view(),
                entries,
                &BTreeSet::from([crate::artifact_inventory::NativeGroupKey {
                    artifact: authored_id,
                    original_ordinal: 7,
                }]),
            )
            .unwrap();
        let sparse_proof = NativeOriginalRequirement::from_selected_view(authored_id, &sparse)
            .unwrap()
            .expect("the owner issued the actual sparse initial group");
        let sparse_offer =
            IssuedNativeOffer::issue(authored_id, &sparse, Some(sparse_proof)).unwrap();
        assert!(matches!(
            sparse_offer.snapshot_facts().body,
            crate::exact_context::NativeOfferSnapshotBody::Partial(_)
        ));
        assert!(!sparse_offer.is_full());

        let expected = vec![NativeBindingRequirement {
            artifact_id: value_id,
            identity: identity.clone(),
            generation: captured.owner().gen.0,
        }];
        let issued_edges = full.binding_dependencies();
        let binding_source = helper
            .as_ref()
            .map_or(authored_id, |entry| entry.descriptor.id);
        let ordinals: &[u32] = if cyclic { &[7] } else { &[11, 13] };
        for &ordinal in ordinals {
            let group = full
                .capture_graph_selection()
                .native_groups
                .into_iter()
                .find(|group| {
                    group.binding.artifact() == binding_source && group.original_ordinal == ordinal
                })
                .unwrap();
            let source = ArtifactBindingNode::Group(group);
            assert!(issued_edges.iter().any(|(from, to, edge)| *from == source
                && matches!(to, ArtifactBindingNode::Artifact(binding)
                        if binding.artifact == binding_source)
                && *edge == ArtifactDependency::Interface));
            assert!(issued_edges.iter().any(|(from, to, edge)| *from == source
                && matches!(to, ArtifactBindingNode::Artifact(binding)
                        if binding.artifact == value_id)
                && matches!(edge, ArtifactDependency::NativeBinding {
                            generation, namespace, occurrence, record_parent, ..
                        } if *generation == captured.owner().gen.0
                            && namespace == &identity.namespace
                            && occurrence == &identity.occurrence
                            && record_parent == &identity.record_parent)));
        }
        if cyclic {
            let groups = full.capture_graph_selection().native_groups;
            let group = |id, ordinal| {
                *groups
                    .iter()
                    .find(|group| {
                        group.binding.artifact() == id && group.original_ordinal == ordinal
                    })
                    .unwrap()
            };
            for (from, to) in [
                (group(authored_id, 11), group(binding_source, 7)),
                (group(authored_id, 13), group(binding_source, 7)),
                (group(binding_source, 7), group(authored_id, 11)),
            ] {
                assert!(issued_edges.contains(&(
                    ArtifactBindingNode::Group(from),
                    ArtifactBindingNode::Group(to),
                    ArtifactDependency::NativeGroup {
                        dependent_ordinal: from.original_ordinal,
                        required_ordinal: to.original_ordinal,
                    },
                )));
            }
        }
        let expected_groups = [(authored_id, 7), (authored_id, 11), (authored_id, 13)]
            .into_iter()
            .chain(cyclic.then_some((binding_source, 7)))
            .collect::<BTreeSet<_>>();
        for (offer, expected_groups, captures_value) in [
            (empty_offer, BTreeSet::new(), false),
            (sparse_offer, BTreeSet::from([(authored_id, 7)]), false),
            (partial_offer, expected_groups.clone(), true),
            (full_offer, expected_groups, true),
        ] {
            let issuance = offer.snapshot_facts().identity().unwrap();
            let offered_groups = offer
                .issued_group_carriers()
                .unwrap()
                .keys()
                .map(|group| (group.binding.artifact(), group.original_ordinal))
                .collect::<BTreeSet<_>>();
            assert_eq!(offered_groups, expected_groups);
            let offered_edges = offer.custody_snapshot().binding_dependencies().unwrap();
            let support = crate::declaration_context::recover_fixture_compiler_support(
                CompilerInputProjection::default(),
                context.artifact_view().clone(),
                [offer],
            )
            .unwrap();
            let retained = context
                .clone()
                .extend_issued_compiler_support(&support)
                .unwrap();
            let before = retained.artifact_view().capture_graph_selection();
            assert_eq!(
                retained.compiler_input_projection(),
                context.compiler_input_projection()
            );
            assert_eq!(
                before.roots,
                context.artifact_view().capture_graph_selection().roots
            );
            assert!(
                retained
                    .compiler_source_support()
                    .native_offers()
                    .iter()
                    .any(|offer| offer.snapshot_facts().identity().unwrap() == issuance),
                "custody composition preserves the exact offered availability mode and proof"
            );
            let retained_offer = retained
                .compiler_source_support()
                .native_offers()
                .iter()
                .find(|offer| offer.snapshot_facts().identity().unwrap() == issuance)
                .unwrap();
            assert_eq!(
                retained_offer
                    .issued_group_carriers()
                    .unwrap()
                    .keys()
                    .filter(|group| group.binding.artifact() == authored_id
                        || group.binding.artifact() == binding_source)
                    .map(|group| (group.binding.artifact(), group.original_ordinal))
                    .collect::<BTreeSet<_>>(),
                expected_groups
            );
            if !expected_groups.is_empty() {
                for edge in offered_edges
                    .iter()
                    .filter(|(from, _, _)| matches!(from, ArtifactBindingNode::Group(_)))
                {
                    assert!(retained_offer
                        .custody_snapshot()
                        .binding_dependencies()
                        .unwrap()
                        .contains(edge));
                }
            } else {
                assert!(retained_offer.issued_group_carriers().unwrap().is_empty());
                assert!(
                    retained
                        .artifact_view()
                        .artifact_ids()
                        .contains(&authored_id),
                    "immutable authored bytes do not imply issued group custody"
                );
            }
            let requirements = retained
                .authored_native_binding_custody_requirements(&accepted)
                .map_err(|error| error.to_string())?;
            assert_eq!(
                requirements,
                if captures_value {
                    expected.clone()
                } else {
                    vec![]
                }
            );
            assert_eq!(retained.artifact_view().capture_graph_selection(), before);
            assert_eq!(
                retained.compiler_input_projection(),
                context.compiler_input_projection()
            );
        }
        Ok(())
    }

    const VALUE_CUSTODY_PRODUCER: &[u8] = b"checked-value custody history";

    fn checked_value_custody_histories(
        observe: impl Fn(
            &Arc<super::CheckedValueArtifact>,
            &crate::declaration_context::ExactDeclarationContext,
            &[u8],
        ) -> Result<(), String>,
    ) {
        // No issuing context, prepared support, checked cell or fixture product
        // owner escapes the capture helper; only values and expected bytes do.
        for (captured, expected_native, expected_child) in capture_value_custody_histories() {
            let next = crate::declaration_context::ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_retained_value_artifacts(&[captured.clone()])
                .unwrap();
            assert!(
                next.artifact_view()
                    .capture_graph_selection()
                    .namespace
                    .contains(&expected_child),
                "the next consumer preserves its pre-capture selected child issuer"
            );
            observe(&captured, &next, &expected_native).unwrap();
        }
    }

    fn capture_value_custody_histories() -> Vec<(
        Arc<super::CheckedValueArtifact>,
        Vec<u8>,
        crate::artifact_inventory::ArtifactBinding,
    )> {
        capture_value_custody_histories_with_materialized_source(false)
    }

    #[test]
    fn checked_value_leaf_preserves_preexisting_dependency_rows_through_next_request() {
        assert_eq!(
            capture_value_custody_histories_with_materialized_source(true).len(),
            8
        );
    }

    fn capture_value_custody_histories_with_materialized_source(
        materialize_source: bool,
    ) -> Vec<(
        Arc<super::CheckedValueArtifact>,
        Vec<u8>,
        crate::artifact_inventory::ArtifactBinding,
    )> {
        use super::*;
        use crate::artifact_inventory::{
            ArtifactBindingNode, ArtifactDependency, ArtifactEntry, ArtifactInventory,
            ArtifactInventoryFailure, CompilerInputProjection, IssuedNativeOffer,
            NativeOriginalRequirement,
        };
        use crate::declaration_context::{ExactDeclarationContext, PreparedValueSourceSupport};
        use crate::declaration_join::{ExactLexicalNode, ExactModuleIdentity};
        let mut failures = Vec::new();
        let mut histories = Vec::new();
        let producer = Sha256::digest(VALUE_CUSTODY_PRODUCER).into();
        let child_interface = crate::certified_products::fixture_module_interface(
            producer,
            "fixture",
            "Child",
            BTreeMap::new(),
        );
        let native = crate::certified_products::fixture_finalized_product_with_requirements(
            crate::certified_products::tests::original_groups_fixture(
                "Epoch",
                vec![(7, vec![])],
                1,
                &BTreeMap::new(),
            ),
            producer,
            Some(BTreeMap::from([(
                ("fixture".into(), "Child".into()),
                child_interface.interface_sha256(),
            )])),
        );
        let native = crate::certified_products::tests::recovered_witness_fixtures(&[native])
            .remove(0)
            .product;
        let expected_native = native.product_bytes().to_vec();
        let native = Arc::new(ArtifactEntry::original(producer, native).unwrap());
        let epoch = native.descriptor.owner.clone();
        let child_owner = ExactModuleIdentity {
            unit: "fixture".into(),
            module: "Child".into(),
        };
        let lexical = vec![ExactLexicalNode {
            owner: epoch.clone(),
            imports: vec![],
        }];
        let template_only = Arc::new(ArtifactEntry::canonical(
            crate::certified_products::fixture_module_interface(
                producer,
                "fixture",
                "TemplateOnly",
                BTreeMap::new(),
            ),
        ));
        let template_owner = template_only.descriptor.owner.clone();
        let mut original_lexical = lexical.clone();
        original_lexical.push(ExactLexicalNode {
            owner: template_owner.clone(),
            imports: vec![],
        });
        for changed_child in [false, true] {
            let children = [
                Arc::new(ArtifactEntry::canonical(child_interface.clone())),
                Arc::new(ArtifactEntry::canonical(if changed_child {
                    crate::certified_products::fixture_module_core(
                        &child_interface,
                        b"next child Core".to_vec(),
                    )
                } else {
                    child_interface.clone()
                })),
            ];
            assert_eq!(
                children[0].descriptor.interface_sha256,
                children[1].descriptor.interface_sha256
            );
            assert_eq!(
                children[0].descriptor.id != children[1].descriptor.id,
                changed_child
            );
            let views = children
                .iter()
                .enumerate()
                .map(|(index, child)| {
                    let sibling = Arc::new(ArtifactEntry::canonical(
                        crate::certified_products::fixture_module_interface(
                            producer,
                            "fixture",
                            ["Left", "Right"][index],
                            BTreeMap::new(),
                        ),
                    ));
                    let inventory = ArtifactInventory::default();
                    inventory
                        .admit_shared(
                            &inventory.empty_view(),
                            vec![
                                native.clone(),
                                child.clone(),
                                sibling,
                                template_only.clone(),
                            ],
                        )
                        .unwrap()
                })
                .collect::<Vec<_>>();
            let native_binding = |index: usize| {
                views[index]
                    .capture_graph_selection()
                    .namespace
                    .into_iter()
                    .find(|binding| binding.artifact == native.descriptor.id)
                    .unwrap()
            };
            assert_ne!(
                native_binding(0),
                native_binding(1),
                "equal payload is not equal issuer"
            );
            let expected_edges = views
                .iter()
                .flat_map(|view| view.binding_dependencies())
                .filter(|(source, target, _)| {
                    matches!(
                        (source, target),
                        (
                            ArtifactBindingNode::Artifact(_),
                            ArtifactBindingNode::Artifact(_)
                        )
                    )
                })
                .collect::<BTreeSet<_>>();
            let canonical = match &native.payload {
                crate::artifact_inventory::ArtifactPayload::Original(product) => {
                    ArtifactEntry::canonical(product.module_interface().unwrap().clone())
                        .descriptor
                        .id
                }
                _ => unreachable!(),
            };
            // The fixture plan chooses these two exact child edges before any join.
            for (index, view) in views.iter().enumerate() {
                let graph = view.capture_graph_selection();
                let binding = |id| {
                    graph
                        .namespace
                        .iter()
                        .find(|binding| binding.artifact == id)
                        .copied()
                        .unwrap()
                };
                assert!(view.binding_dependencies().contains(&(
                    ArtifactBindingNode::Artifact(binding(canonical)),
                    ArtifactBindingNode::Artifact(binding(children[index].descriptor.id)),
                    ArtifactDependency::Interface,
                )));
            }
            for selected in 0..2 {
                let projection = CompilerInputProjection::from_issued_entries(&[
                    native.clone(),
                    children[selected].clone(),
                    template_only.clone(),
                ])
                .unwrap();
                let selected_child = views[selected]
                    .capture_graph_selection()
                    .namespace
                    .into_iter()
                    .find(|binding| binding.artifact == children[selected].descriptor.id)
                    .unwrap();
                // Historical byte custody grants no competing Group choice.
                let old =
                    IssuedNativeOffer::issue(native.descriptor.id, &views[1 - selected], None)
                        .unwrap();
                let support = crate::declaration_context::recover_fixture_compiler_support(
                    projection.clone(),
                    views[selected].clone(),
                    [old.clone(), old],
                )
                .unwrap();
                let original = ExactDeclarationContext::new(&[], &[], vec![])
                    .unwrap()
                    .extend_issued_compiler_support(&support)
                    .unwrap()
                    .extend(&[], &[], original_lexical.clone())
                    .unwrap();
                let original = Arc::new(original);
                let source_scratch = tempfile::tempdir().unwrap();
                let inherited_path = materialize_source.then(|| {
                    let request = original
                        .prepare_compilation(
                            &source_scratch.path().join("source"),
                            VALUE_CUSTODY_PRODUCER,
                        )
                        .unwrap();
                    request
                        .artifacts
                        .iter()
                        .find(|artifact| artifact.interface.module == "Epoch")
                        .unwrap()
                        .interface
                        .path
                        .clone()
                });
                let narrowed = original
                    .compiler_source_support()
                    .source_support(&BTreeSet::from([epoch.clone()]))
                    .unwrap();
                assert!(children.iter().all(|child| narrowed
                    .artifacts()
                    .artifact_ids()
                    .contains(&child.descriptor.id)));
                assert!(expected_edges
                    .iter()
                    .all(|edge| narrowed.artifacts().binding_dependencies().contains(edge)));
                assert_eq!(
                    narrowed.projection().snapshot_roles()[&child_owner].interface(),
                    children[selected].descriptor.id
                );
                assert_eq!(
                    narrowed
                        .artifacts()
                        .capture_graph_selection()
                        .bindings
                        .iter()
                        .filter(|binding| binding.artifact == native.descriptor.id)
                        .count(),
                    2
                );
                if changed_child {
                    let competing = CompilerInputProjection::from_issued_entries(&[children
                        [1 - selected]
                        .clone()])
                    .unwrap();
                    let competing = crate::declaration_context::recover_fixture_compiler_support(
                        competing,
                        views[1 - selected].clone(),
                        [],
                    )
                    .unwrap();
                    assert!(
                        matches!((*original).clone().extend_issued_compiler_support(&competing), Err(CompileError::ArtifactInventory(error))
                    if matches!(error.failure, ArtifactInventoryFailure::OwnerConflict { .. })),
                        "genuinely differing selected roles must still refuse"
                    );
                }

                let (inputs, owner, path) = prepare_test_value_interface_files(1, true);
                let produced = capture_test_produced_value_interface(producer, owner, &path, 0);
                let interface = produced.output(owner).unwrap().interface.clone();
                let cell = ExactCheckedCell {
                    specification: CheckedCellSpecification::Finite(fixture_request(
                        String::new(),
                        String::new(),
                        vec![],
                        vec![],
                    )),
                    producer,
                    context: original.semantic_sha256(),
                    declaration_context: original.clone(),
                    retained_projections: vec![],
                    receipt_digest: [4; 32],
                    checked_source: "".into(),
                    evidence: vec![],
                    observations: vec![].into(),
                    items: vec![].into(),
                    include: vec![],
                    planned_declaration: None,
                    planned_declarations: BTreeMap::new(),
                    typed_segments: vec![],
                    value_inputs: inputs.clone(),
                };
                // The immutable expected selections above precede the real producer call.
                // Prepared capture receives the physically admitted output; Ordinary admits it itself.
                let prepared_context = Arc::new(
                    (*original)
                        .clone()
                        .extend_program_value_interface(interface.clone())
                        .unwrap(),
                );
                let prepared_support =
                    PreparedValueSourceSupport::new(prepared_context.clone(), &lexical).unwrap();
                for prepared in [false, true] {
                    let result = if prepared {
                        inputs.capture_value_data(
                            1,
                            &cell,
                            &prepared_context,
                            &lexical,
                            None,
                            CheckedValueCaptureInput::Prepared {
                                support: &prepared_support,
                                produced: &produced,
                            },
                        )
                    } else {
                        inputs.capture_value_data(
                            1,
                            &cell,
                            &original,
                            &lexical,
                            None,
                            CheckedValueCaptureInput::Ordinary {
                                produced: Some(&produced),
                            },
                        )
                    };
                    let captured = match result {
                        Ok(captured) => captured,
                        Err(error) => {
                            failures.push(format!("changed_child={changed_child} selected={selected} prepared={prepared}: {error}"));
                            continue;
                        }
                    };
                    assert_eq!(
                        captured.compiler_input_projection().snapshot_roles()[&child_owner]
                            .interface(),
                        children[selected].descriptor.id
                    );
                    assert!(captured
                        .compiler_input_projection()
                        .roles()
                        .iter()
                        .any(|role| role.interface() == interface.artifact_id()));
                    assert!(
                        !captured
                            .compiler_input_projection()
                            .snapshot_roles()
                            .contains_key(&template_owner),
                        "unselected template-only custody cannot supply value compiler roles"
                    );
                    assert!(
                        captured
                            .compiler_input_projection()
                            .snapshot_roles()
                            .keys()
                            .all(|owner| owner == &epoch
                                || owner == &child_owner
                                || owner == &template_owner
                                || owner.module == interface.interface().module()),
                        "unrelated sibling custody cannot issue compiler roles"
                    );
                    assert_eq!(captured.source_lexical(), lexical);
                    let graph = captured.compiler_artifact_view().capture_graph_selection();
                    assert!(expected_edges.iter().all(|edge| captured
                        .compiler_artifact_view()
                        .binding_dependencies()
                        .contains(edge)));
                    let expected_groups = views[selected]
                        .capture_graph_selection()
                        .roots
                        .into_iter()
                        .filter(|node| matches!(node, ArtifactBindingNode::Group(_)))
                        .collect::<BTreeSet<_>>();
                    let actual_groups = graph
                        .roots
                        .into_iter()
                        .filter(|node| matches!(node, ArtifactBindingNode::Group(_)))
                        .collect::<BTreeSet<_>>();
                    assert_eq!(
                        actual_groups, expected_groups,
                        "historical custody cannot add executable roots"
                    );
                    let next = ExactDeclarationContext::new(&[], &[], vec![])
                        .unwrap()
                        .extend_retained_value_artifacts(&[captured.clone()])
                        .unwrap();
                    if let Some(inherited_path) = &inherited_path {
                        let scratch = tempfile::tempdir().unwrap();
                        let request = Arc::new(next.clone())
                            .prepare_compilation(
                                &scratch.path().join("descendant"),
                                VALUE_CUSTODY_PRODUCER,
                            )
                            .unwrap();
                        let parent = request
                            .artifacts
                            .iter()
                            .find(|artifact| artifact.interface.module == "Epoch")
                            .unwrap();
                        assert_eq!(
                            &parent.interface.path, inherited_path,
                            "value leaf cannot hide or recopy inherited A"
                        );
                        let value_row = request
                            .artifacts
                            .iter()
                            .find(|artifact| {
                                artifact.interface.module == captured.owner().module_name()
                            })
                            .unwrap();
                        assert_eq!(
                            value_row.interface.path, captured.interface.path,
                            "next request borrows the exact V leaf"
                        );
                    }
                    assert_eq!(
                        next.compiler_input_projection().snapshot_roles()[&child_owner].interface(),
                        children[selected].descriptor.id
                    );
                    let requirements = NativeOriginalRequirement::for_projection(
                        next.compiler_input_projection(),
                        next.artifact_view(),
                        &[],
                    )
                    .unwrap();
                    let issued = requirements
                        .iter()
                        .find(|proof| proof.original == native.descriptor.id)
                        .unwrap();
                    let observed = issued
                        .selection
                        .graph_selection()
                        .bindings
                        .iter()
                        .filter(|binding| {
                            children
                                .iter()
                                .any(|child| binding.artifact == child.descriptor.id)
                        })
                        .copied()
                        .collect::<Vec<_>>();
                    assert_eq!(
                        observed,
                        vec![selected_child],
                        "compiler demand uses its current exact child issuer"
                    );
                    let metadata = next.artifact_view().metadata_snapshot();
                    let mut selected_source = crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                        next.compiler_input_projection(), &metadata,
                        &crate::recovery_artifacts::PackageInterfaceValidation::default().inventory,
                    ).unwrap();
                    selected_source
                        .with_new_product_dependencies(&metadata, &requirements)
                        .unwrap();
                    histories.push((captured, expected_native.clone(), selected_child));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "capture histories: {}",
            failures.join("\n")
        );
        assert_eq!(histories.len(), 8);
        histories
    }

    #[test]
    fn independently_captured_values_preserve_one_compiler_native_issuer() {
        use super::*;
        use crate::artifact_inventory::{
            ArtifactBindingNode, ArtifactEntry, ArtifactInventory, ArtifactInventoryFailure,
            CompilerInputProjection, NativeOriginalRequirement, NativeRequirementRoot,
        };
        use crate::declaration_context::ExactDeclarationContext;
        use crate::declaration_join::{ExactLexicalNode, ExactModuleIdentity};
        let producer = [2; 32];
        let child_interface = crate::certified_products::fixture_module_interface(
            producer,
            "fixture",
            "Child",
            BTreeMap::new(),
        );
        let child = Arc::new(ArtifactEntry::canonical(child_interface.clone()));
        let product = crate::certified_products::fixture_finalized_product_with_requirements(
            crate::certified_products::tests::original_groups_fixture(
                "Epoch",
                vec![(7, vec![])],
                1,
                &BTreeMap::new(),
            ),
            producer,
            Some(BTreeMap::from([(
                ("fixture".into(), "Child".into()),
                child_interface.interface_sha256(),
            )])),
        );
        let product = crate::certified_products::tests::recovered_witness_fixtures(&[product])
            .remove(0)
            .product;
        let native = Arc::new(ArtifactEntry::original(producer, product).unwrap());
        let native_id = native.descriptor.id;
        let owner = ExactModuleIdentity {
            unit: "fixture".into(),
            module: "Epoch".into(),
        };
        let sources = ["Left", "Right"].map(|module| {
            let sibling = Arc::new(
                ArtifactEntry::original(
                    producer,
                    crate::certified_products::fixture_finalized_product(
                        crate::certified_products::tests::original_groups_fixture(
                            module,
                            vec![(7, vec![])],
                            1,
                            &BTreeMap::new(),
                        ),
                        producer,
                    ),
                )
                .unwrap(),
            );
            let inventory = ArtifactInventory::default();
            let view = inventory
                .admit_shared(
                    &inventory.empty_view(),
                    vec![native.clone(), child.clone(), sibling],
                )
                .unwrap();
            let projection =
                CompilerInputProjection::from_issued_entries(&[native.clone(), child.clone()])
                    .unwrap();
            Arc::new(
                ExactDeclarationContext::new(&[], &[], vec![])
                    .unwrap()
                    .extend_issued_compiler_support(
                        &crate::declaration_context::issue_fixture_compiler_support(
                            projection, view,
                        )
                        .unwrap(),
                    )
                    .unwrap()
                    .extend(
                        &[],
                        &[],
                        vec![ExactLexicalNode {
                            owner: owner.clone(),
                            imports: vec![],
                        }],
                    )
                    .unwrap(),
            )
        });
        let captured = std::array::from_fn::<_, 2, _>(|index| {
            capture_test_value_from_output_for_generation(
                &sources[index],
                true,
                false,
                index as u64 + 1,
            )
        });
        for reverse in [false, true] {
            let first = usize::from(reverse);
            let context = ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_retained_value_artifacts(&[
                    captured[first].clone(),
                    captured[1 - first].clone(),
                ])
                .unwrap();
            let view = context.artifact_view();
            let projection = context.compiler_input_projection();
            let graph = view.capture_graph_selection();
            let source_roots = |graph: &crate::artifact_inventory::ArtifactGraphSelection| {
                graph.roots.iter().filter(|root| matches!(root,
                    ArtifactBindingNode::Group(group) if group.binding.artifact() == native_id
                )).copied().collect::<BTreeSet<_>>()
            };
            assert_eq!(
                source_roots(&graph),
                source_roots(&sources[first].artifact_view().capture_graph_selection())
            );
            assert_eq!(
                graph
                    .bindings
                    .iter()
                    .filter(|binding| binding.artifact == native_id)
                    .count(),
                2
            );
            assert_eq!(
                graph
                    .native_groups
                    .iter()
                    .filter(|group| group.binding.artifact() == native_id)
                    .count(),
                2
            );
            let selected_child = sources[first]
                .artifact_view()
                .capture_graph_selection()
                .namespace
                .into_iter()
                .find(|binding| binding.artifact == child.descriptor.id)
                .unwrap();
            let canonical_id = projection
                .roles()
                .iter()
                .find(|role| role.original() == Some(native_id))
                .unwrap()
                .interface();
            let child_edges = view.binding_dependencies().into_iter().filter(|(source, target, _)| {
                matches!((source, target), (ArtifactBindingNode::Artifact(source), ArtifactBindingNode::Artifact(target))
                    if source.artifact == canonical_id && target.artifact == child.descriptor.id)
            }).collect::<BTreeSet<_>>();
            assert_eq!(child_edges.len(), 2);
            assert!(captured.iter().all(|value| value
                .compiler_artifact_view()
                .binding_dependencies()
                .iter()
                .all(|edge| view.binding_dependencies().contains(edge))));
            let inventory = ArtifactInventory::default();
            let recovered = inventory
                .recover_selection(
                    &inventory.empty_view(),
                    view.entries(),
                    &graph,
                    &view.binding_dependencies(),
                )
                .unwrap();
            assert_eq!(recovered.capture_graph_selection(), graph);
            for available in [view, &recovered] {
                let requirements =
                    NativeOriginalRequirement::for_projection(projection, available, &[]).unwrap();
                let selected = requirements
                    .iter()
                    .find(|requirement| requirement.original == native_id)
                    .unwrap();
                let child_bindings = selected
                    .selection
                    .graph_selection()
                    .bindings
                    .iter()
                    .filter(|binding| binding.artifact == child.descriptor.id)
                    .copied()
                    .collect::<Vec<_>>();
                assert_eq!(child_bindings, vec![selected_child]);
                let offers = requirements
                    .iter()
                    .map(|requirement| {
                        crate::artifact_inventory::IssuedNativeOffer::issue(
                            requirement.original,
                            available,
                            Some(requirement.clone()),
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                let catalog = crate::artifact_inventory::NativeDependencyCatalog::from_source(
                    context.compiler_source_support(),
                )
                .unwrap();
                let admitted = projection
                    .admit_interfaces_with_native_offers(available, vec![], &offers, &catalog)
                    .unwrap();
                admitted
                    .native_requirements_from_roots(&[NativeRequirementRoot::Group {
                        artifact: native_id,
                        original_ordinal: 7,
                    }])
                    .unwrap();
                assert_eq!(
                    source_roots(&admitted.capture_graph_selection()),
                    source_roots(&graph)
                );
            }
            let ambiguous = captured[first]
                .compiler_artifact_view()
                .merge_fixture_selections(captured[1 - first].compiler_artifact_view())
                .unwrap();
            let before = ambiguous.capture_graph_selection();
            let nodes = ambiguous.inventory().node_count();
            // Conflicting choices come from the two actual captured issuers,
            // not from treating every available proof as a selected root.
            let admitted = sources[first]
                .compiler_source_support()
                .merge(sources[1 - first].compiler_source_support());
            assert!(
                matches!(admitted, Err(CompileError::ArtifactInventory(error)) if matches!(error.failure,
                ArtifactInventoryFailure::BindingAmbiguity { artifact } if artifact == native_id))
            );
            assert_eq!(ambiguous.capture_graph_selection(), before);
            assert_eq!(ambiguous.inventory().node_count(), nodes);
        }
    }

    #[test]
    fn captured_value_reattachment_preserves_custody_without_selecting_unused_epoch() {
        check_captured_value_epoch_reattachment(false);
        check_captured_value_epoch_reattachment(true);
    }

    fn signature_codec_fixture() -> ciborium::Value {
        use super::*;
        // Structural codec fixture only; this payload is never executed by GHC.
        array([
            text("TPCHECKEDSIGNATURE2"),
            text("pin"),
            text("UI presentation"),
            Value::Bytes(vec![1, 2, 3]),
            array([array([
                text("main"),
                text("Owner"),
                text("type"),
                text("Original"),
            ])]),
        ])
    }

    fn request_signature_codec_fixture(progress: bool) -> ciborium::Value {
        use super::*;
        let signature = |key: &str| {
            let mut value = signature_codec_fixture();
            value.as_array_mut().unwrap()[1] = text(key);
            value
        };
        array([
            text("TPREQUESTTYPESIGNATURES1"),
            text("1"),
            signature("request-reply"),
            if progress {
                signature("request-progress")
            } else {
                Value::Null
            },
        ])
    }

    #[test]
    fn request_annotations_require_signatures_and_preserve_helper_mode() {
        use crate::declaration_join::{
            ExactCompileContext, ExactDeclarationContext, RequestHelperRecipe,
        };
        let ordinary = ExactCompileContext::new(std::sync::Arc::new(
            ExactDeclarationContext::new(&[], &[], Vec::new()).unwrap(),
        ));
        assert_eq!(
            ordinary
                .clone()
                .with_request_helper_recipe(RequestHelperRecipe::None)
                .unwrap(),
            ordinary,
        );
        assert!(ordinary
            .clone()
            .with_request_helper_recipe(RequestHelperRecipe::ActorReply)
            .is_err());
        assert!(ordinary.request_annotations().is_none());

        for progress in [false, true] {
            let mut bytes = Vec::new();
            ciborium::into_writer(&request_signature_codec_fixture(progress), &mut bytes).unwrap();
            let signatures =
                std::sync::Arc::new(RequestTypeSignatures::from_bytes(&bytes).unwrap());
            let type_only = ordinary.clone().with_request_types(signatures.clone());
            let annotations = type_only.request_annotations().unwrap();
            assert!(std::sync::Arc::ptr_eq(
                annotations.signatures(),
                &signatures
            ));
            assert_eq!(annotations.helper_recipe(), RequestHelperRecipe::None);
            let reply = type_only
                .with_request_helper_recipe(RequestHelperRecipe::ActorReply)
                .unwrap();
            let annotations = reply.request_annotations().unwrap();
            assert!(std::sync::Arc::ptr_eq(
                annotations.signatures(),
                &signatures
            ));
            assert_eq!(annotations.helper_recipe(), RequestHelperRecipe::ActorReply);
            let mut replacement_bytes = Vec::new();
            ciborium::into_writer(
                &request_signature_codec_fixture(!progress),
                &mut replacement_bytes,
            )
            .unwrap();
            let replacement =
                std::sync::Arc::new(RequestTypeSignatures::from_bytes(&replacement_bytes).unwrap());
            assert_ne!(replacement.metadata_digest(), signatures.metadata_digest());
            let replaced = reply.with_request_types(replacement.clone());
            let annotations = replaced.request_annotations().unwrap();
            assert!(std::sync::Arc::ptr_eq(
                annotations.signatures(),
                &replacement
            ));
            assert_eq!(annotations.helper_recipe(), RequestHelperRecipe::ActorReply);
        }
    }

    #[test]
    fn request_type_signatures_native_codec_preserves_payload_and_progress() {
        use super::*;
        for progress in [false, true] {
            let wire = request_signature_codec_fixture(progress);
            let mut bytes = Vec::new();
            ciborium::into_writer(&wire, &mut bytes).unwrap();
            let signatures = RequestTypeSignatures::from_bytes(&bytes).unwrap();
            assert_eq!(signatures.reply().key(), "request-reply");
            assert_eq!(
                signatures.progress().map(ExactCheckedSignature::key),
                progress.then_some("request-progress")
            );
            assert_eq!(signatures.authorization_value(), wire);
            assert_eq!(
                signatures.metadata_digest(),
                <[u8; 32]>::from(Sha256::digest(&bytes))
            );
            let hex = bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            assert_eq!(
                serde_json::from_value::<RequestTypeSignatures>(serde_json::Value::String(hex))
                    .unwrap(),
                signatures
            );
            let mut changed = wire;
            changed.as_array_mut().unwrap()[2].as_array_mut().unwrap()[3] =
                Value::Bytes(vec![3, 2, 1]);
            let mut changed_bytes = Vec::new();
            ciborium::into_writer(&changed, &mut changed_bytes).unwrap();
            assert_ne!(
                signatures.metadata_digest(),
                RequestTypeSignatures::from_bytes(&changed_bytes)
                    .unwrap()
                    .metadata_digest()
            );
        }
    }

    #[test]
    fn request_type_signatures_native_codec_refuses_legacy_purpose_and_envelope_substitution() {
        use super::*;
        let valid = request_signature_codec_fixture(true);
        for field in [0, 1] {
            let mut invalid = valid.clone();
            invalid.as_array_mut().unwrap()[field] = text("legacy");
            let mut bytes = Vec::new();
            ciborium::into_writer(&invalid, &mut bytes).unwrap();
            assert!(RequestTypeSignatures::from_bytes(&bytes).is_err());
        }
        for field in [2, 3] {
            let mut invalid = valid.clone();
            invalid.as_array_mut().unwrap()[field]
                .as_array_mut()
                .unwrap()[1] = text("activation-input");
            let mut bytes = Vec::new();
            ciborium::into_writer(&invalid, &mut bytes).unwrap();
            assert!(RequestTypeSignatures::from_bytes(&bytes).is_err());
        }
        let mut bytes = Vec::new();
        ciborium::into_writer(&valid, &mut bytes).unwrap();
        bytes.push(0);
        assert!(RequestTypeSignatures::from_bytes(&bytes).is_err());
        assert!(RequestTypeSignatures::from_bytes(&vec![0; 4 * 1024 * 1024 + 1]).is_err());
        for invalid in ["0", "FF", "gg"] {
            assert!(
                serde_json::from_value::<RequestTypeSignatures>(serde_json::Value::String(
                    invalid.into()
                ))
                .is_err()
            );
        }
    }

    #[test]
    fn checked_signature_native_codec_preserves_opaque_payload_and_original_names() {
        use super::*;
        let mut wire = signature_codec_fixture();
        let Value::Array(fields) = &mut wire else {
            unreachable!()
        };
        fields[4] = Value::Array(
            ["data", "type", "var"]
                .into_iter()
                .map(|namespace| {
                    array([
                        text("main"),
                        text("Owner"),
                        text(namespace),
                        text("Original"),
                    ])
                })
                .collect(),
        );
        let signature = decode_signature(&wire).unwrap();
        assert_eq!(signature.key(), "pin");
        assert_eq!(signature.presentation(), "UI presentation");
        assert_eq!(signature.iface.as_ref(), &[1, 2, 3]);
        assert_eq!(signature.names()[1].unit(), "main");
        assert_eq!(signature.names()[1].module(), "Owner");
        assert_eq!(signature.names()[1].namespace(), "type");
        assert_eq!(signature.names()[1].occurrence(), "Original");
        assert_eq!(encode_signature(&signature), wire);
    }

    #[test]
    fn checked_signature_native_codec_rejects_old_and_malformed_authority() {
        use super::*;
        assert!(decode_signature(&array([text("pin"), text("old"), array([])])).is_err());
        for (index, invalid) in [
            (0, text("TPCHECKEDSIGNATURE1")),
            (1, Value::Integer(1.into())),
            (2, Value::Bytes(vec![1])),
            (3, text("IfaceType")),
            (3, Value::Bytes(Vec::new())),
            (3, Value::Bytes(vec![1; 4 * 1024 * 1024 + 1])),
            (4, Value::Array(vec![array([]); 65537])),
        ] {
            let mut wire = signature_codec_fixture();
            let Value::Array(fields) = &mut wire else {
                unreachable!()
            };
            fields[index] = invalid;
            assert!(decode_signature(&wire).is_err());
        }
        for (index, invalid) in [(0, ""), (1, ""), (2, "field"), (3, "")] {
            let mut wire = signature_codec_fixture();
            let Value::Array(fields) = &mut wire else {
                unreachable!()
            };
            let Value::Array(names) = &mut fields[4] else {
                unreachable!()
            };
            let Value::Array(name) = &mut names[0] else {
                unreachable!()
            };
            name[index] = text(invalid);
            assert!(decode_signature(&wire).is_err());
        }
        for occurrences in [["A", "A"], ["B", "A"]] {
            let mut wire = signature_codec_fixture();
            let Value::Array(fields) = &mut wire else {
                unreachable!()
            };
            fields[4] = Value::Array(
                occurrences
                    .into_iter()
                    .map(|occurrence| {
                        array([text("main"), text("Owner"), text("type"), text(occurrence)])
                    })
                    .collect(),
            );
            assert!(decode_signature(&wire).is_err());
        }
    }

    fn world_binder_fixture(name: &str, varid: u64) -> ciborium::Value {
        use super::*;
        array([
            text(name),
            Value::Integer(varid.into()),
            text("SessionMain"),
            text("ForceData"),
            text("Int"),
            array([
                array([
                    text("main"),
                    text("Owner"),
                    text("data"),
                    text("Original"),
                    array([Value::Integer(0.into())]),
                ]),
                array([Value::Integer(1.into()), Value::Bytes(vec![3; 16])]),
            ]),
            text("Text"),
        ])
    }

    #[test]
    fn world_item_binders_decode_exact_reservation_and_reject_substitution() {
        use super::*;
        let expected = vec![
            ("x".to_owned(), tidepool_repr::SessionVarId::from_extract(7)),
            ("y".to_owned(), tidepool_repr::SessionVarId::from_extract(9)),
        ];
        let wire = array([world_binder_fixture("x", 7), world_binder_fixture("y", 9)]);
        let decoded = decode_world_item_binders(&wire, &expected).unwrap();
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].name(), "x");
        assert_eq!(decoded[0].varid(), 7);
        assert_eq!(decoded[0].module(), "SessionMain");
        assert_eq!(decoded[0].tier(), WorldItemBinderTier::ForceData);
        assert_eq!(decoded[0].type_display(), "Int");
        assert_eq!(
            decoded[0].host_authority(),
            Some(WorldItemHostAuthority::Text)
        );
        assert!(decoded[0].root_head().is_some());

        for (wire, message) in [
            (
                array([world_binder_fixture("y", 7), world_binder_fixture("y", 9)]),
                "binder order is part of the exact reservation",
            ),
            (
                array([world_binder_fixture("x", 8), world_binder_fixture("y", 9)]),
                "binder id is part of the exact reservation",
            ),
            (
                array([world_binder_fixture("x", 7)]),
                "binder count is part of the exact reservation",
            ),
            (
                array([world_binder_fixture("x", 7), world_binder_fixture("x", 7)]),
                "duplicate stable ids cannot define two binders",
            ),
        ] {
            assert!(
                decode_world_item_binders(&wire, &expected).is_err(),
                "{message}"
            );
        }

        let mut unknown_tier = world_binder_fixture("x", 7);
        unknown_tier.as_array_mut().unwrap()[3] = text("ForceLazy");
        assert!(decode_world_item_binders(
            &array([unknown_tier, world_binder_fixture("y", 9)]),
            &expected
        )
        .is_err());
        let mut unknown_authority = world_binder_fixture("x", 7);
        unknown_authority.as_array_mut().unwrap()[6] = text("Process");
        assert!(decode_world_item_binders(
            &array([unknown_authority, world_binder_fixture("y", 9)]),
            &expected
        )
        .is_err());
    }

    #[test]
    fn world_binder_root_head_uses_short_version_arrays_not_cbor_tags() {
        use super::*;
        let expected = vec![("x".to_owned(), tidepool_repr::SessionVarId::from_extract(7))];
        let mut wired = world_binder_fixture("x", 7);
        {
            let Value::Array(root_head) = &mut wired.as_array_mut().unwrap()[5] else {
                unreachable!()
            };
            root_head[1] = array([Value::Integer(0.into())]);
        }
        let decoded = decode_world_item_binders(&array([wired]), &expected).unwrap();
        assert_eq!(decoded[0].type_display(), "Int");
        for version in [
            array([Value::Integer(2.into())]),
            array([Value::Integer(1.into()), Value::Bytes(vec![3; 8])]),
            array([Value::Integer(1.into())]),
            Value::Integer(1.into()),
        ] {
            let mut invalid = world_binder_fixture("x", 7);
            let Value::Array(root_head) = &mut invalid.as_array_mut().unwrap()[5] else {
                unreachable!()
            };
            root_head[1] = version;
            assert!(decode_world_item_binders(&array([invalid]), &expected).is_err());
        }
    }

    #[test]
    fn ordered_item_observations_require_typed_segment_and_exhaustive_evidence() {
        // Deliberately untrusted decoder input, not a checked/executable owner.
        let mut output = vec![
            array([array([
                Value::Integer(0.into()),
                text("bind"),
                text("pure ()"),
                array([array([]), Value::Null]),
                Value::Null,
                Value::Null,
            ])]),
            array([]),
            text("checked source"),
            Value::Null,
            array([]),
        ];
        let segment = CheckedTypedSegmentPlan {
            digest: "segment".into(),
            root: "root".into(),
            items: vec![CheckedTypedSegmentItem {
                ordinal: 0,
                entry: "entry".into(),
                generation: 1,
                body: CheckedTypedSegmentBody::Action {
                    step: "step".into(),
                    probe: "probe".into(),
                    marker: "marker".into(),
                    captures: vec![],
                },
            }],
        };
        let decode = |output: &[Value], segments: &[CheckedTypedSegmentPlan]| {
            decode_checked_items(output, segments.iter(), std::iter::empty(), true)
        };
        let segments = [segment];
        let items = decode(&output, &segments).unwrap();
        assert_eq!(items[0].kind, CheckedItemKind::Bind);
        assert_eq!(items[0].source, "pure ()");
        assert!(
            decode(&output, &[]).is_err(),
            "missing typed entry must refuse"
        );
        output[1] = array([array([text("foreign"), Value::Null, Value::Null])]);
        assert!(
            decode(&output, &segments).is_err(),
            "ordered pins must refuse"
        );
        output[1] = array([]);
        output[4] = array([array([
            text("foreign"),
            Value::Null,
            Value::Null,
            Value::Null,
        ])]);
        assert!(
            decode(&output, &segments).is_err(),
            "unowned observation must refuse"
        );
    }

    #[test]
    fn world_item_signatures_key_entry_and_capture_order() {
        use super::*;
        let signature = |key: &str| {
            let mut value = signature_codec_fixture();
            value.as_array_mut().unwrap()[1] = text(key);
            value
        };
        let wire = array([signature("Entry:x")]);
        let decoded = decode_world_item_signatures(&wire, "Entry", &["x"]).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].key(), "Entry:x");
        assert!(decode_world_item_signatures(&wire, "Other", &["x"]).is_err());
        assert!(decode_world_item_signatures(&wire, "Entry", &["y"]).is_err());
        assert!(decode_world_item_signatures(&wire, "Entry", &["x", "y"]).is_err());
        assert!(
            decode_world_item_signatures(&array([]), "Entry", &["x"]).is_err(),
            "an item with one capture cannot omit its signature"
        );
    }

    fn witness_bytes(shape: ciborium::Value, seal: &str) -> Vec<u8> {
        use super::*;
        let mut structure = Vec::new();
        ciborium::into_writer(&shape, &mut structure).unwrap();
        let wire = array([
            text("TPCANONICALINPUTTYPE1"),
            text("1"),
            // Structural codec fixture only; these bytes are not a GHC IfaceType.
            array([
                text("TPCHECKEDSIGNATURE2"),
                text("activation-input"),
                text("presentation"),
                Value::Bytes(vec![1]),
                array([]),
            ]),
            Value::Bytes(structure),
            array([array([text("main"), text("Owner"), text(seal)])]),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&wire, &mut bytes).unwrap();
        bytes
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
        signature[2] = text("different parser presentation");
        signature[3] = Value::Bytes(vec![2]);
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
            request_type_signatures: None,
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
    fn original_input_witness_retains_payload_and_refuses_unowned_producer() {
        use super::*;
        // Codec-only bytes cannot mint native authority: the context must first
        // belong to an authenticated original compiler output bundle.
        let bytes = witness_bytes(
            array([text("literal"), text("nat"), text("1")]),
            &"a".repeat(64),
        );
        // The literal has no owner seal; remove the helper's nominal-only seal.
        let mut value = decode(&bytes).unwrap();
        let Value::Array(fields) = &mut value else {
            unreachable!()
        };
        fields[4] = array([]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&value, &mut bytes).unwrap();
        let witness = CanonicalInputTypeWitness::from_bytes(&bytes).unwrap();
        assert_eq!(witness.original_bytes(), bytes);
        assert_eq!(
            witness.metadata_digest(),
            <[u8; 32]>::from(Sha256::digest(&bytes))
        );
        let context = Arc::new(
            crate::declaration_context::ExactDeclarationContext::new(&[], &[], Vec::new()).unwrap(),
        );
        assert!(ExactHostBindingPrototype::from_original_input(witness, context).is_err());
        // Indefinite arrays decode to the same structure but are not the
        // compiler's canonical serialized payload.
        assert_eq!(bytes[0], 0x85);
        let mut noncanonical = bytes.clone();
        noncanonical[0] = 0x9f;
        noncanonical.push(0xff);
        assert!(CanonicalInputTypeWitness::from_bytes(&noncanonical).is_err());
        let mut trailing = bytes;
        trailing.push(0);
        assert!(CanonicalInputTypeWitness::from_bytes(&trailing).is_err());
    }

    #[test]
    fn canonical_input_witness_refuses_noncanonical_embedded_structure() {
        use super::*;
        // Structural codec control only; this fixture grants no native authority.
        let mut wire = decode(&witness_bytes(
            array([text("literal"), text("char"), Value::Integer(0.into())]),
            &"a".repeat(64),
        ))
        .unwrap();
        let Value::Array(fields) = &mut wire else {
            unreachable!()
        };
        fields[4] = array([]);
        let Value::Bytes(canonical) = &fields[3] else {
            unreachable!()
        };
        let canonical = canonical.clone();
        let canonical_value = decode(&canonical).unwrap();
        let encode_witness = |structure: Vec<u8>| {
            let mut wire = wire.clone();
            let Value::Array(fields) = &mut wire else {
                unreachable!()
            };
            fields[3] = Value::Bytes(structure);
            let mut bytes = Vec::new();
            ciborium::into_writer(&wire, &mut bytes).unwrap();
            bytes
        };
        CanonicalInputTypeWitness::from_bytes(&encode_witness(canonical.clone())).unwrap();
        assert_eq!(canonical[0], 0x83);
        assert_eq!(canonical.last(), Some(&0x00));
        let mut indefinite = canonical.clone();
        indefinite[0] = 0x9f;
        indefinite.push(0xff);
        let mut nonminimal = canonical;
        nonminimal.pop();
        nonminimal.extend_from_slice(&[0x18, 0x00]);
        for structure in [indefinite, nonminimal] {
            assert_eq!(decode(&structure).unwrap(), canonical_value);
            let error =
                CanonicalInputTypeWitness::from_bytes(&encode_witness(structure)).unwrap_err();
            assert!(error
                .to_string()
                .contains("canonical input structure uses noncanonical CBOR"));
        }
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

    #[test]
    fn host_interface_certification_consumes_only_receipt_sealed_capture() {
        use super::{array, hash, text, CapturedValueInterfaceOutput, Value};
        use crate::certified_products::{
            fixture_module_interface, fixture_source_module_interface,
        };
        use std::collections::BTreeMap;

        let owner = tidepool_repr::SessionModule::val(tidepool_repr::Generation(7));
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(owner.relative_hi_path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original =
            fixture_module_interface([1; 32], "main", &owner.module_name(), BTreeMap::new());
        let interface = original.interface_bytes();
        let packages = original.package_imports_bytes();
        let package_path = directory.path().join("package.hi");
        std::fs::write(&package_path, b"replacement package dependency").unwrap();
        let replacement = fixture_source_module_interface(
            [1; 32],
            "main",
            &owner.module_name(),
            [2; 32],
            BTreeMap::new(),
            Some(&package_path),
        );
        let replacement_packages = replacement.package_imports_bytes();
        let encode = |value: Value| {
            let mut bytes = Vec::new();
            ciborium::ser::into_writer(&value, &mut bytes).unwrap();
            bytes
        };
        let requirements = encode(array([array([text("main"), text("OriginalType")])]));
        let replacement_requirements =
            encode(array([array([text("main"), text("ReplacementType")])]));
        let expected = [hash(interface), hash(packages), hash(&requirements)];
        let seals = || expected.each_ref().map(String::as_str);
        let restore = || {
            std::fs::write(&path, interface).unwrap();
            std::fs::write(path.with_extension("hi.packages"), packages).unwrap();
            std::fs::write(path.with_extension("hi.requirements"), &requirements).unwrap();
        };
        restore();
        let captured = CapturedValueInterfaceOutput::capture(&path).unwrap();
        std::fs::write(&path, b"replacement interface").unwrap();
        std::fs::write(path.with_extension("hi.packages"), replacement_packages).unwrap();
        std::fs::write(
            path.with_extension("hi.requirements"),
            &replacement_requirements,
        )
        .unwrap();
        let certified = captured.certify([1; 32], owner, Some(seals())).unwrap();
        assert_eq!(certified.interface().interface_bytes(), interface);
        assert_eq!(certified.interface().package_imports_bytes(), packages);
        assert_eq!(
            certified.requirements(),
            &[crate::declaration_join::ExactModuleIdentity {
                unit: "main".into(),
                module: "OriginalType".into(),
            }]
        );
        assert!(CapturedValueInterfaceOutput::capture(&path)
            .unwrap()
            .certify([1; 32], owner, Some(seals()))
            .is_err());

        // Both typed package fixtures certify the same interface; only the
        // dependency evidence changes. Every component has a receipt seal.
        for changed in 0..3 {
            restore();
            match changed {
                0 => std::fs::write(&path, b"replacement interface").unwrap(),
                1 => std::fs::write(path.with_extension("hi.packages"), replacement_packages)
                    .unwrap(),
                _ => std::fs::write(
                    path.with_extension("hi.requirements"),
                    &replacement_requirements,
                )
                .unwrap(),
            }
            assert!(
                CapturedValueInterfaceOutput::capture(&path)
                    .unwrap()
                    .certify([1; 32], owner, Some(seals()))
                    .is_err(),
                "changed component {changed} must not be certified by the old receipt"
            );
        }
    }

    #[test]
    fn checked_value_capture_refuses_raw_bytes_without_original_certificate() {
        let owner = tidepool_repr::SessionModule::val(tidepool_repr::Generation(3));
        let bytes: Arc<[u8]> = Arc::from(&b"raw interface"[..]);
        let raw = CheckedValueInputs::capture_raw(vec![(owner, bytes.clone())]).unwrap();
        assert!(raw.certified_artifacts().is_empty());
        assert!(matches!(
            raw.baseline[0],
            super::CapturedValueInterface::Raw(_)
        ));
        assert!(CheckedValueInputs::capture_checked(vec![(owner, bytes)], &[]).is_err());
        assert!(CheckedValueInputs::capture_checked(Vec::new(), &[]).is_ok());
    }

    #[test]
    fn failed_checked_inputs_retain_sealed_and_observed_bytes_after_owner_drop() {
        let owner = tidepool_repr::SessionModule::val(tidepool_repr::Generation(3));
        let inputs = CheckedValueInputs::capture_raw(vec![(owner, Arc::from(&b"sealed"[..]))])
            .expect("capture exact input");
        std::fs::write(inputs.root().join(owner.relative_hi_path()), b"edited")
            .expect("alter observed file");
        std::fs::write(inputs.root().join("unselected.hi"), b"unselected")
            .expect("unselected directory member");
        let retained = tempfile::tempdir().expect("retained diagnostic directory");
        inputs
            .retain_diagnostics(retained.path())
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

/// Serialization inputs have no completed-prefix or same-offer capability.
pub(crate) struct ItemAuthorization<'a> {
    pub(crate) admission: [u8; 32],
    pub(crate) receipt: [u8; 32],
    pub(crate) index: usize,
    pub(crate) source: &'a str,
    pub(crate) kind: CheckedItemKind,
    pub(crate) binders: &'a [String],
    pub(crate) templates: &'a [(String, String)],
    pub(crate) injected: &'a [String],
    pub(crate) signatures: &'a [ExactCheckedSignature],
    pub(crate) expression: Option<&'a Value>,
    pub(crate) generation: u64,
    pub(crate) runtime_prefix: [u8; 32],
    pub(crate) imports: &'a [(String, Vec<String>)],
    pub(crate) observation: Option<&'a str>,
    pub(crate) planned: Value,
    pub(crate) settled: Vec<Value>,
    pub(crate) value_interfaces: Value,
    pub(crate) template_imports: crate::declaration_context::SelectedTemplateImports,
}
pub(crate) fn encode_item_authorization(fields: ItemAuthorization<'_>) -> Value {
    array([
        text(crate::artifacts::CheckedPurpose::Item.wire_tag()),
        text(hex(&fields.admission)),
        text(hex(&fields.receipt)),
        Value::Integer((fields.index as u64).into()),
        text(hash(fields.source.as_bytes())),
        text(match fields.kind {
            CheckedItemKind::Bind => "bind",
            CheckedItemKind::Expression => "expr",
            CheckedItemKind::Declaration => "decl",
        }),
        Value::Array(fields.binders.iter().map(text).collect()),
        Value::Array(
            fields
                .templates
                .iter()
                .map(|(kind, source)| array([text(kind), text(hash(source.as_bytes()))]))
                .collect(),
        ),
        Value::Array(fields.injected.iter().map(text).collect()),
        Value::Array(fields.signatures.iter().map(encode_signature).collect()),
        fields.expression.cloned().unwrap_or(Value::Null),
        Value::Integer(fields.generation.into()),
        text(hex(&fields.runtime_prefix)),
        Value::Array(
            fields
                .imports
                .iter()
                .map(|(module, names)| {
                    array([text(module), Value::Array(names.iter().map(text).collect())])
                })
                .collect(),
        ),
        fields.observation.map_or(Value::Null, text),
        fields.planned,
        Value::Array(fields.settled),
        fields.value_interfaces,
        fields.template_imports.authorization_value(),
    ])
}
#[cfg(test)]
pub(crate) fn fixture_checked_signature(
    bytes: &[u8],
) -> Result<ExactCheckedSignature, CompileError> {
    decode_signature(&decode(bytes)?)
}

fn decode_expression_lift(expression: &Value) -> Result<CheckedExpressionLift, CompileError> {
    Ok(match string(&row(expression, 4)?[1])? {
        "pure" => CheckedExpressionLift::Pure,
        "effectful" => CheckedExpressionLift::Effectful,
        _ => return Err(failure("sealed expression has an unknown lift")),
    })
}
#[cfg(test)]
pub(crate) fn fixture_expression_plan(bytes: &[u8]) -> Result<Value, CompileError> {
    let expression = unique_key(&[decode(bytes)?], "__tidepool_cell_expr_0", 4)?;
    decode_expression_lift(&expression)?;
    Ok(expression)
}
