//! Shared semantic inventory tables with a bound on total index references.
use super::{
    identity_value, rep_value, signature_value, ProjectedGroup, RuntimeRep, Signature,
    SymbolIdentity, Value, MANIFEST_LIMIT,
};
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::PathBuf;

const TABLE_LIMIT: usize = 65536;
const LIST_LIMIT: usize = 65536;
const SIGNATURE_LIMIT: usize = 256;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::module_candidates) struct GlobalKey {
    pub(in crate::module_candidates) identity: SymbolIdentity,
    pub(in crate::module_candidates) rep: RuntimeRep,
    pub(in crate::module_candidates) signature: Option<Signature>,
    pub(in crate::module_candidates) evaluated: bool,
    pub(in crate::module_candidates) generation: Option<u64>,
}

/// Structural codec input; this contains no executable definitions or admission proof.
#[cfg(test)]
pub(in crate::module_candidates) struct StructuralGroup {
    pub(in crate::module_candidates) ordinal: u32,
    pub(in crate::module_candidates) binders: Vec<SymbolIdentity>,
    pub(in crate::module_candidates) globals: Vec<GlobalKey>,
}

#[derive(Clone, Copy)]
enum GroupView<'a> {
    Projected(&'a ProjectedGroup),
    #[cfg(test)]
    Structural(&'a StructuralGroup),
}
impl<'a> GroupView<'a> {
    fn ordinal(self) -> u32 {
        match self {
            Self::Projected(group) => group.original_ordinal(),
            #[cfg(test)]
            Self::Structural(group) => group.ordinal,
        }
    }
    fn binders(self) -> &'a [SymbolIdentity] {
        match self {
            Self::Projected(group) => group.binders(),
            #[cfg(test)]
            Self::Structural(group) => &group.binders,
        }
    }
    fn globals_len(self) -> usize {
        match self {
            Self::Projected(group) => group.globals().len(),
            #[cfg(test)]
            Self::Structural(group) => group.globals.len(),
        }
    }
    fn keys(self) -> impl Iterator<Item = InventoryResult<GlobalKey>> + 'a {
        (0..self.globals_len()).map(move |index| match self {
            Self::Projected(group) => global_key(group, &group.globals()[index]),
            #[cfg(test)]
            Self::Structural(group) => {
                let key = &group.globals[index];
                validate_key(key)?;
                Ok(key.clone())
            }
        })
    }
    fn check(self) -> InventoryResult<()> {
        check_budget(
            InventoryResource::BinderCount,
            self.binders().len(),
            LIST_LIMIT,
        )?;
        check_budget(
            InventoryResource::GlobalCount,
            self.globals_len(),
            LIST_LIMIT,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InventoryResource {
    ManifestBytes,
    GroupCount,
    BinderCount,
    GlobalCount,
    References,
    SymbolEntries,
    GlobalEntries,
    UniqueEntryBytes,
    SignatureArguments,
    SignatureResults,
}

impl InventoryResource {
    fn unit(self) -> &'static str {
        match self {
            Self::UniqueEntryBytes | Self::ManifestBytes => "bytes",
            Self::References => "references",
            Self::SymbolEntries | Self::GlobalEntries => "entries",
            Self::GroupCount
            | Self::BinderCount
            | Self::GlobalCount
            | Self::SignatureArguments
            | Self::SignatureResults => "elements",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(super) enum InventoryEncodingError {
    #[error("inventory {resource:?}: observed {observed}, limit {limit} {}", resource.unit())]
    Budget {
        resource: InventoryResource,
        observed: usize,
        limit: usize,
    },
    #[error("inventory accounting overflow")]
    ArithmeticOverflow,
    #[error("inventory signature index {index} is unavailable")]
    InvalidSignatureIndex { index: usize },
    #[error("inventory encoding failed")]
    Encoding,
    #[error("manifest acquisition refused")]
    Acquisition,
    #[error("candidate metadata preparation refused")]
    Preparation,
    #[error("manifest candidate metadata is absent")]
    MissingManifestMetadata,
    #[error("availability-only candidate supplied manifest metadata")]
    UnexpectedManifestMetadata,
}

type InventoryResult<T> = Result<T, InventoryEncodingError>;

fn check_budget(resource: InventoryResource, observed: usize, limit: usize) -> InventoryResult<()> {
    if observed > limit {
        Err(InventoryEncodingError::Budget {
            resource,
            observed,
            limit,
        })
    } else {
        Ok(())
    }
}

#[derive(Default, Debug, Eq, PartialEq)]
struct InventoryWork {
    group_rows: usize,
    binder_references: usize,
    global_key_resolutions: usize,
}

/// An immutable encoding owner. Each row stays paired with the input that
/// supplied its ordered groups; table indices never leave as reusable handles.
pub(super) struct PreparedInventory<T> {
    tables: WireTables,
    rows: Vec<(T, Option<Value>)>,
    #[cfg(test)]
    work: InventoryWork,
}

impl<T> PreparedInventory<T> {
    pub(super) fn new(
        inputs: impl IntoIterator<Item = T>,
        groups: impl Fn(&T) -> Option<&[ProjectedGroup]>,
    ) -> InventoryResult<Self> {
        let mut tables = InventoryTables::empty();
        let mut work = InventoryWork::default();
        let mut rows = Vec::new();
        let mut references = 0;
        for input in inputs {
            let row = match groups(&input) {
                Some(groups) => Some(tables.prepare_groups(
                    groups.iter().map(GroupView::Projected),
                    &mut references,
                    &mut work,
                )?),
                None => None,
            };
            rows.push((input, row));
        }
        tracing::debug!(target: "tidepool_toolchain::module_candidates", ?work,
            unique_symbols = tables.symbol_rows.len(), unique_globals = tables.global_rows.len(),
            "prepared candidate inventory");
        Ok(Self {
            tables: tables.finish(),
            rows,
            #[cfg(test)]
            work,
        })
    }

    pub(super) fn try_finish(
        self,
        mut metadata: impl FnMut(T) -> Option<ManifestDisposition>,
    ) -> InventoryResult<FinishedCandidateManifest> {
        let mut rows = Vec::with_capacity(self.rows.len());
        for (input, groups) in self.rows {
            match (
                groups,
                metadata(input).ok_or(InventoryEncodingError::Preparation)?,
            ) {
                (Some(groups), ManifestDisposition::Emit(metadata)) => {
                    let Value::Array(ref entries) = groups else {
                        unreachable!("prepared groups are an array")
                    };
                    check_budget(InventoryResource::GroupCount, entries.len(), LIST_LIMIT)?;
                    rows.push(metadata.with_groups(groups))
                }
                (Some(_), ManifestDisposition::Omit)
                | (None, ManifestDisposition::AvailabilityOnly) => {}
                (Some(_), ManifestDisposition::AvailabilityOnly) => {
                    return Err(InventoryEncodingError::MissingManifestMetadata)
                }
                (None, ManifestDisposition::Emit(_) | ManifestDisposition::Omit) => {
                    return Err(InventoryEncodingError::UnexpectedManifestMetadata)
                }
            }
        }
        Ok(FinishedCandidateManifest {
            tables: self.tables,
            rows,
        })
    }
}

/// Existing selection policy: omitted non-availability records still contributed
/// inventory tables before metadata admission. Only an explicit Omit preserves
/// that outcome; availability-only input cannot emit or omit an indexed row.
pub(super) enum ManifestDisposition {
    AvailabilityOnly,
    Emit(CandidateManifestMetadata),
    Omit,
}

/// Preserve the existing short-circuit metadata omission policy. This does not
/// validate source authority; callers already own admitted candidate evidence.
pub(super) fn manifest_source_evidence<T>(
    digest: Option<String>,
    matching_module: impl FnOnce() -> Option<T>,
) -> Option<(String, T)> {
    Some((digest?, matching_module()?))
}

struct WireTables {
    symbols: Value,
    globals: Value,
}

/// Semantic candidate fields contain no inventory indices. The inventory owner
/// alone supplies the matching group-reference row after metadata preparation.
pub(super) struct CandidateManifestMetadata {
    pub(super) unit: String,
    pub(super) module: String,
    pub(super) source: PathBuf,
    pub(super) source_sha256: String,
    pub(super) interface: PathBuf,
    pub(super) interface_sha256: String,
    pub(super) module_version: String,
    pub(super) product_sha256: String,
    pub(super) evidence_sha256: String,
    pub(super) imports: Vec<Value>,
    pub(super) packages: PathBuf,
    pub(super) packages_sha256: String,
    pub(super) product: PathBuf,
    pub(super) canonical_requirements: BTreeMap<(String, String), [u8; 32]>,
    pub(super) certificate: PathBuf,
    pub(super) certificate_sha256: [u8; 32],
    pub(super) core: PathBuf,
    pub(super) core_sha256: [u8; 32],
}

impl CandidateManifestMetadata {
    fn with_groups(self, groups: Value) -> Value {
        let path = |path: PathBuf| Value::Text(path.to_string_lossy().into_owned());
        Value::Array(vec![
            Value::Text(self.unit),
            Value::Text(self.module),
            path(self.source),
            Value::Text(self.source_sha256),
            path(self.interface),
            Value::Text(self.interface_sha256),
            Value::Text(self.module_version),
            Value::Text(self.product_sha256),
            Value::Text(self.evidence_sha256),
            Value::Array(self.imports),
            groups,
            path(self.packages),
            Value::Text(self.packages_sha256),
            path(self.product),
            Value::Array(
                self.canonical_requirements
                    .into_keys()
                    .map(|(unit, module)| {
                        Value::Array(vec![Value::Text(unit), Value::Text(module)])
                    })
                    .collect(),
            ),
            Value::Array(vec![
                Value::Text("module".into()),
                path(self.certificate),
                Value::Text(super::hex(&self.certificate_sha256)),
                path(self.core),
                Value::Text(super::hex(&self.core_sha256)),
            ]),
        ])
    }
}

pub(super) struct FinishedCandidateManifest {
    tables: WireTables,
    rows: Vec<Value>,
}

impl FinishedCandidateManifest {
    pub(super) fn into_manifest(
        self,
        graphs: Vec<Value>,
        owners: Vec<Value>,
        producer: &str,
    ) -> CandidateManifest {
        CandidateManifest {
            value: Value::Array(vec![
                Value::Text("TPMCAN".into()),
                Value::Text("11".into()),
                self.tables.symbols,
                self.tables.globals,
                Value::Array(self.rows),
                Value::Array(vec![Value::Array(graphs), Value::Array(owners)]),
                Value::Text(producer.into()),
                Value::Array(vec![Value::Text("fresh-files".into())]),
            ]),
        }
    }
}

pub(super) struct CandidateManifest {
    value: Value,
}

impl CandidateManifest {
    /// Acquisition may inspect the completed manifest and supplies only its
    /// transport descriptor. Tables and candidate rows cannot be replaced.
    pub(super) fn encode_with_acquisition<U>(
        self,
        acquire: impl FnOnce(&Value) -> Result<(Value, U), InventoryEncodingError>,
    ) -> Result<(Vec<u8>, U), InventoryEncodingError> {
        let (acquisition, transport) = acquire(&self.value)?;
        let Value::Array(mut fields) = self.value else {
            unreachable!("manifest owner constructs an array")
        };
        fields[7] = acquisition;
        let mut encoded = Vec::new();
        ciborium::ser::into_writer(&Value::Array(fields), &mut encoded)
            .map_err(|_| InventoryEncodingError::Encoding)?;
        if encoded.len() > MANIFEST_LIMIT {
            tracing::info!(target: "tidepool_toolchain::module_candidates", reason = ?super::CandidateExecutionUnavailable::ManifestBound,
                manifest_bytes = encoded.len(), limit = MANIFEST_LIMIT,
                "candidate offer omitted because its execution provenance exceeds the manifest bound");
            return Err(InventoryEncodingError::Budget {
                resource: InventoryResource::ManifestBytes,
                observed: encoded.len(),
                limit: MANIFEST_LIMIT,
            });
        }
        Ok((encoded, transport))
    }
}

pub(super) struct InventoryTables {
    symbols: BTreeMap<SymbolIdentity, usize>,
    globals: BTreeMap<GlobalKey, usize>,
    symbol_rows: Vec<Value>,
    global_rows: Vec<Value>,
    #[cfg(test)]
    references_emitted: usize,
}

impl InventoryTables {
    fn empty() -> Self {
        Self {
            symbols: BTreeMap::new(),
            globals: BTreeMap::new(),
            symbol_rows: Vec::new(),
            global_rows: Vec::new(),
            #[cfg(test)]
            references_emitted: 0,
        }
    }

    /// Resolve each occurrence once, preserving the first-seen table order.
    fn prepare_groups<'a>(
        &mut self,
        groups: impl ExactSizeIterator<Item = GroupView<'a>>,
        references: &mut usize,
        work: &mut InventoryWork,
    ) -> InventoryResult<Value> {
        // The legacy path bounds group rows only when a candidate is emitted.
        // Metadata-omitted candidates still contribute shared table entries.
        let mut rows = Vec::with_capacity(groups.len());
        for group in groups {
            group.check()?;
            charge_references(references, group.binders().len(), group.globals_len())?;
            let mut binders = Vec::with_capacity(group.binders().len());
            for binder in group.binders() {
                work.binder_references += 1;
                binders.push(Value::Integer((self.intern_symbol(binder)? as u64).into()));
            }
            let mut globals = Vec::with_capacity(group.globals_len());
            for key in group.keys() {
                work.global_key_resolutions += 1;
                globals.push(Value::Integer((self.intern_global(key?)? as u64).into()));
            }
            work.group_rows += 1;
            rows.push(Value::Array(vec![
                Value::Integer(group.ordinal().into()),
                Value::Array(binders),
                Value::Array(globals),
            ]));
        }
        Ok(Value::Array(rows))
    }

    #[cfg(test)]
    pub(super) fn new<'a>(groups: impl IntoIterator<Item = &'a ProjectedGroup>) -> Option<Self> {
        Self::from_views(groups.into_iter().map(GroupView::Projected)).or_else(|| {
            tracing::warn!(target: "tidepool_toolchain::module_candidates", "candidate inventory construction refused");
            None
        })
    }

    #[cfg(test)]
    pub(in crate::module_candidates) fn structural(groups: &[StructuralGroup]) -> Option<Self> {
        Self::from_views(groups.iter().map(GroupView::Structural))
    }

    #[cfg(test)]
    fn from_views<'a>(groups: impl IntoIterator<Item = GroupView<'a>>) -> Option<Self> {
        let mut tables = Self::empty();
        let mut references = 0usize;
        for group in groups {
            group.check().ok()?;
            charge_references(&mut references, group.binders().len(), group.globals_len()).ok()?;
            for binder in group.binders() {
                tables.intern_symbol(binder).ok()?;
            }
            for key in group.keys() {
                tables.intern_global(key.ok()?).ok()?;
            }
        }
        Some(tables)
    }

    /// Each index needs at least one byte in the bounded manifest. Charge the
    /// aggregate reference budget across candidates before allocating lists.
    #[cfg(test)]
    pub(super) fn groups(&mut self, groups: &[ProjectedGroup]) -> Option<Value> {
        self.encode_groups(groups.iter().map(GroupView::Projected))
    }

    #[cfg(test)]
    pub(in crate::module_candidates) fn structural_groups(
        &mut self,
        groups: &[StructuralGroup],
    ) -> Option<Value> {
        self.encode_groups(groups.iter().map(GroupView::Structural))
    }

    #[cfg(test)]
    fn encode_groups<'a>(
        &mut self,
        groups: impl Iterator<Item = GroupView<'a>> + Clone + ExactSizeIterator,
    ) -> Option<Value> {
        if groups.len() > LIST_LIMIT {
            return None;
        }
        let mut references = self.references_emitted;
        for group in groups.clone() {
            group.check().ok()?;
            charge_references(&mut references, group.binders().len(), group.globals_len()).ok()?;
            for binder in group.binders() {
                self.symbols.get(binder)?;
            }
            for key in group.keys() {
                self.globals.get(&key.ok()?)?;
            }
        }
        let mut rows = Vec::with_capacity(groups.len());
        for group in groups {
            let binders = group
                .binders()
                .iter()
                .map(|binder| Some(Value::Integer((*self.symbols.get(binder)? as u64).into())))
                .collect::<Option<Vec<_>>>()?;
            let globals = group
                .keys()
                .map(|key| {
                    Some(Value::Integer(
                        (*self.globals.get(&key.ok()?)? as u64).into(),
                    ))
                })
                .collect::<Option<Vec<_>>>()?;
            rows.push(Value::Array(vec![
                Value::Integer(group.ordinal().into()),
                Value::Array(binders),
                Value::Array(globals),
            ]));
        }
        self.references_emitted = references;
        Some(Value::Array(rows))
    }

    fn finish(self) -> WireTables {
        WireTables {
            symbols: Value::Array(self.symbol_rows),
            globals: Value::Array(self.global_rows),
        }
    }

    #[cfg(test)]
    pub(super) fn into_wire_tables(self) -> (Value, Value) {
        (
            Value::Array(self.symbol_rows),
            Value::Array(self.global_rows),
        )
    }

    fn intern_symbol(&mut self, identity: &SymbolIdentity) -> InventoryResult<usize> {
        if let Some(entry) = self.symbols.get(identity) {
            return Ok(*entry);
        }
        check_budget(
            InventoryResource::SymbolEntries,
            self.symbol_rows.len() + 1,
            TABLE_LIMIT,
        )?;
        identity_size(identity)?;
        let value = identity_value(identity);
        let index = self.symbol_rows.len();
        self.symbol_rows.push(value);
        self.symbols.insert(identity.clone(), index);
        Ok(index)
    }

    fn intern_global(&mut self, key: GlobalKey) -> InventoryResult<usize> {
        if let Some(entry) = self.globals.get(&key) {
            return Ok(*entry);
        }
        check_budget(
            InventoryResource::GlobalEntries,
            self.global_rows.len() + 1,
            TABLE_LIMIT,
        )?;
        let symbol = self.intern_symbol(&key.identity)?;
        let identity_bytes = identity_size(&key.identity)?;
        let tail = vec![
            rep_value(key.rep),
            key.signature.as_ref().map_or(Value::Null, signature_value),
            Value::Bool(key.evaluated),
            key.generation
                .map_or(Value::Null, |generation| Value::Integer(generation.into())),
        ];
        let mut bytes = array_size(5)
            .checked_add(identity_bytes)
            .ok_or(InventoryEncodingError::ArithmeticOverflow)?;
        for value in &tail {
            charge_bytes(&mut bytes, encoded_size(value)?)?;
        }
        let index = self.global_rows.len();
        let mut row = Vec::with_capacity(5);
        row.push(Value::Integer((symbol as u64).into()));
        row.extend(tail);
        self.global_rows.push(Value::Array(row));
        self.globals.insert(key, index);
        Ok(index)
    }
}

fn global_key(
    group: &ProjectedGroup,
    global: &tidepool_repr::execution_schema::GlobalDecl,
) -> InventoryResult<GlobalKey> {
    identity_size(&global.identity)?;
    let signature = match global.entry_signature {
        None => None,
        Some(id) => {
            let definitions = group.definitions();
            let signature = definitions.signatures().get(id.0 as usize).ok_or(
                InventoryEncodingError::InvalidSignatureIndex {
                    index: id.0 as usize,
                },
            )?;
            validate_signature(signature)?;
            Some(signature.clone())
        }
    };
    Ok(GlobalKey {
        identity: global.identity.clone(),
        rep: global.rep,
        signature,
        evaluated: global.required_evaluated,
        generation: global.required_generation,
    })
}

fn validate_signature(signature: &Signature) -> InventoryResult<()> {
    check_budget(
        InventoryResource::SignatureArguments,
        signature.arguments.len(),
        SIGNATURE_LIMIT,
    )?;
    if let super::ResultContract::Returns(results) = &signature.results {
        check_budget(
            InventoryResource::SignatureResults,
            results.len(),
            SIGNATURE_LIMIT,
        )?;
    }
    Ok(())
}

#[cfg(test)]
fn validate_key(key: &GlobalKey) -> InventoryResult<()> {
    identity_size(&key.identity)?;
    if let Some(signature) = &key.signature {
        validate_signature(signature)?;
    }
    Ok(())
}
/// Reference count is an allocation safeguard derived from the actual encoded
/// manifest bound: every binder/global index occupies at least one CBOR byte.
fn charge_references(total: &mut usize, binders: usize, globals: usize) -> InventoryResult<()> {
    let observed = total
        .checked_add(binders)
        .and_then(|n| n.checked_add(globals))
        .ok_or(InventoryEncodingError::ArithmeticOverflow)?;
    check_budget(InventoryResource::References, observed, MANIFEST_LIMIT)?;
    *total = observed;
    Ok(())
}

fn charge_bytes(total: &mut usize, bytes: usize) -> InventoryResult<()> {
    let observed = total
        .checked_add(bytes)
        .ok_or(InventoryEncodingError::ArithmeticOverflow)?;
    check_budget(
        InventoryResource::UniqueEntryBytes,
        observed,
        MANIFEST_LIMIT,
    )?;
    *total = observed;
    Ok(())
}
fn identity_size(identity: &SymbolIdentity) -> InventoryResult<usize> {
    let mut bytes = array_size(5);
    for text in [
        &identity.unit,
        &identity.module,
        &identity.namespace,
        &identity.occurrence,
    ] {
        charge_bytes(
            &mut bytes,
            uint_size(text.len() as u64)
                .checked_add(text.len())
                .ok_or(InventoryEncodingError::ArithmeticOverflow)?,
        )?;
    }
    let parent = match &identity.record_parent {
        None => 1,
        Some(text) => uint_size(text.len() as u64)
            .checked_add(text.len())
            .ok_or(InventoryEncodingError::ArithmeticOverflow)?,
    };
    charge_bytes(&mut bytes, parent)?;
    Ok(bytes)
}
fn array_size(items: usize) -> usize {
    uint_size(items as u64)
}
fn uint_size(value: u64) -> usize {
    match value {
        0..=23 => 1,
        24..=255 => 2,
        256..=65535 => 3,
        65536..=4294967295 => 5,
        _ => 9,
    }
}
struct SizeWriter {
    bytes: usize,
    refusal: Option<InventoryEncodingError>,
}
impl Write for SizeWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Err(error) = charge_bytes(&mut self.bytes, bytes.len()) {
            self.refusal = Some(error);
            return Err(io::Error::other("candidate inventory entry budget refused"));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn encoded_size(value: &Value) -> InventoryResult<usize> {
    let mut writer = SizeWriter {
        bytes: 0,
        refusal: None,
    };
    if ciborium::ser::into_writer(value, &mut writer).is_err() {
        return Err(writer.refusal.unwrap_or(InventoryEncodingError::Encoding));
    }
    Ok(writer.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::execution_schema::{testing, GlobalDecl, ResultContract, SignatureId};

    fn group(
        signature: Option<Signature>,
        evaluated: bool,
        generation: Option<u64>,
        ordinal: u32,
    ) -> ProjectedGroup {
        let mut wire = testing::wire_program();
        let entry_signature = signature.map(|signature| {
            wire.signatures.push(signature);
            SignatureId((wire.signatures.len() - 1) as u32)
        });
        wire.globals.push(GlobalDecl {
            identity: testing::identity("External", "same"),
            rep: RuntimeRep::LiftedRef,
            entry_signature,
            required_evaluated: evaluated,
            required_generation: generation,
        });
        testing::projected_group(wire, ordinal).unwrap()
    }
    fn expand(tables: &InventoryTables, value: &Value) -> Value {
        Value::Array(
            value
                .as_array()
                .unwrap()
                .iter()
                .map(|group| {
                    let fields = group.as_array().unwrap();
                    let symbols = fields[1]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|index| {
                            tables.symbol_rows
                                [usize::try_from(index.as_integer().unwrap()).unwrap()]
                            .clone()
                        })
                        .collect();
                    let globals = fields[2]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|index| {
                            let mut row = tables.global_rows
                                [usize::try_from(index.as_integer().unwrap()).unwrap()]
                            .as_array()
                            .unwrap()
                            .clone();
                            row[0] = tables.symbol_rows
                                [usize::try_from(row[0].as_integer().unwrap()).unwrap()]
                            .clone();
                            Value::Array(row)
                        })
                        .collect();
                    Value::Array(vec![
                        fields[0].clone(),
                        Value::Array(symbols),
                        Value::Array(globals),
                    ])
                })
                .collect(),
        )
    }
    fn metadata(label: &str) -> CandidateManifestMetadata {
        CandidateManifestMetadata {
            unit: "main".into(),
            module: label.into(),
            source: "/fixture/source.hs".into(),
            source_sha256: "0".repeat(64),
            interface: "/fixture/interface.hi".into(),
            interface_sha256: "1".repeat(64),
            module_version: "2".repeat(64),
            product_sha256: "3".repeat(64),
            evidence_sha256: "4".repeat(64),
            imports: vec![],
            packages: "/fixture/packages".into(),
            packages_sha256: "5".repeat(64),
            product: "/fixture/product.tpmod".into(),
            canonical_requirements: BTreeMap::new(),
            certificate: "/fixture/certificate.cbor".into(),
            certificate_sha256: [6; 32],
            core: "/fixture/core".into(),
            core_sha256: [7; 32],
        }
    }

    fn legacy_row(metadata: &CandidateManifestMetadata, groups: Value) -> Value {
        super::super::candidate_manifest_row(super::super::CandidateManifestRow {
            unit: &metadata.unit,
            module: &metadata.module,
            source: &metadata.source,
            source_sha256: &metadata.source_sha256,
            interface: &metadata.interface,
            interface_sha256: &metadata.interface_sha256,
            module_version: &metadata.module_version,
            product_sha256: &metadata.product_sha256,
            evidence_sha256: &metadata.evidence_sha256,
            imports: metadata.imports.clone(),
            groups,
            packages: &metadata.packages,
            packages_sha256: &metadata.packages_sha256,
            product: &metadata.product,
            canonical_requirements: &metadata.canonical_requirements,
            certificate: &metadata.certificate,
            certificate_sha256: &metadata.certificate_sha256,
            core: &metadata.core,
            core_sha256: &metadata.core_sha256,
        })
    }

    fn encode_finished(manifest: FinishedCandidateManifest) -> Vec<u8> {
        manifest
            .into_manifest(vec![], vec![], "producer")
            .encode_with_acquisition(|_| {
                Ok((Value::Array(vec![Value::Text("fresh-files".into())]), ()))
            })
            .unwrap()
            .0
    }

    #[test]
    fn prepared_inventory_preserves_wire_bytes_and_resolves_each_occurrence_once() {
        // Vary full semantic keys independently of group order and optional rows.
        for reverse in [false, true] {
            for evaluated in [false, true] {
                for generation in [None, Some(0), Some(9)] {
                    let signature = Some(Signature {
                        arguments: vec![RuntimeRep::LiftedRef],
                        results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
                    });
                    let mut first = vec![
                        group(None, false, None, 7),
                        group(signature.clone(), evaluated, generation, 2),
                        group(signature, evaluated, generation, 7),
                    ];
                    if reverse {
                        first.reverse();
                    }
                    let inputs = vec![
                        ("first", Some(first)),
                        ("availability", None),
                        ("empty", Some(vec![])),
                        ("last", Some(vec![group(None, true, Some(1), 99)])),
                    ];
                    let all_groups = inputs.iter().filter_map(|(_, groups)| groups.as_ref());
                    let mut old = InventoryTables::new(all_groups.flatten()).unwrap();
                    let expected_rows = inputs
                        .iter()
                        .map(|(label, groups)| {
                            (
                                *label,
                                groups.as_ref().map(|groups| old.groups(groups).unwrap()),
                            )
                        })
                        .collect::<Vec<_>>();
                    let expected_work = InventoryWork {
                        group_rows: inputs
                            .iter()
                            .filter_map(|(_, groups)| groups.as_ref())
                            .map(Vec::len)
                            .sum(),
                        binder_references: inputs
                            .iter()
                            .filter_map(|(_, groups)| groups.as_ref())
                            .flatten()
                            .map(|group| group.binders().len())
                            .sum(),
                        global_key_resolutions: inputs
                            .iter()
                            .filter_map(|(_, groups)| groups.as_ref())
                            .flatten()
                            .map(|group| group.globals().len())
                            .sum(),
                    };
                    let prepared =
                        PreparedInventory::new(inputs, |(_, groups)| groups.as_deref()).unwrap();
                    assert_eq!(prepared.work, expected_work);
                    let finished = prepared
                        .try_finish(|(label, groups)| {
                            Some(match groups {
                                Some(_) => ManifestDisposition::Emit(metadata(label)),
                                None => ManifestDisposition::AvailabilityOnly,
                            })
                        })
                        .unwrap();
                    let (symbols, globals) = old.into_wire_tables();
                    let rows = expected_rows
                        .into_iter()
                        .filter_map(|(label, groups)| {
                            groups.map(|groups| legacy_row(&metadata(label), groups))
                        })
                        .collect();
                    let expected = super::super::candidate_manifest_value(
                        symbols,
                        globals,
                        rows,
                        vec![],
                        vec![],
                        "producer",
                    );
                    let mut expected_bytes = Vec::new();
                    ciborium::ser::into_writer(&expected, &mut expected_bytes).unwrap();
                    assert_eq!(encode_finished(finished), expected_bytes);
                }
            }
        }
    }

    #[test]
    fn prepared_inventory_keeps_independent_tables_paired_through_encoding() {
        let inputs = |generation| vec![("candidate", vec![group(None, false, generation, 7)])];
        let left = PreparedInventory::new(inputs(Some(1)), |(_, groups)| Some(groups)).unwrap();
        let right = PreparedInventory::new(inputs(Some(2)), |(_, groups)| Some(groups)).unwrap();
        // Both private group rows use the same indices, but those indices name
        // different complete facts in their respective private tables.
        assert_eq!(left.rows[0].1, right.rows[0].1);
        assert_ne!(left.tables.globals, right.tables.globals);
        let finish = |owner: PreparedInventory<(&str, Vec<ProjectedGroup>)>| {
            owner
                .try_finish(|(label, _)| Some(ManifestDisposition::Emit(metadata(label))))
                .unwrap()
        };
        let left_bytes = encode_finished(finish(left));
        let right_bytes = encode_finished(finish(right));
        assert_ne!(left_bytes, right_bytes);
        let decode = |bytes: &[u8]| -> Value { ciborium::de::from_reader(bytes).unwrap() };
        let left = decode(&left_bytes);
        let right = decode(&right_bytes);
        let left = left.as_array().unwrap();
        let right = right.as_array().unwrap();
        assert_eq!(
            left[4], right[4],
            "metadata and matching index rows are identical"
        );
        assert_ne!(
            left[3], right[3],
            "complete global facts retain their own generation"
        );
    }

    #[test]
    fn prepared_inventory_enforces_manifest_disposition_and_propagates_errors() {
        let manifest = || {
            PreparedInventory::new([vec![group(None, false, None, 7)]], |groups| Some(groups))
                .unwrap()
        };
        let availability = || PreparedInventory::new([()], |_| None).unwrap();
        assert!(matches!(
            manifest().try_finish(|_| Some(ManifestDisposition::AvailabilityOnly)),
            Err(InventoryEncodingError::MissingManifestMetadata)
        ));
        assert!(matches!(
            availability().try_finish(|_| Some(ManifestDisposition::Emit(metadata("wrong")))),
            Err(InventoryEncodingError::UnexpectedManifestMetadata)
        ));
        assert!(matches!(
            availability().try_finish(|_| Some(ManifestDisposition::Omit)),
            Err(InventoryEncodingError::UnexpectedManifestMetadata)
        ));
        assert!(matches!(
            manifest().try_finish(|_| None),
            Err(InventoryEncodingError::Preparation)
        ));
        assert!(availability()
            .try_finish(|_| Some(ManifestDisposition::AvailabilityOnly))
            .unwrap()
            .rows
            .is_empty());
    }

    #[test]
    fn omitted_metadata_preserves_legacy_tables_rows_and_subsequent_indices() {
        for missing_hash in [false, true] {
            for missing_module in [false, true] {
                for reverse in [false, true] {
                    let mut inputs = vec![
                        (
                            "before",
                            Some(vec![group(None, false, Some(1), 4)]),
                            true,
                            true,
                        ),
                        (
                            "omitted",
                            Some(vec![group(None, true, Some(2), 9)]),
                            !missing_hash,
                            !missing_module,
                        ),
                        ("availability", None, true, true),
                        ("empty", Some(vec![]), true, true),
                        (
                            "after",
                            Some(vec![group(None, false, Some(3), 1)]),
                            true,
                            true,
                        ),
                    ];
                    if reverse {
                        inputs.reverse();
                    }
                    // Independent old route: all non-availability inputs build
                    // tables before the two metadata skips; only emitted rows
                    // resolve group indices. No new owner helper builds this oracle.
                    let mut old = InventoryTables::new(
                        inputs.iter().filter_map(|row| row.1.as_ref()).flatten(),
                    )
                    .unwrap();
                    let mut rows = Vec::new();
                    for (label, groups, hash, matched) in &inputs {
                        let Some(groups) = groups else {
                            continue;
                        };
                        if !hash {
                            continue;
                        }
                        if !matched {
                            continue;
                        }
                        rows.push(legacy_row(&metadata(label), old.groups(groups).unwrap()));
                    }
                    let (symbols, globals) = old.into_wire_tables();
                    let legacy = super::super::candidate_manifest_value(
                        symbols,
                        globals,
                        rows,
                        vec![],
                        vec![],
                        "producer",
                    );
                    let mut expected = Vec::new();
                    ciborium::ser::into_writer(&legacy, &mut expected).unwrap();
                    let prepared = PreparedInventory::new(inputs, |row| row.1.as_deref()).unwrap();
                    let finished = prepared
                        .try_finish(|(label, groups, hash, matched)| {
                            if groups.is_none() {
                                return Some(ManifestDisposition::AvailabilityOnly);
                            }
                            let evidence =
                                manifest_source_evidence(hash.then(|| "digest".into()), || {
                                    matched.then_some(())
                                });
                            Some(match evidence {
                                Some(_) => ManifestDisposition::Emit(metadata(label)),
                                None => ManifestDisposition::Omit,
                            })
                        })
                        .unwrap();
                    assert_eq!(encode_finished(finished), expected,
                        "missing hash={missing_hash}, missing module={missing_module}, reversed={reverse}");
                }
            }
        }
        let called = std::cell::Cell::new(false);
        assert!(manifest_source_evidence::<()>(None, || {
            called.set(true);
            Some(())
        })
        .is_none());
        assert!(
            !called.get(),
            "missing digest keeps the original short-circuit order"
        );
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(64))]
        #[test]
        fn generated_inventory_dispositions_match_legacy_bytes(
            rows in proptest::collection::vec((0u8..8, proptest::bool::ANY, proptest::bool::ANY, proptest::bool::ANY), 0..12)
        ) {
            let inputs = rows.iter().enumerate().map(|(index, (generation, available, hash, matched))| {
                (format!("row-{index}"), (!available).then(|| vec![group(None, index % 2 == 0, Some(u64::from(*generation)), index as u32)]), *hash, *matched)
            }).collect::<Vec<_>>();
            let mut old = InventoryTables::new(inputs.iter().filter_map(|row| row.1.as_ref()).flatten()).unwrap();
            let mut old_rows = Vec::new();
            for (label, groups, hash, matched) in &inputs {
                let Some(groups) = groups else { continue; };
                if !hash || !matched { continue; }
                old_rows.push(legacy_row(&metadata(label), old.groups(groups).unwrap()));
            }
            let (symbols, globals) = old.into_wire_tables();
            let old_manifest = super::super::candidate_manifest_value(symbols, globals, old_rows, vec![], vec![], "producer");
            let mut old_bytes = Vec::new();
            ciborium::ser::into_writer(&old_manifest, &mut old_bytes).unwrap();
            let owner = PreparedInventory::new(inputs, |row| row.1.as_deref()).unwrap();
            let finished = owner.try_finish(|(label, groups, hash, matched)| Some(
                if groups.is_none() { ManifestDisposition::AvailabilityOnly }
                else if manifest_source_evidence(hash.then(|| "digest".into()), || matched.then_some(())).is_some() {
                    ManifestDisposition::Emit(metadata(&label))
                } else { ManifestDisposition::Omit }
            )).unwrap();
            proptest::prop_assert_eq!(encode_finished(finished), old_bytes);
        }
    }

    #[test]
    fn omitted_group_count_keeps_legacy_table_contribution_without_emission() {
        let groups = vec![group(None, false, None, 7); LIST_LIMIT + 1];
        let old = InventoryTables::new(groups.iter()).unwrap();
        let (symbols, globals) = old.into_wire_tables();
        let legacy = super::super::candidate_manifest_value(
            symbols,
            globals,
            vec![],
            vec![],
            vec![],
            "producer",
        );
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&legacy, &mut bytes).unwrap();
        let prepared = PreparedInventory::new([groups.clone()], |groups| Some(groups)).unwrap();
        assert_eq!(
            encode_finished(
                prepared
                    .try_finish(|_| Some(ManifestDisposition::Omit))
                    .unwrap()
            ),
            bytes
        );
        let emitted = PreparedInventory::new([groups], |groups| Some(groups)).unwrap();
        assert!(
            matches!(emitted.try_finish(|_| Some(ManifestDisposition::Emit(metadata("too-many")))),
            Err(InventoryEncodingError::Budget { resource: InventoryResource::GroupCount, observed, limit })
                if observed == LIST_LIMIT + 1 && limit == LIST_LIMIT)
        );
    }

    #[test]
    fn completed_manifest_preserves_acquisition_errors_and_wire_budget() {
        // Each completed owner is consumed exactly once; acquisition refusal
        // remains the original typed interruption rather than an empty offer.
        let make = || {
            PreparedInventory::new([()], |_| None)
                .unwrap()
                .try_finish(|_| Some(ManifestDisposition::AvailabilityOnly))
                .unwrap()
                .into_manifest(vec![], vec![], "producer")
        };
        let error = make()
            .encode_with_acquisition::<()>(|_| Err(InventoryEncodingError::Acquisition))
            .unwrap_err();
        assert_eq!(error, InventoryEncodingError::Acquisition);
        // Determine the fixed envelope cost independently with the legacy codec,
        // then exercise the production owner's exact inclusive byte boundary.
        let (symbols, globals) = InventoryTables::new(std::iter::empty())
            .unwrap()
            .into_wire_tables();
        let mut legacy = super::super::candidate_manifest_value(
            symbols,
            globals,
            vec![],
            vec![],
            vec![],
            "producer",
        );
        let Value::Array(ref mut fields) = legacy else {
            unreachable!()
        };
        fields[7] = Value::Bytes(vec![0; MANIFEST_LIMIT / 2]);
        let mut baseline = Vec::new();
        ciborium::ser::into_writer(&legacy, &mut baseline).unwrap();
        let overhead = baseline.len() - MANIFEST_LIMIT / 2;
        for delta in [-1isize, 0, 1] {
            let payload = (MANIFEST_LIMIT - overhead)
                .checked_add_signed(delta)
                .unwrap();
            let result =
                make().encode_with_acquisition(|_| Ok((Value::Bytes(vec![0; payload]), ())));
            if delta <= 0 {
                let (bytes, ()) = result.unwrap();
                assert_eq!(
                    bytes.len(),
                    MANIFEST_LIMIT.checked_add_signed(delta).unwrap()
                );
            } else {
                assert!(matches!(result, Err(InventoryEncodingError::Budget {
                    resource: InventoryResource::ManifestBytes, observed, limit,
                }) if observed == MANIFEST_LIMIT + 1 && limit == MANIFEST_LIMIT));
            }
        }
        let oversized = make()
            .encode_with_acquisition(|_| Ok((Value::Text("x".repeat(MANIFEST_LIMIT)), ())))
            .unwrap_err();
        assert!(matches!(oversized, InventoryEncodingError::Budget {
            resource: InventoryResource::ManifestBytes, observed, limit,
        } if observed > limit && limit == MANIFEST_LIMIT));
    }

    #[test]
    fn prepared_inventory_reports_first_owner_budget_and_overflow() {
        assert_eq!(
            charge_references(&mut 0, MANIFEST_LIMIT, 1),
            Err(InventoryEncodingError::Budget {
                resource: InventoryResource::References,
                observed: MANIFEST_LIMIT + 1,
                limit: MANIFEST_LIMIT,
            })
        );
        assert_eq!(
            charge_references(&mut usize::MAX, 1, 0),
            Err(InventoryEncodingError::ArithmeticOverflow)
        );
        let signature = Signature {
            arguments: vec![RuntimeRep::LiftedRef; SIGNATURE_LIMIT + 1],
            results: ResultContract::NoSuccess,
        };
        assert_eq!(
            validate_signature(&signature),
            Err(InventoryEncodingError::Budget {
                resource: InventoryResource::SignatureArguments,
                observed: SIGNATURE_LIMIT + 1,
                limit: SIGNATURE_LIMIT,
            })
        );
    }

    #[test]
    fn interned_inventory_reconstructs_full_groups_in_order() {
        let groups = vec![group(None, false, None, 7), group(None, false, None, 2)];
        let mut tables = InventoryTables::new(groups.iter()).unwrap();
        assert_eq!(tables.symbol_rows.len(), 2);
        assert_eq!(tables.global_rows.len(), 1);
        let indexed = tables.groups(&groups).unwrap();
        let expanded = Value::Array(groups.iter().map(super::super::group_inventory).collect());
        assert_eq!(expand(&tables, &indexed), expanded);
        assert_eq!(
            tables.references_emitted,
            groups
                .iter()
                .map(|group| group.binders().len() + group.globals().len())
                .sum::<usize>()
        );
        let other = InventoryTables::new(groups.iter()).unwrap();
        assert_eq!(tables.into_wire_tables(), other.into_wire_tables());
    }
    #[test]
    fn global_interning_keeps_complete_signature_and_requirements() {
        let signature = |results| {
            Some(Signature {
                arguments: vec![RuntimeRep::LiftedRef],
                results,
            })
        };
        let groups = vec![
            group(None, false, None, 0),
            group(signature(ResultContract::Returns(vec![])), false, None, 1),
            group(signature(ResultContract::NoSuccess), false, None, 2),
            group(signature(ResultContract::CallerResult), false, None, 3),
            group(signature(ResultContract::Returns(vec![])), true, None, 4),
            group(
                signature(ResultContract::Returns(vec![])),
                false,
                Some(9),
                5,
            ),
        ];
        let mut tables = InventoryTables::new(groups.iter()).unwrap();
        assert_eq!(tables.global_rows.len(), groups.len());
        let indexed = tables.groups(&groups).unwrap();
        assert_eq!(
            expand(&tables, &indexed),
            Value::Array(groups.iter().map(super::super::group_inventory).collect())
        );
    }
    #[test]
    fn symbol_interning_keeps_record_parent_and_owner() {
        let mut tables = InventoryTables::new(std::iter::empty()).unwrap();
        let identity = testing::identity("Module", "same");
        let mut record = identity.clone();
        record.record_parent = Some("Record".into());
        let mut unit = identity.clone();
        unit.unit = "other-unit".into();
        let mut namespace = identity.clone();
        namespace.namespace = "other-namespace".into();
        for value in [&identity, &record, &unit, &namespace] {
            tables.intern_symbol(value).unwrap();
        }
        assert_eq!(tables.symbol_rows.len(), 4);
        assert_eq!(tables.intern_symbol(&identity).unwrap(), 0);
    }
    #[test]
    fn reference_budget_counts_all_lists_at_limit_and_checks_overflow() {
        let groups = vec![group(None, false, None, 0)];
        let references = groups[0].binders().len() + groups[0].globals().len();
        let mut tables = InventoryTables::new(groups.iter()).unwrap();
        tables.references_emitted = MANIFEST_LIMIT - references;
        assert!(tables.groups(&groups).is_some());
        assert_eq!(tables.references_emitted, MANIFEST_LIMIT);
        assert!(tables.groups(&groups).is_none());
        assert_eq!(tables.references_emitted, MANIFEST_LIMIT);
        let repeated = std::iter::repeat(&groups[0]).take(MANIFEST_LIMIT / references + 1);
        assert!(InventoryTables::new(repeated).is_none());
        let mut maximum = usize::MAX;
        assert!(charge_references(&mut maximum, 1, 0).is_err());
        assert!(charge_references(&mut 0, usize::MAX, 1).is_err());
        let mut exact = 0;
        assert!(charge_references(&mut exact, MANIFEST_LIMIT / 2, MANIFEST_LIMIT / 2).is_ok());
        assert_eq!(exact, MANIFEST_LIMIT);
        assert!(charge_references(&mut exact, 0, 1).is_err());
    }

    #[test]
    fn compact_shared_inventory_does_not_charge_repeated_semantic_bytes() {
        let mut identity = testing::identity("Fixture", "entry");
        identity.occurrence = "x".repeat(2048);
        let key = GlobalKey {
            identity: identity.clone(),
            rep: RuntimeRep::LiftedRef,
            signature: Some(Signature {
                arguments: vec![RuntimeRep::LiftedRef; SIGNATURE_LIMIT],
                results: ResultContract::Returns(vec![RuntimeRep::Int(64); SIGNATURE_LIMIT]),
            }),
            evaluated: false,
            generation: None,
        };
        let groups = vec![StructuralGroup {
            ordinal: 0,
            binders: vec![identity.clone(); 1536],
            globals: vec![key; 1536],
        }];
        // Repeated identity bytes alone exceeded the obsolete semantic-expansion
        // limit; the wire format stores one identity and bounded index lists.
        assert!(identity_size(&identity).unwrap() * 1536 * 2 > MANIFEST_LIMIT);
        let mut tables = InventoryTables::structural(&groups).unwrap();
        assert_eq!(tables.symbol_rows.len(), 1);
        assert_eq!(tables.global_rows.len(), 1);
        let indexed = tables.structural_groups(&groups).unwrap();
        assert_eq!(tables.references_emitted, 3072);
        let (symbols, globals) = tables.into_wire_tables();
        assert!(
            encoded_size(&Value::Array(vec![symbols, globals, indexed])).unwrap() < MANIFEST_LIMIT
        );
    }

    #[test]
    fn unique_inventory_entry_byte_bound_is_preserved() {
        let mut identity = testing::identity("Fixture", "entry");
        identity.occurrence = "x".repeat(MANIFEST_LIMIT);
        let mut tables = InventoryTables::structural(&[]).unwrap();
        assert!(tables.intern_symbol(&identity).is_err());
        assert!(tables.symbol_rows.is_empty());
        let mut maximum = usize::MAX;
        assert!(charge_bytes(&mut maximum, 1).is_err());
    }
}
