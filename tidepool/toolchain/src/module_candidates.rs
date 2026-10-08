//! Bounded durable storage for worker-certified module products. Records are
//! candidates only; the resident compiler must recheck their GHC semantics.

use ciborium::value::Value;
use serde::{Deserialize, Serialize};

mod candidate_diagnostics;
#[cfg(test)]
mod codec_measurement;
pub(crate) mod dependencies;
pub(crate) mod deployment;
#[cfg(test)]
mod fixture_packets;
mod inventory;
#[cfg(test)]
mod product_decode_observer;
pub(crate) mod shared_evidence;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tidepool_repr::execution_schema::{
    CachedHomeOwner, InventoryDecodeLimits, ModuleVersion, ProjectedGroup, RawModuleProduct,
    ResultContract, RuntimeRep, Signature, SymbolIdentity,
};

use crate::cache::{DependencyEvidence, ProductAvailability};

pub(crate) const RECORD_LIMIT: usize = 32 << 20;
const MANIFEST_LIMIT: usize = 4 << 20;
const CANDIDATE_LIMIT: usize = 128;
const RECORD_DIR: &str = "module-candidates-v12";
const RECORD_MAGIC: &[u8; 8] = b"TPCRE10\n";
const RECORD_VERSION: u32 = 10;
const HEADER_LIMIT: usize = 64 << 10;
const PAYLOAD_LIMIT: usize = 128 << 20;
// A measured 33,955,557-byte ordinary resident display graph fits within one
// module's bound. The enclosing inventory can contain many such owners.
pub(crate) const PRODUCT_MODULE_MAX_BYTES: usize = 64 << 20;

pub(crate) fn product_decode_limits() -> InventoryDecodeLimits {
    InventoryDecodeLimits {
        max_module_bytes: PRODUCT_MODULE_MAX_BYTES,
        ..InventoryDecodeLimits::default()
    }
}

/// Preserve each original TPMOD row in its own bounded sidecar. A module's
/// product identity must not change when an unrelated module is compiled in
/// the same worker request.
#[cfg(test)]
fn split_module_product_bytes(bytes: &[u8], products: &[RawModuleProduct]) -> Option<Vec<Vec<u8>>> {
    if bytes.len() > product_decode_limits().max_bytes {
        return None;
    }
    let Value::Array(header) = ciborium::de::from_reader::<Value, _>(bytes).ok()? else {
        return None;
    };
    let [Value::Text(magic), Value::Integer(version), Value::Array(rows)] =
        <[Value; 3]>::try_from(header).ok()?
    else {
        return None;
    };
    if magic != "TPMOD" || version != 1.into() || rows.len() != products.len() {
        return None;
    }
    rows.into_iter()
        .zip(products)
        .map(|(row, product)| {
            let Value::Array(fields) = &row else {
                return None;
            };
            if !matches!(fields.as_slice(),
                [Value::Text(unit), Value::Text(module), Value::Bytes(_), Value::Array(_)]
                if unit == &product.unit && module == &product.module)
            {
                return None;
            }
            let sidecar = Value::Array(vec![
                Value::Text("TPMOD".into()),
                Value::Integer(1.into()),
                Value::Array(vec![row]),
            ]);
            let mut encoded = Vec::new();
            ciborium::ser::into_writer(&sidecar, &mut encoded).ok()?;
            (encoded.len() <= RECORD_LIMIT).then_some(encoded)
        })
        .collect()
}

/// The worker emits one sealed package-import witness for each fresh product.
/// Keep its original bytes: candidate identity and recovery must bind the same
/// direct selections that GHC actually loaded in this transaction.
pub(crate) fn split_package_imports(
    bytes: &[u8],
    products: &[RawModuleProduct],
) -> Option<BTreeMap<(String, String), Vec<u8>>> {
    split_package_imports_with_operation(
        bytes,
        products,
        &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
    )
    .ok()
    .flatten()
}

pub(crate) fn split_package_imports_with_operation(
    bytes: &[u8],
    products: &[RawModuleProduct],
    operation: &tidepool_repr::execution_schema::InventoryOperation,
) -> Result<Option<BTreeMap<(String, String), Vec<u8>>>, tidepool_repr::execution_schema::ParseError>
{
    let value = match operation.decode_value(bytes, operation.limits().max_bytes) {
        Ok(value) => value,
        Err(
            error @ tidepool_repr::execution_schema::ParseError::LimitExceeded(
                "work" | "accounting owner",
            ),
        )
        | Err(error @ tidepool_repr::execution_schema::ParseError::ByteLimit { .. }) => {
            return Err(error)
        }
        Err(_) => return Ok(None),
    };
    operation.charge_value_copies(&value, 1)?;
    operation.charge(bytes.len())?;
    struct OriginalEncoding<'a> {
        bytes: &'a [u8],
        position: usize,
    }
    impl std::io::Write for OriginalEncoding<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let end = self
                .position
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("canonical size overflow"))?;
            if self.bytes.get(self.position..end) != Some(bytes) {
                return Err(std::io::Error::other("noncanonical package bundle"));
            }
            self.position = end;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut original = OriginalEncoding { bytes, position: 0 };
    if ciborium::ser::into_writer(&value, &mut original).is_err()
        || original.position != bytes.len()
    {
        return Ok(None);
    }
    let decoded = (|| {
        let Value::Array(fields) = value else {
            return None;
        };
        let [Value::Text(magic), Value::Integer(version), Value::Array(rows)] =
            <[Value; 3]>::try_from(fields).ok()?
        else {
            return None;
        };
        if magic != "TPPKGBUNDLES" || version != 1.into() || rows.len() != products.len() {
            return None;
        }
        let mut by_owner = BTreeMap::new();
        for row in rows {
            let Value::Array(fields) = row else {
                return None;
            };
            let [Value::Text(unit), Value::Text(module), Value::Bytes(sidecar)] =
                <[Value; 3]>::try_from(fields).ok()?
            else {
                return None;
            };
            if sidecar.len() > (4 << 20)
                || !products
                    .iter()
                    .any(|product| product.unit == unit && product.module == module)
                || by_owner.insert((unit, module), sidecar).is_some()
            {
                return None;
            }
        }
        Some(by_owner)
    })();
    Ok(decoded)
}

fn identity_value(identity: &SymbolIdentity) -> Value {
    Value::Array(vec![
        Value::Text(identity.unit.clone()),
        Value::Text(identity.module.clone()),
        Value::Text(identity.namespace.clone()),
        Value::Text(identity.occurrence.clone()),
        identity
            .record_parent
            .clone()
            .map_or(Value::Null, Value::Text),
    ])
}

fn rep_value(rep: RuntimeRep) -> Value {
    let (tag, bits) = match rep {
        RuntimeRep::Void => ("void", 0),
        RuntimeRep::LiftedRef => ("lifted", 0),
        RuntimeRep::UnliftedRef => ("unlifted", 0),
        RuntimeRep::Address => ("address", 0),
        RuntimeRep::Int(bits) => ("int", bits),
        RuntimeRep::Word(bits) => ("word", bits),
        RuntimeRep::Float(bits) => ("float", bits),
    };
    Value::Array(vec![Value::Text(tag.into()), Value::Integer(bits.into())])
}

fn signature_value(signature: &Signature) -> Value {
    let (tag, results) = match &signature.results {
        ResultContract::Returns(results) => ("returns", results.as_slice()),
        ResultContract::NoSuccess => ("no_success", &[][..]),
        ResultContract::CallerResult => ("caller_result", &[][..]),
    };
    Value::Array(vec![
        Value::Array(signature.arguments.iter().copied().map(rep_value).collect()),
        Value::Array(vec![
            Value::Text(tag.into()),
            Value::Array(results.iter().copied().map(rep_value).collect()),
        ]),
    ])
}

#[cfg(test)]
fn group_inventory(group: &ProjectedGroup) -> Value {
    let signatures = group.definitions();
    Value::Array(vec![
        Value::Integer(group.original_ordinal().into()),
        Value::Array(group.binders().iter().map(identity_value).collect()),
        Value::Array(
            group
                .globals()
                .iter()
                .map(|global| {
                    Value::Array(vec![
                        identity_value(&global.identity),
                        rep_value(global.rep),
                        global
                            .entry_signature
                            .and_then(|id| signatures.signatures().get(id.0 as usize))
                            .map_or(Value::Null, signature_value),
                        Value::Bool(global.required_evaluated),
                        global
                            .required_generation
                            .map_or(Value::Null, |generation| Value::Integer(generation.into())),
                    ])
                })
                .collect(),
        ),
    ])
}

/// A singleton product and the exact bounded bytes that issued it. Construction
/// stays with the decoder; later certification borrows this immutable pairing.
#[derive(Debug)]
pub(crate) struct CandidateProduct {
    bytes: Vec<u8>,
    decoded: RawModuleProduct,
}

impl CandidateProduct {
    #[cfg(test)]
    fn decode_product(
        bytes: &[u8],
        requirements: &tidepool_repr::execution_schema::ProgramRequirements,
    ) -> Option<RawModuleProduct> {
        let operation =
            tidepool_repr::execution_schema::InventoryOperation::new(product_decode_limits());
        Self::decode_product_with_operation(bytes, requirements, &operation).ok()?
    }

    fn decode_product_with_operation(
        bytes: &[u8],
        requirements: &tidepool_repr::execution_schema::ProgramRequirements,
        operation: &tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<Option<RawModuleProduct>, tidepool_repr::execution_schema::ParseError> {
        #[cfg(test)]
        product_decode_observer::record();
        let mut products = operation.parse_module_products(bytes, requirements)?;
        if products.len() != 1 {
            return Ok(None);
        }
        Ok(products.pop())
    }

    #[cfg(test)]
    pub(crate) fn decode(bytes: Vec<u8>) -> Option<Self> {
        let requirements = crate::prepared_artifact::production_requirements().ok()?;
        let decoded = Self::decode_product(&bytes, &requirements)?;
        Some(Self { bytes, decoded })
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub(crate) fn decoded(&self) -> &RawModuleProduct {
        &self.decoded
    }
}

impl std::ops::Deref for CandidateProduct {
    type Target = RawModuleProduct;
    fn deref(&self) -> &Self::Target {
        &self.decoded
    }
}

#[derive(Debug)]
pub(crate) struct CandidateBundle {
    pub owner: CachedHomeOwner,
    pub product: CandidateProduct,
    pub source: PathBuf,
    pub source_sha256: String,
    pub iface_path: PathBuf,
    pub iface_sha256: String,
    pub package_imports_path: PathBuf,
    pub package_imports_sha256: String,
    pub package_imports_bytes: Vec<u8>,
    pub evidence: shared_evidence::SharedEvidence,
    pub target_source: String,
    pub origin: CandidateOrigin,
    pub original_execution: Option<OriginalCandidateExecution>,
    pub original_module_interface: crate::certified_products::CertifiedModuleInterface,
    pub(crate) execution_admitted: bool,
}

#[derive(Debug)]
pub(crate) struct CandidateSet {
    pub manifest_path: PathBuf,
    pub by_owner: BTreeMap<(String, String), CandidateBundle>,
}

/// Emit receipt-derived evidence only after products and target owners passed
/// certification. Bytes refer to original per-module TPMOD artifacts, including
/// their framing; this is not an estimate of compiler time or aggregate output.
pub(crate) fn record_deployment_acceptance(
    candidates: Option<&CandidateSet>,
    receipt: &crate::certified_products::CertifiedReceipt,
) {
    let Some(candidates) = candidates else { return };
    if !candidates
        .by_owner
        .values()
        .any(|bundle| matches!(bundle.origin, CandidateOrigin::Deployment { .. }))
    {
        return;
    }
    let accepted: Vec<_> = receipt
        .modules
        .iter()
        .filter_map(|module| {
            if module.origin != crate::certified_products::ProductOrigin::Cached {
                return None;
            }
            let bundle = candidates
                .by_owner
                .get(&(module.unit.clone(), module.module.clone()))?;
            matches!(bundle.origin, CandidateOrigin::Deployment { .. }).then_some(bundle)
        })
        .collect();
    tracing::info!(target: "tidepool_toolchain::module_candidates",
        deployment_accepted_modules = accepted.len() as u64,
        deployment_original_product_bytes = accepted.iter().map(|b| b.product.bytes().len() as u64).sum::<u64>(),
        "deployment module products accepted");
}

/// The original version recipe is retained, never inferred from a later offer.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum CandidateVersionOrigin {
    Ordinary,
    Exact { semantic_sha256: [u8; 32] },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OriginalOwner {
    unit: String,
    module: String,
    module_version: [u8; 32],
    skinny_iface_sha256: [u8; 32],
    product_sha256: [u8; 32],
}
impl OriginalOwner {
    fn from_owner(owner: &CachedHomeOwner) -> Self {
        Self {
            unit: owner.unit.clone(),
            module: owner.module.clone(),
            module_version: owner.module_version.0,
            skinny_iface_sha256: owner.skinny_iface_sha256,
            product_sha256: owner.product_sha256,
        }
    }
    fn owner(&self) -> CachedHomeOwner {
        CachedHomeOwner {
            unit: self.unit.clone(),
            module: self.module.clone(),
            module_version: ModuleVersion(self.module_version),
            skinny_iface_sha256: self.skinny_iface_sha256,
            product_sha256: self.product_sha256,
        }
    }
}

/// Constructed only after the original owner, Home seal and graph are verified.
#[derive(Clone, Debug)]
pub(crate) struct OriginalCandidateExecution {
    pub(crate) graph: Arc<crate::execution_source::CertifiedExecutionSourceGraph>,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(test, derive(Clone))]
#[serde(deny_unknown_fields)]
struct RecordData {
    tag: String,
    version: u32,
    #[serde(with = "opaque_bytes::endpoint")]
    endpoint: Vec<u8>,
    include: Vec<PathBuf>,
    evidence: shared_evidence::EvidenceRef,
    #[serde(with = "opaque_bytes")]
    products: Vec<u8>,
    unit: String,
    module: String,
    source: PathBuf,
    source_sha256: String,
    #[serde(with = "opaque_bytes")]
    interface: Vec<u8>,
    #[serde(with = "opaque_bytes::packages")]
    package_imports: Vec<u8>,
    target_source: String,
    version_origin: CandidateVersionOrigin,
    original_owner: OriginalOwner,
    #[serde(with = "opaque_bytes::packages")]
    original_certification: Vec<u8>,
    module_interface: Option<crate::recovery_artifacts::RecoveryModuleInterfaceRef>,
    execution_source_sha256: Option<[u8; 32]>,
}

/// The wire record is retained intact from issuance through bounded decoding.
/// Resolved proofs are a separate projection and never replace stored evidence.
#[derive(Debug)]
#[cfg_attr(test, derive(Clone))]
struct Record {
    data: RecordData,
    evidence: shared_evidence::SharedEvidence,
    module_interface_proof: Option<crate::certified_products::CertifiedModuleInterface>,
    execution_source: Option<Arc<crate::execution_source::CertifiedExecutionSourceGraph>>,
}

/// A deployment record carries the product decoded by its owning loader.
/// Ordinary durable records still decode at candidate acquisition.
enum CandidateRecord {
    Encoded(Record),
    Deployment(deployment::DecodedDeploymentRecord),
}

enum CandidateProductInput {
    Encoded,
    Decoded(RawModuleProduct),
}

impl From<Record> for CandidateRecord {
    fn from(record: Record) -> Self {
        Self::Encoded(record)
    }
}

impl CandidateRecord {
    fn record(&self) -> &Record {
        match self {
            Self::Encoded(record) => record,
            Self::Deployment(record) => record.record(),
        }
    }

    fn into_parts(self) -> (Record, CandidateProductInput) {
        match self {
            Self::Encoded(record) => (record, CandidateProductInput::Encoded),
            Self::Deployment(record) => {
                let (record, product) = record.into_parts();
                (record, CandidateProductInput::Decoded(product))
            }
        }
    }
}

impl std::ops::Deref for Record {
    type Target = RecordData;
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

#[cfg(test)]
impl std::ops::DerefMut for Record {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}

/// Candidate selection has authenticated both canonical and native products.
/// A selected record cannot lose its canonical interface after admission.
struct ValidatedRecord {
    data: RecordData,
    evidence: shared_evidence::SharedEvidence,
    canonical: crate::certified_products::CertifiedModuleInterface,
    execution_source: Option<Arc<crate::execution_source::CertifiedExecutionSourceGraph>>,
}

impl std::ops::Deref for ValidatedRecord {
    type Target = RecordData;
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl ValidatedRecord {
    fn admit(
        record: Record,
        canonical: crate::certified_products::CertifiedModuleInterface,
    ) -> Option<Self> {
        let unavailable = match record.module_interface.as_ref() {
            None => Some(CandidateInterfaceUnavailable::MissingCanonicalReference),
            Some(reference) if !canonical.matches_recovery_reference(reference) => {
                Some(CandidateInterfaceUnavailable::CanonicalReferenceMismatch)
            }
            Some(_) => None,
        };
        if let Some(reason) = unavailable {
            tracing::debug!(target: "tidepool_toolchain::module_candidates",
                unit = record.unit.as_str(), module = record.module.as_str(), ?reason,
                "candidate canonical descriptor admission refused");
            return None;
        }
        Some(Self {
            data: record.data,
            evidence: record.evidence,
            canonical,
            execution_source: record.execution_source,
        })
    }
}

/// Opaque compiler buffers are byte strings, never element-wise integer arrays.
/// Record readers bound the entire encoded payload before invoking serde;
/// ciborium grows buffers from consumed chunks, not an untrusted length hint.
mod opaque_bytes {
    use serde::{de, Deserializer, Serializer};

    struct Bytes<const LIMIT: usize>;

    impl<'de, const LIMIT: usize> de::Visitor<'de> for Bytes<LIMIT> {
        type Value = Vec<u8>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "a byte string of at most {LIMIT} bytes")
        }

        fn visit_bytes<E: de::Error>(self, value: &[u8]) -> Result<Self::Value, E> {
            if value.len() > LIMIT {
                return Err(E::custom("candidate byte string exceeds field limit"));
            }
            Ok(value.to_vec())
        }

        fn visit_byte_buf<E: de::Error>(self, value: Vec<u8>) -> Result<Self::Value, E> {
            if value.len() > LIMIT {
                return Err(E::custom("candidate byte string exceeds field limit"));
            }
            Ok(value)
        }
    }

    fn serialize_bounded<S: Serializer, const LIMIT: usize>(
        value: &[u8],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        if value.len() > LIMIT {
            return Err(serde::ser::Error::custom(
                "candidate byte string exceeds field limit",
            ));
        }
        serializer.serialize_bytes(value)
    }

    pub(super) fn serialize<S: Serializer>(value: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serialize_bounded::<S, { super::RECORD_LIMIT }>(value, serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        deserializer.deserialize_byte_buf(Bytes::<{ super::RECORD_LIMIT }>)
    }

    pub(super) mod endpoint {
        use super::*;
        pub(in super::super) fn serialize<S: Serializer>(
            value: &[u8],
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            serialize_bounded::<S, 4096>(value, serializer)
        }
        pub(in super::super) fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Vec<u8>, D::Error> {
            deserializer.deserialize_byte_buf(Bytes::<4096>)
        }
    }

    pub(super) mod packages {
        use super::*;
        pub(in super::super) fn serialize<S: Serializer>(
            value: &[u8],
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            serialize_bounded::<S, { 4 << 20 }>(value, serializer)
        }
        pub(in super::super) fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Vec<u8>, D::Error> {
            deserializer.deserialize_byte_buf(Bytes::<{ 4 << 20 }>)
        }
    }
}

fn sha(bytes: &[u8]) -> String {
    use std::fmt::Write;
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn absolute(path: &Path) -> Option<PathBuf> {
    let canonical = fs::canonicalize(path).ok()?;
    canonical.is_absolute().then_some(canonical)
}

pub(crate) fn context_paths(include: &[PathBuf]) -> Option<Vec<PathBuf>> {
    include.iter().map(|p| absolute(p)).collect()
}

fn record_dir(endpoint_identity: &[u8]) -> PathBuf {
    crate::paths::compile_cache_dir()
        .join(RECORD_DIR)
        .join(sha(endpoint_identity))
}

/// This bounded projection is discovery advice, never compilation authority.
/// The selected payload must authenticate it before ordinary record validation.
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RecordHeader {
    endpoint: Vec<u8>,
    include: Vec<PathBuf>,
    unit: String,
    module: String,
    source: PathBuf,
    payload_len: u64,
    payload_sha256: String,
}

impl RecordHeader {
    fn for_record(record: &RecordData, payload: &[u8]) -> Self {
        Self {
            endpoint: record.endpoint.clone(),
            include: record.include.clone(),
            unit: record.unit.clone(),
            module: record.module.clone(),
            source: record.source.clone(),
            payload_len: payload.len() as u64,
            payload_sha256: sha(payload),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PublicationDisposition {
    IncludeUnavailable,
    InvocationProofRejected,
    ProductSplitRejected,
    PackageBundleRejected,
    GenerationDependent,
    PackageWitnessMissing,
    PackageWitnessRejected,
    ProductOwnerRejected,
    ModuleEvidenceRejected,
    GeneratedRequestTarget,
    SourceUnavailable,
    SourceEvidenceRejected,
    Eligible,
    SourceRootUnavailable,
    DirectoryUnavailable,
    PayloadEncodingRejected,
    PayloadLimit,
    HeaderEncodingRejected,
    HeaderLimit,
    WriteRejected,
    Published,
    ExactContext,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RecordEncodingRejection {
    disposition: PublicationDisposition,
    payload_bytes: Option<usize>,
}

struct PublicationDiagnostics {
    remaining: usize,
}

impl PublicationDiagnostics {
    fn new() -> Self {
        Self {
            remaining: if std::env::var(crate::timing::TIMING_ENV).as_deref() == Ok("1") {
                CANDIDATE_LIMIT
            } else {
                0
            },
        }
    }

    fn report(
        &mut self,
        product: &RawModuleProduct,
        original_bytes: Option<usize>,
        encoded_payload_bytes: Option<usize>,
        disposition: PublicationDisposition,
    ) {
        self.report_owner(
            &product.unit,
            &product.module,
            product.groups.len(),
            original_bytes,
            encoded_payload_bytes,
            disposition,
        );
    }

    fn report_owner(
        &mut self,
        unit: &str,
        module: &str,
        group_rows: usize,
        original_bytes: Option<usize>,
        encoded_payload_bytes: Option<usize>,
        disposition: PublicationDisposition,
    ) {
        if self.remaining == 0 {
            return;
        }
        self.remaining -= 1;
        tracing::debug!(target: "tidepool_toolchain::module_candidates",
            phase = "candidate_publication_owner", disposition = ?disposition,
            unit, module, group_rows,
            original_bytes_known = original_bytes.is_some(),
            original_bytes = original_bytes.unwrap_or_default(),
            encoded_payload_bytes_known = encoded_payload_bytes.is_some(),
            encoded_payload_bytes = encoded_payload_bytes.unwrap_or_default(),
            "ordinary module candidate publication disposition");
    }
}

pub(crate) fn record_exact_context_publication_skip(products: &[RawModuleProduct]) {
    let mut diagnostics = PublicationDiagnostics::new();
    for product in products {
        diagnostics.report(product, None, None, PublicationDisposition::ExactContext);
    }
}

fn encode_record_checked(record: &Record) -> Result<Vec<u8>, RecordEncodingRejection> {
    let payload = shared_evidence::encode_record(record).ok_or(RecordEncodingRejection {
        disposition: PublicationDisposition::PayloadEncodingRejected,
        payload_bytes: None,
    })?;
    if payload.len() > RECORD_LIMIT {
        return Err(RecordEncodingRejection {
            disposition: PublicationDisposition::PayloadLimit,
            payload_bytes: Some(payload.len()),
        });
    }
    let header = serde_json::to_vec(&RecordHeader::for_record(record, &payload)).map_err(|_| {
        RecordEncodingRejection {
            disposition: PublicationDisposition::HeaderEncodingRejected,
            payload_bytes: Some(payload.len()),
        }
    })?;
    if header.len() > HEADER_LIMIT {
        return Err(RecordEncodingRejection {
            disposition: PublicationDisposition::HeaderLimit,
            payload_bytes: Some(payload.len()),
        });
    }
    let mut framed = Vec::with_capacity(12 + header.len() + payload.len());
    framed.extend_from_slice(RECORD_MAGIC);
    framed.extend_from_slice(&(header.len() as u32).to_be_bytes());
    framed.extend(header);
    framed.extend(payload);
    Ok(framed)
}

#[cfg(test)]
fn encode_record(record: &Record) -> Option<Vec<u8>> {
    let mut fixture = record.clone();
    fixture.data.evidence = fixture.evidence.reference()?.clone();
    encode_record_checked(&fixture).ok()
}

fn read_header(file: &mut fs::File) -> Option<RecordHeader> {
    let mut prefix = [0; 12];
    file.read_exact(&mut prefix).ok()?;
    if &prefix[..8] != RECORD_MAGIC {
        return None;
    }
    let len = u32::from_be_bytes(prefix[8..].try_into().ok()?) as usize;
    if len > HEADER_LIMIT {
        return None;
    }
    let mut bytes = vec![0; len];
    file.read_exact(&mut bytes).ok()?;
    let header: RecordHeader = serde_json::from_slice(&bytes).ok()?;
    if header.endpoint.len() > 4096
        || header.payload_len > RECORD_LIMIT as u64
        || file.metadata().ok()?.len() != 12 + len as u64 + header.payload_len
        || !header.source.is_absolute()
    {
        return None;
    }
    Some(header)
}

fn read_record(
    file: &mut fs::File,
    header: &RecordHeader,
    producer_dir: &Path,
    budget: &mut shared_evidence::ReadBudget,
) -> Option<Record> {
    budget.charge(header.payload_len)?;
    let mut payload = vec![0; usize::try_from(header.payload_len).ok()?];
    file.read_exact(&mut payload).ok()?;
    if sha(&payload) != header.payload_sha256 {
        return None;
    }
    shared_evidence::decode_record(&payload, header, producer_dir, budget)
}

#[cfg(test)]
fn read_record_path(path: &Path) -> Option<Record> {
    let mut file = fs::File::open(path).ok()?;
    let header = read_header(&mut file).or_else(|| {
        eprintln!("candidate fixture header refused: {}", path.display());
        None
    })?;
    read_record(
        &mut file,
        &header,
        path.parent()?.parent()?,
        &mut shared_evidence::ReadBudget::default(),
    )
    .or_else(|| {
        eprintln!(
            "candidate fixture record body or shared proof refused: {}",
            path.display()
        );
        None
    })
}

/// Conservative discovery filter for ordinary .hs/.lhs source owners. GHC
/// remains responsible for definitive current import selection.
fn current_home_path(module: &str, include: &[PathBuf]) -> Result<Option<PathBuf>, ()> {
    if module.split('.').any(|part| {
        part.is_empty()
            || !part
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'\'')
    }) {
        return Err(());
    }
    let relative = module.replace('.', "/");
    for root in include {
        for extension in ["hs", "lhs"] {
            let candidate = root.join(format!("{relative}.{extension}"));
            match fs::metadata(&candidate) {
                Ok(metadata) if metadata.is_file() => {
                    return absolute(&candidate).map(Some).ok_or(());
                }
                Ok(_) => return Err(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(()),
            }
        }
    }
    Ok(None)
}

fn current_source_matches(module: &str, source: &Path, include: &[PathBuf]) -> bool {
    include.is_empty()
        || matches!(current_home_path(module, include), Ok(Some(path)) if path == source)
}

/// Partition advice by the root that actually selected this ordinary source,
/// not by the entire invocation's include vector. Unsupported or ambiguous
/// conventions remain a miss; the original record and version stay intact.
fn selected_record_root(record: &Record) -> Option<PathBuf> {
    let relative = record.module.replace('.', "/");
    for root in &record.include {
        let mut selected = Vec::new();
        for extension in ["hs", "lhs"] {
            let candidate = root.join(format!("{relative}.{extension}"));
            match fs::metadata(&candidate) {
                Ok(metadata) if metadata.is_file() => selected.push(absolute(&candidate)?),
                Ok(_) => return None,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return None,
            }
        }
        match selected.as_slice() {
            [] => {}
            [source] if source == &record.source => return absolute(root),
            _ => return None,
        }
    }
    None
}

fn root_shard(producer_dir: &Path, root: &Path) -> PathBuf {
    producer_dir.join(sha(root.as_os_str().as_encoded_bytes()))
}

/// Request-local candidate records. Construction consumes the exact decoded
/// inventory; durable writes remain deferred until the front door admits targets.
pub(crate) struct PreparedPublication<'a> {
    endpoint_identity: &'a [u8],
    include: &'a [PathBuf],
    evidence: &'a DependencyEvidence,
    pub(super) inventory: Arc<tidepool_repr::execution_schema::InventoryOperation>,
    records: Vec<Record>,
    group_counts: BTreeMap<(String, String), usize>,
    graphs: BTreeMap<[u8; 32], Arc<crate::execution_source::CertifiedExecutionSourceGraph>>,
}

pub(crate) fn prepare_publication<'a>(
    endpoint_identity: &'a [u8],
    include: &'a [PathBuf],
    evidence: &'a DependencyEvidence,
    products: crate::certified_products::ParsedModuleProducts,
    target_source: &str,
    version_origin: CandidateVersionOrigin,
    certified: &[crate::recovery_artifacts::CertifiedRecoveryProduct],
) -> (Vec<RawModuleProduct>, PreparedPublication<'a>) {
    let start = std::time::Instant::now();
    let bytes = products.aggregate_bytes_len();
    let owners = products.products().len();
    let inventory = Arc::clone(products.operation());
    let mut diagnostics = PublicationDiagnostics::new();
    let (products, records) = eligible_records_with_report(
        endpoint_identity,
        include,
        evidence,
        products,
        target_source,
        &version_origin,
        certified,
        &mut |product, bytes, disposition| diagnostics.report(product, bytes, None, disposition),
    );
    let graphs = records
        .iter()
        .filter_map(|record| record.execution_source.as_ref())
        .map(|graph| (graph.digest(), Arc::clone(graph)))
        .collect();
    let group_counts = products
        .iter()
        .map(|product| {
            (
                (product.unit.clone(), product.module.clone()),
                product.groups.len(),
            )
        })
        .collect();
    crate::timing::record_stage_with_owners(
        crate::timing::NO_NODE,
        crate::timing::NO_ROUND,
        "products.publication_prepare",
        start.elapsed(),
        bytes as u64,
        owners,
    );
    (
        products,
        PreparedPublication {
            endpoint_identity,
            include,
            evidence,
            inventory,
            records,
            group_counts,
            graphs,
        },
    )
}

fn eligible_records_with_report(
    endpoint_identity: &[u8],
    include: &[PathBuf],
    evidence: &DependencyEvidence,
    parsed: crate::certified_products::ParsedModuleProducts,
    target_source: &str,
    version_origin: &CandidateVersionOrigin,
    certified: &[crate::recovery_artifacts::CertifiedRecoveryProduct],
    report: &mut impl FnMut(&RawModuleProduct, Option<usize>, PublicationDisposition),
) -> (Vec<RawModuleProduct>, Vec<Record>) {
    let (inventory, products, per_module_bytes, package_imports) = parsed.into_publication_parts();
    macro_rules! reject_all {
        ($disposition:expr) => {{
            for product in products.iter().take(CANDIDATE_LIMIT) {
                report(product, None, $disposition);
            }
            return (products, Vec::new());
        }};
    }
    let Some(include) = context_paths(include) else {
        reject_all!(PublicationDisposition::IncludeUnavailable);
    };
    if endpoint_identity.is_empty()
        || endpoint_identity.len() > 4096
        || !evidence.valid(target_source)
        || !evidence.selection_complete
    {
        reject_all!(PublicationDisposition::InvocationProofRejected);
    }
    if per_module_bytes
        .iter()
        .any(|bytes| bytes.len() > inventory.limits().max_module_bytes)
    {
        reject_all!(PublicationDisposition::ProductSplitRejected);
    }
    let Some(mut package_imports) = package_imports else {
        reject_all!(PublicationDisposition::PackageBundleRejected);
    };
    let mut records = Vec::new();
    let shared_evidence = shared_evidence::SharedEvidence::from(evidence.clone());
    let Some(evidence_reference) = shared_evidence.reference().cloned() else {
        reject_all!(PublicationDisposition::InvocationProofRejected);
    };
    for (product, module_bytes) in products.iter().zip(per_module_bytes) {
        let original_bytes = Some(module_bytes.len());
        if generation_dependent(product) {
            report(
                product,
                original_bytes,
                PublicationDisposition::GenerationDependent,
            );
            continue;
        }
        let Some(package_imports_bytes) =
            package_imports.remove(&(product.unit.clone(), product.module.clone()))
        else {
            report(
                product,
                original_bytes,
                PublicationDisposition::PackageWitnessMissing,
            );
            continue;
        };
        let iface_sha: [u8; 32] = Sha256::digest(&product.interface).into();
        if crate::recovery_artifacts::validate_package_imports(
            &package_imports_bytes,
            &product.unit,
            &product.module,
            &iface_sha,
            Path::new("module-package-imports.cbor"),
        )
        .is_err()
        {
            report(
                product,
                original_bytes,
                PublicationDisposition::PackageWitnessRejected,
            );
            continue;
        }
        if product.unit.is_empty() || product.module.is_empty() || product.interface.is_empty() {
            report(
                product,
                original_bytes,
                PublicationDisposition::ProductOwnerRejected,
            );
            continue;
        }
        let matching: Vec<_> = evidence
            .modules
            .iter()
            .filter(|m| {
                m.unit == product.unit
                    && m.module == product.module
                    && !m.boot
                    && m.product == ProductAvailability::Ready
            })
            .collect();
        if matching.len() != 1 {
            report(
                product,
                original_bytes,
                PublicationDisposition::ModuleEvidenceRejected,
            );
            continue;
        }
        let source = &matching[0].source;
        let (source, source_sha256) = if source == Path::new("@generated-source") {
            report(
                product,
                original_bytes,
                PublicationDisposition::GeneratedRequestTarget,
            );
            continue;
        } else {
            let Some(path) = absolute(source) else {
                report(
                    product,
                    original_bytes,
                    PublicationDisposition::SourceUnavailable,
                );
                continue;
            };
            let Ok(bytes) = fs::read(&path) else {
                report(
                    product,
                    original_bytes,
                    PublicationDisposition::SourceUnavailable,
                );
                continue;
            };
            (path, sha(&bytes))
        };
        if !evidence
            .sources
            .iter()
            .any(|s| absolute(&s.path).as_ref() == Some(&source) && s.sha256 == source_sha256)
        {
            report(
                product,
                original_bytes,
                PublicationDisposition::SourceEvidenceRejected,
            );
            continue;
        }
        let evidence: shared_evidence::SharedEvidence = shared_evidence.clone();
        let record = Record {
            evidence: evidence.clone(),
            module_interface_proof: None,
            execution_source: None,
            data: RecordData {
                evidence: evidence_reference.clone(),
                tag: "TPMCAN".into(),
                version: RECORD_VERSION,
                endpoint: endpoint_identity.to_vec(),
                include: include.clone(),
                products: module_bytes,
                unit: product.unit.clone(),
                module: product.module.clone(),
                source,
                source_sha256,
                interface: product.interface.clone(),
                package_imports: package_imports_bytes,
                target_source: target_source.to_owned(),
                version_origin: version_origin.clone(),
                original_owner: OriginalOwner {
                    unit: product.unit.clone(),
                    module: product.module.clone(),
                    module_version: [0; 32],
                    skinny_iface_sha256: [0; 32],
                    product_sha256: [0; 32],
                },
                original_certification: Vec::new(),
                module_interface: None,
                execution_source_sha256: None,
            },
        };
        let mut record = record;
        let owner = computed_owner(&record);
        record.data.original_owner = OriginalOwner::from_owner(&owner);
        if let Some(original) = certified.iter().find(|original| {
            original.owner().unit == record.unit && original.owner().module == record.module
        }) {
            if original.owner() != &owner
                || original.interface_bytes() != record.interface
                || original.product_bytes() != record.products
                || original.package_imports_bytes() != record.package_imports
            {
                report(
                    product,
                    original_bytes,
                    PublicationDisposition::ProductOwnerRejected,
                );
                continue;
            }
            record.module_interface_proof = original.module_interface().cloned();
            record.data.original_certification = original.certification_bytes().to_vec();
            record.execution_source = original.execution_source().cloned();
            record.data.execution_source_sha256 =
                record.execution_source.as_ref().map(|graph| graph.digest());
        }
        report(product, original_bytes, PublicationDisposition::Eligible);
        records.push(record);
    }
    (products, records)
}

/// Persist each eligible ordinary source module in its own bounded record.
pub(crate) fn publish_prepared(prepared: PreparedPublication<'_>) {
    let start = std::time::Instant::now();
    let owners = prepared.records.len();
    let mut encoded_bytes = 0u64;
    let mut diagnostics = PublicationDiagnostics::new();
    let producer_dir = record_dir(prepared.endpoint_identity);
    // A reference becomes visible only after its complete immutable proof.
    if !prepared.records.is_empty()
        && shared_evidence::publish(&producer_dir, prepared.evidence).is_none()
    {
        return;
    }
    if !prepared.graphs.is_empty() {
        if fs::create_dir_all(&producer_dir).is_err() {
            return;
        }
        for (digest, graph) in &prepared.graphs {
            if tidepool_atomic_write::write_best_effort(
                &graph_path(&producer_dir, digest),
                graph.bytes(),
            )
            .is_err()
            {
                return;
            }
        }
    }
    for mut record in prepared.records {
        let group_rows = prepared.group_counts[&(record.unit.clone(), record.module.clone())];
        let mut report = |encoded_payload_bytes, disposition| {
            diagnostics.report_owner(
                &record.data.unit,
                &record.data.module,
                group_rows,
                Some(record.data.products.len()),
                encoded_payload_bytes,
                disposition,
            );
        };
        let producer_dir = record_dir(&record.endpoint);
        let Some(root) = selected_record_root(&record) else {
            report(None, PublicationDisposition::SourceRootUnavailable);
            continue;
        };
        let dir = root_shard(&producer_dir, &root);
        if fs::create_dir_all(&dir).is_err() {
            report(None, PublicationDisposition::DirectoryUnavailable);
            continue;
        }
        let Some(interface) = record.module_interface_proof.as_ref() else {
            report(None, PublicationDisposition::ProductOwnerRejected);
            continue;
        };
        record.data.module_interface = crate::recovery_artifacts::materialize_module_interface(
            &producer_dir,
            interface,
            &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
            crate::recovery_artifacts::MaterializationMode::Scratch,
        )
        .ok();
        if record.module_interface.is_none() {
            report(None, PublicationDisposition::WriteRejected);
            continue;
        }
        let bytes = match encode_record_checked(&record) {
            Ok(bytes) => bytes,
            Err(rejection) => {
                report(rejection.payload_bytes, rejection.disposition);
                continue;
            }
        };
        encoded_bytes += bytes.len() as u64;
        let header_bytes = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let payload_bytes = bytes.len() - 12 - header_bytes;
        let mut key_material = Vec::new();
        key_material.extend_from_slice(&record.endpoint);
        key_material.extend_from_slice(record.unit.as_bytes());
        key_material.push(0);
        key_material.extend_from_slice(record.module.as_bytes());
        key_material.push(0);
        key_material.extend_from_slice(record.source.as_os_str().as_encoded_bytes());
        // Lookup retains ordered include recipes and prefers an exact recipe
        // for disjoint offers. Another recipe must not replace its dependency
        // products while leaving the original importers in the store.
        key_material.push(0);
        key_material.extend_from_slice(&(record.include.len() as u64).to_be_bytes());
        for root in &record.include {
            let bytes = root.as_os_str().as_encoded_bytes();
            key_material.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
            key_material.extend_from_slice(bytes);
        }
        let key = sha(&key_material);
        let disposition =
            if tidepool_atomic_write::write_best_effort(&dir.join(format!("{key}.cbor")), &bytes)
                .is_ok()
            {
                PublicationDisposition::Published
            } else {
                PublicationDisposition::WriteRejected
            };
        report(Some(payload_bytes), disposition);
    }
    crate::timing::record_stage_with_owners(
        crate::timing::NO_NODE,
        crate::timing::NO_ROUND,
        "products.publication_encode_write",
        start.elapsed(),
        encoded_bytes,
        owners,
    );
}

fn version_hash(record: &RecordData) -> [u8; 32] {
    match &record.version_origin {
        CandidateVersionOrigin::Ordinary => {
            module_version_for_product(
                &record.endpoint,
                &record.include,
                &record.source_sha256,
                &record.interface,
                &record.products,
                &record.package_imports,
            )
            .0
        }
        CandidateVersionOrigin::Exact { semantic_sha256 } => {
            exact_module_version_for_product(
                &record.endpoint,
                semantic_sha256,
                &record.unit,
                &record.module,
                &parse_sha(&record.source_sha256).unwrap_or([0; 32]),
                &record.interface,
                &record.products,
                &record.package_imports,
            )
            .0
        }
    }
}

pub(crate) fn exact_module_version_for_product(
    producer: &[u8],
    semantic: &[u8; 32],
    unit: &str,
    module: &str,
    source_sha256: &[u8; 32],
    interface: &[u8],
    products: &[u8],
    packages: &[u8],
) -> ModuleVersion {
    let producer =
        crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(producer)
            .sha256();
    let mut digest = Sha256::new();
    for field in [
        b"tidepool-exact-source-home-v2".as_slice(),
        producer.as_slice(),
        semantic.as_slice(),
        unit.as_bytes(),
        module.as_bytes(),
        source_sha256.as_slice(),
        interface,
        products,
        packages,
    ] {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field);
    }
    ModuleVersion(digest.finalize().into())
}

fn computed_owner(record: &RecordData) -> CachedHomeOwner {
    CachedHomeOwner {
        unit: record.unit.clone(),
        module: record.module.clone(),
        module_version: ModuleVersion(version_hash(record)),
        skinny_iface_sha256: Sha256::digest(&record.interface).into(),
        product_sha256: Sha256::digest(&record.products).into(),
    }
}

fn parse_sha(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut result = [0; 32];
    for (slot, pair) in result.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        *slot = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(result)
}

#[cfg(test)]
pub(crate) fn test_candidate_graph_path(producer: &[u8], digest: [u8; 32]) -> PathBuf {
    graph_path(&record_dir(producer), &digest)
}

fn graph_path(root: &Path, digest: &[u8; 32]) -> PathBuf {
    root.join(format!("execution-{}.cbor", hex(digest)))
}

fn validate_original_execution(
    record: &Record,
    graph: Arc<crate::execution_source::CertifiedExecutionSourceGraph>,
    validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
) -> Option<OriginalCandidateExecution> {
    let owner = computed_owner(record);
    let semantic = match record.version_origin {
        CandidateVersionOrigin::Ordinary => None,
        CandidateVersionOrigin::Exact { semantic_sha256 } => Some(semantic_sha256),
    };
    if record.original_owner.owner() != owner
        || record.execution_source_sha256 != Some(graph.digest())
        || graph.producer_sha256()
            != crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
                &record.endpoint,
            )
            .sha256()
        || graph.semantic_sha256() != semantic
        || !graph.eligible_source_replay_root(&owner)
        || crate::certified_products::candidate_execution_source_digest_with_validation(
            &record.original_certification,
            &owner,
            &record.package_imports,
            validation,
        )
        .ok()?
            != Some(graph.digest())
    {
        return None;
    }
    Some(OriginalCandidateExecution { graph })
}

/// The include paths must already be canonicalized by `context_paths`.
pub(crate) fn module_version_for_product(
    endpoint_identity: &[u8],
    include: &[PathBuf],
    source_sha256: &str,
    interface: &[u8],
    products: &[u8],
    package_imports: &[u8],
) -> ModuleVersion {
    let mut h = Sha256::new();
    h.update(b"tidepool-module-candidate-v6\0");
    h.update(endpoint_identity);
    for path in include {
        h.update(path.as_os_str().as_encoded_bytes());
        h.update([0]);
    }
    h.update(source_sha256);
    h.update(interface);
    h.update(products);
    h.update(package_imports);
    ModuleVersion(h.finalize().into())
}

fn ordinary_records(
    endpoint_identity: &[u8],
    include: &[PathBuf],
    exact_context: bool,
) -> Option<Vec<Record>> {
    ordinary_records_with_limits(
        endpoint_identity,
        include,
        exact_context,
        CacheOfferLimits::default(),
    )
    .map(|offer| offer.records)
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum CacheOfferOmission {
    OwnerLimit,
    ReadBudget,
    InvalidRecord,
    OperationBudget,
    RequiredInterfaceUnavailable,
}

#[derive(Clone, Copy)]
struct CacheOfferLimits {
    owners: usize,
    payload_bytes: u64,
}

impl Default for CacheOfferLimits {
    fn default() -> Self {
        Self {
            owners: CANDIDATE_LIMIT,
            payload_bytes: PAYLOAD_LIMIT as u64,
        }
    }
}

struct CacheOffer {
    records: Vec<Record>,
    diagnostics: CacheOfferDiagnostics,
}

#[derive(Default)]
struct CacheOfferDiagnostics {
    omissions: BTreeMap<CacheOfferOmission, usize>,
    sampled: usize,
}

impl CacheOfferDiagnostics {
    fn omit(&mut self, unit: &str, module: &str, reason: CacheOfferOmission) {
        *self.omissions.entry(reason).or_default() += 1;
        if self.sampled < CANDIDATE_LIMIT {
            self.sampled += 1;
            tracing::debug!(target: "tidepool_toolchain::module_candidates",
                phase = "candidate_offer_omission", unit, module, ?reason,
                "ordinary cache owner omitted from optional candidate offer");
        }
    }

    fn total(&self) -> usize {
        self.omissions.values().sum()
    }

    fn count(&self, reason: CacheOfferOmission) -> usize {
        self.omissions.get(&reason).copied().unwrap_or_default()
    }
}

/// Deployment packages remain all-or-nothing through shared record validation.
/// Ordinary cache rows are optional and can be omitted independently.
fn omit_or_refuse_candidate(
    origin: &CandidateOrigin,
    diagnostics: &mut CacheOfferDiagnostics,
    unit: &str,
    module: &str,
    reason: CacheOfferOmission,
) -> bool {
    if matches!(origin, CandidateOrigin::Deployment { .. }) {
        true
    } else {
        diagnostics.omit(unit, module, reason);
        false
    }
}

impl CacheOffer {
    fn omit(&mut self, owner: &(String, String), reason: CacheOfferOmission) {
        self.diagnostics.omit(&owner.0, &owner.1, reason);
    }
}

fn operation_budget_error(error: &crate::certified_products::CertificationError) -> bool {
    use crate::certified_products::CertificationError;
    match error {
        CertificationError::Product(cause) => operation_parse_budget_error(cause),
        CertificationError::SizeLimit { .. }
        | CertificationError::EvidenceRead {
            failure: crate::certified_products::EvidenceReadFailure::SizeLimit { .. },
            ..
        }
        | CertificationError::CapturedModulePayload(
            crate::recovery_artifacts::RecoveryArtifactError::InventoryAccounting(_),
        ) => true,
        _ => false,
    }
}

fn operation_parse_budget_error(error: &tidepool_repr::execution_schema::ParseError) -> bool {
    matches!(
        error,
        tidepool_repr::execution_schema::ParseError::LimitExceeded(_)
            | tidepool_repr::execution_schema::ParseError::InventoryByteLimit { .. }
            | tidepool_repr::execution_schema::ParseError::ByteLimit { .. }
            | tidepool_repr::execution_schema::ParseError::ModuleByteLimit { .. }
    )
}

fn recovery_operation_budget_error(
    error: &crate::recovery_artifacts::RecoveryArtifactError,
) -> bool {
    matches!(
        error,
        crate::recovery_artifacts::RecoveryArtifactError::InventoryAccounting(_)
    )
}

fn ordinary_records_with_limits(
    endpoint_identity: &[u8],
    include: &[PathBuf],
    exact_context: bool,
    limits: CacheOfferLimits,
) -> Option<CacheOffer> {
    let include = context_paths(include)?;
    let producer_dir = record_dir(endpoint_identity);
    let started = std::time::Instant::now();
    let roots = include.iter().cloned().collect::<BTreeSet<_>>();
    let mut headers_read = 0_u64;
    let mut header_bytes = 0_u64;
    let mut offer = CacheOffer {
        records: Vec::new(),
        diagnostics: CacheOfferDiagnostics::default(),
    };
    let mut selected: BTreeMap<(String, String), (RecordHeader, fs::File, PathBuf)> =
        BTreeMap::new();
    let mut ambiguous = BTreeSet::new();
    for root in &roots {
        match fs::read_dir(root_shard(&producer_dir, root)) {
            Ok(entries) => {
                for entry in entries {
                    let path = entry.ok()?.path();
                    if !path.extension().is_some_and(|x| x == "cbor") {
                        continue;
                    }
                    let Ok(mut file) = fs::File::open(&path) else {
                        continue;
                    };
                    let Some(header) = read_header(&mut file) else {
                        continue;
                    };
                    headers_read += 1;
                    let owner = (header.unit.clone(), header.module.clone());
                    let position = match file.stream_position() {
                        Ok(position) => position,
                        Err(_) => {
                            offer.omit(&owner, CacheOfferOmission::InvalidRecord);
                            continue;
                        }
                    };
                    header_bytes += position;
                    if header.endpoint != endpoint_identity
                        || (!exact_context && header.include != include)
                        || (exact_context
                            && !current_source_matches(&header.module, &header.source, &include))
                    {
                        continue;
                    }
                    let key = (header.unit.clone(), header.module.clone());
                    if ambiguous.contains(&key) {
                        continue;
                    }
                    if let Some((previous, _, previous_path)) = selected.get(&key) {
                        if previous.source != header.source {
                            selected.remove(&key);
                            ambiguous.insert(key);
                            continue;
                        }
                        // Directory order is not a preference. Preserve exact
                        // recipe preference and the former sorted-path tie break.
                        let previous_matches = previous.include == include;
                        let current_matches = header.include == include;
                        if (previous_matches && !current_matches)
                            || (previous_matches == current_matches && previous_path <= &path)
                        {
                            continue;
                        }
                    }
                    selected.insert(key, (header, file, path));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    let mut record_bytes = 0_u64;
    let mut budget = shared_evidence::ReadBudget::default();
    for (owner, (header, mut file, _)) in selected {
        if offer.records.len() >= limits.owners {
            offer.omit(&owner, CacheOfferOmission::OwnerLimit);
            continue;
        }
        let Some(next_record_bytes) = record_bytes
            .checked_add(header.payload_len)
            .filter(|bytes| *bytes <= limits.payload_bytes)
        else {
            offer.omit(&owner, CacheOfferOmission::ReadBudget);
            continue;
        };
        // The header was authenticated against the file length by read_header;
        // recheck before allocating the payload in case the file changed.
        let metadata_len = match file.metadata() {
            Ok(metadata) => metadata.len(),
            Err(_) => {
                offer.omit(&owner, CacheOfferOmission::InvalidRecord);
                continue;
            }
        };
        let payload_position = match file.stream_position() {
            Ok(position) => position,
            Err(_) => {
                offer.omit(&owner, CacheOfferOmission::InvalidRecord);
                continue;
            }
        };
        if metadata_len != payload_position + header.payload_len {
            offer.omit(&owner, CacheOfferOmission::InvalidRecord);
            continue;
        }
        if let Some(record) = read_record(&mut file, &header, &producer_dir, &mut budget) {
            record_bytes = next_record_bytes;
            offer.records.push(record);
        } else if budget.exhausted {
            // A failed charge does not consume bytes. Earlier successful
            // payload/proof charges remain in the shared budget, while a
            // smaller later record can still fit.
            budget.exhausted = false;
            offer.omit(&owner, CacheOfferOmission::ReadBudget);
        } else {
            offer.omit(&owner, CacheOfferOmission::InvalidRecord);
        }
    }
    tracing::info!(target: "tidepool_toolchain::module_candidates",
        phase = "candidate_record_read_decode", elapsed_ms = started.elapsed().as_millis() as u64,
        active_roots = roots.len(), headers_read, header_bytes, record_bytes,
        evidence_bytes = budget.evidence_bytes, unique_evidence = budget.evidence_count(),
        decoded_records = offer.records.len(), owner_limit_omitted = offer.diagnostics.count(CacheOfferOmission::OwnerLimit),
        read_budget_omitted = offer.diagnostics.count(CacheOfferOmission::ReadBudget),
        invalid_record_omitted = offer.diagnostics.count(CacheOfferOmission::InvalidRecord));
    Some(offer)
}

#[derive(Clone, Debug)]
pub(crate) enum CandidateOrigin {
    Ordinary,
    Deployment {
        interface: PathBuf,
        packages: PathBuf,
    },
}

/// Ordinary cache policy retains exact ordered include roots.
#[cfg(test)]
pub(crate) fn select(
    endpoint_identity: &[u8],
    include: &[PathBuf],
    scratch: &Path,
) -> Option<CandidateSet> {
    select_records(
        endpoint_identity,
        include,
        scratch,
        ordinary_records(endpoint_identity, include, false)?
            .into_iter()
            .map(|r| (r, CandidateOrigin::Ordinary))
            .collect(),
    )
}

/// Exact owners and planned generated modules are issued by the owning compile
/// request after its context and checked values have been admitted.
#[derive(Debug)]
pub(crate) struct ExactCandidateContext {
    protected: BTreeSet<(String, String)>,
    reserved: BTreeSet<String>,
    originals: Vec<crate::recovery_artifacts::CertifiedRecoveryProduct>,
    interface_seals: BTreeMap<(String, String), [u8; 32]>,
}

impl ExactCandidateContext {
    pub(crate) fn new(protected: BTreeSet<(String, String)>, reserved: BTreeSet<String>) -> Self {
        Self {
            protected,
            reserved,
            originals: Vec::new(),
            interface_seals: BTreeMap::new(),
        }
    }

    pub(crate) fn with_originals(
        mut self,
        originals: Vec<crate::recovery_artifacts::CertifiedRecoveryProduct>,
    ) -> Self {
        self.originals = originals;
        self
    }

    pub(crate) fn with_interface_seals(
        mut self,
        seals: BTreeMap<(String, String), [u8; 32]>,
    ) -> Self {
        self.interface_seals = seals;
        self
    }

    fn excludes_root(&self, unit: &str, module: &str) -> bool {
        self.protected
            .contains(&(unit.to_owned(), module.to_owned()))
            || self.reserved.contains(module)
    }
}

/// Configuration enters here from the owning compile front door, never from
/// product comparators or the worker's source-resolution checks.
pub(crate) fn select_configured(
    endpoint_identity: &[u8],
    include: &[PathBuf],
    scratch: &Path,
) -> Result<Option<CandidateSet>, deployment::ModulePackageError> {
    select_configured_inner(endpoint_identity, include, scratch, None)
}

pub(crate) fn select_configured_in_context(
    endpoint_identity: &[u8],
    include: &[PathBuf],
    scratch: &Path,
    context: &ExactCandidateContext,
) -> Result<Option<CandidateSet>, deployment::ModulePackageError> {
    select_configured_inner(endpoint_identity, include, scratch, Some(context))
}

fn select_configured_inner(
    endpoint_identity: &[u8],
    include: &[PathBuf],
    scratch: &Path,
    context: Option<&ExactCandidateContext>,
) -> Result<Option<CandidateSet>, deployment::ModulePackageError> {
    let started = std::time::Instant::now();
    let package = crate::toolchain::configured_module_package()?;
    let has_package = package.is_some();
    let mut records = match package {
        Some(package) => package.into_candidates(endpoint_identity)?,
        None => Vec::new(),
    };
    let deployed: std::collections::BTreeSet<_> = records
        .iter()
        .map(|(r, _)| (r.record().unit.clone(), r.record().module.clone()))
        .collect();
    records.extend(
        ordinary_records(endpoint_identity, include, context.is_some())
            .unwrap_or_default()
            .into_iter()
            .filter(|r| !deployed.contains(&(r.unit.clone(), r.module.clone())))
            .map(|r| (CandidateRecord::Encoded(r), CandidateOrigin::Ordinary)),
    );
    let selected = select_records_inner(endpoint_identity, include, scratch, records, context);
    tracing::info!(target: "tidepool_toolchain::module_candidates",
        phase = "candidate_selection", elapsed_ms = started.elapsed().as_millis() as u64,
        exact_context = context.is_some(),
        offered = selected.as_ref().map_or(0, |set| set.by_owner.len()));
    if has_package && selected.is_none() {
        return Err(deployment::ModulePackageError::Format(
            "candidate selection",
        ));
    }
    Ok(selected)
}

#[cfg(test)]
fn select_records(
    endpoint_identity: &[u8],
    include: &[PathBuf],
    scratch: &Path,
    records: Vec<(Record, CandidateOrigin)>,
) -> Option<CandidateSet> {
    select_records_inner(endpoint_identity, include, scratch, records, None)
}

fn select_records_inner<R: Into<CandidateRecord>>(
    endpoint_identity: &[u8],
    include: &[PathBuf],
    scratch: &Path,
    records: Vec<(R, CandidateOrigin)>,
    context: Option<&ExactCandidateContext>,
) -> Option<CandidateSet> {
    let include = context_paths(include)?;
    let requirements = crate::prepared_artifact::production_requirements().ok()?;
    let mut evidence_validation = shared_evidence::ValidationStage::acquisition();
    let mut validated = BTreeMap::new();
    let mut package_validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
    let mut validation_elapsed = std::time::Duration::ZERO;
    let mut decode_elapsed = std::time::Duration::ZERO;
    let mut decoded_bytes = 0_u64;
    fs::create_dir_all(scratch).ok()?;
    let scratch = absolute(scratch)?;
    let mut recovered_graphs =
        BTreeMap::<[u8; 32], Arc<crate::execution_source::CertifiedExecutionSourceGraph>>::new();
    let mut graph_bytes = 0usize;
    let mut selection_omissions = CacheOfferDiagnostics::default();
    for (record, origin) in records {
        let (mut record, product_input) = record.into().into_parts();
        if record.tag != "TPMCAN"
            || record.version != RECORD_VERSION
            || record.endpoint != endpoint_identity
            || (matches!(origin, CandidateOrigin::Ordinary)
                && context.is_none()
                && record.include != include)
            || record.source.is_relative()
            || context.is_some_and(|e| e.excludes_root(&record.unit, &record.module))
        {
            continue;
        }
        let validation_started = std::time::Instant::now();
        let source_bytes = match crate::certified_products::read_bounded_with_operation(
            &record.source,
            RECORD_LIMIT as u64,
            &package_validation.inventory,
        ) {
            Ok(bytes) => bytes,
            Err(error) => {
                let reason = if operation_budget_error(&error) {
                    CacheOfferOmission::OperationBudget
                } else {
                    CacheOfferOmission::InvalidRecord
                };
                if omit_or_refuse_candidate(
                    &origin,
                    &mut selection_omissions,
                    &record.unit,
                    &record.module,
                    reason,
                ) {
                    return None;
                }
                continue;
            }
        };
        let valid = sha(&source_bytes) == record.source_sha256
            && record.evidence.selection_complete
            && evidence_validation
                .validate(&record.evidence, &record.target_source)
                .is_ok();
        validation_elapsed += validation_started.elapsed();
        if !valid {
            selection_omissions.omit(
                &record.unit,
                &record.module,
                CacheOfferOmission::InvalidRecord,
            );
            continue;
        }
        let decode_started = std::time::Instant::now();
        let parsed = match product_input {
            CandidateProductInput::Encoded => {
                decoded_bytes += record.products.len() as u64;
                CandidateProduct::decode_product_with_operation(
                    &record.products,
                    &requirements,
                    &package_validation.inventory,
                )
            }
            CandidateProductInput::Decoded(product) => Ok(Some(product)),
        };
        decode_elapsed += decode_started.elapsed();
        let product = match parsed {
            Ok(Some(product)) => product,
            Ok(None) => {
                selection_omissions.omit(
                    &record.unit,
                    &record.module,
                    CacheOfferOmission::InvalidRecord,
                );
                continue;
            }
            Err(error) => {
                let reason = if operation_parse_budget_error(&error) {
                    CacheOfferOmission::OperationBudget
                } else {
                    CacheOfferOmission::InvalidRecord
                };
                if reason == CacheOfferOmission::OperationBudget
                    && matches!(origin, CandidateOrigin::Deployment { .. })
                {
                    return None;
                }
                selection_omissions.omit(&record.unit, &record.module, reason);
                continue;
            }
        };
        if product.unit != record.unit
            || product.module != record.module
            || product.interface != record.interface
            || record.interface.is_empty()
        {
            selection_omissions.omit(
                &record.unit,
                &record.module,
                CacheOfferOmission::InvalidRecord,
            );
            continue;
        }
        let evidence_count = record
            .evidence
            .modules
            .iter()
            .filter(|m| {
                m.unit == record.unit
                    && m.module == record.module
                    && !m.boot
                    && m.product == ProductAvailability::Ready
                    && absolute(&m.source).as_ref() == Some(&record.source)
            })
            .count();
        if evidence_count != 1
            || !record.evidence.sources.iter().any(|s| {
                absolute(&s.path).as_ref() == Some(&record.source)
                    && s.sha256 == record.source_sha256
            })
        {
            selection_omissions.omit(
                &record.unit,
                &record.module,
                CacheOfferOmission::InvalidRecord,
            );
            continue;
        }
        let iface_sha = Sha256::digest(&record.interface);
        let iface_sha_array: [u8; 32] = iface_sha.into();
        let package_roots =
            crate::recovery_artifacts::validate_package_import_evidence_with_validation(
                &record.package_imports,
                &record.unit,
                &record.module,
                &iface_sha_array,
                Path::new("module-package-imports.cbor"),
                &mut package_validation,
            );
        let package_roots = match package_roots {
            Ok(roots) => roots,
            Err(error) => {
                let reason = if recovery_operation_budget_error(&error) {
                    CacheOfferOmission::OperationBudget
                } else {
                    CacheOfferOmission::InvalidRecord
                };
                if reason == CacheOfferOmission::OperationBudget
                    && matches!(origin, CandidateOrigin::Deployment { .. })
                {
                    return None;
                }
                selection_omissions.omit(&record.unit, &record.module, reason);
                continue;
            }
        };
        if record.original_owner.owner() != computed_owner(&record) {
            selection_omissions.omit(
                &record.unit,
                &record.module,
                CacheOfferOmission::InvalidRecord,
            );
            continue;
        }
        if let Some(digest) = record.execution_source_sha256 {
            let graph = if let Some(graph) = record.execution_source.take() {
                graph
            } else if let Some(graph) = recovered_graphs.get(&digest) {
                Arc::clone(graph)
            } else {
                let path = graph_path(&record_dir(endpoint_identity), &digest);
                let metadata_len = match fs::metadata(&path) {
                    Ok(metadata) if metadata.is_file() => metadata.len(),
                    _ => {
                        if omit_or_refuse_candidate(
                            &origin,
                            &mut selection_omissions,
                            &record.unit,
                            &record.module,
                            CacheOfferOmission::InvalidRecord,
                        ) {
                            return None;
                        }
                        continue;
                    }
                };
                let Some(metadata_bytes) = usize::try_from(metadata_len).ok() else {
                    if omit_or_refuse_candidate(
                        &origin,
                        &mut selection_omissions,
                        &record.unit,
                        &record.module,
                        CacheOfferOmission::ReadBudget,
                    ) {
                        return None;
                    }
                    continue;
                };
                let Some(next_graph_bytes) = graph_bytes
                    .checked_add(metadata_bytes)
                    .filter(|bytes| *bytes <= crate::execution_source::GRAPH_BYTES_LIMIT)
                else {
                    if omit_or_refuse_candidate(
                        &origin,
                        &mut selection_omissions,
                        &record.unit,
                        &record.module,
                        CacheOfferOmission::ReadBudget,
                    ) {
                        return None;
                    }
                    continue;
                };
                let bytes = match crate::certified_products::read_bounded_with_operation(
                    &path,
                    crate::execution_source::GRAPH_BYTES_LIMIT as u64,
                    &package_validation.inventory,
                ) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        let reason = if operation_budget_error(&error) {
                            CacheOfferOmission::OperationBudget
                        } else {
                            CacheOfferOmission::ReadBudget
                        };
                        if omit_or_refuse_candidate(
                            &origin,
                            &mut selection_omissions,
                            &record.unit,
                            &record.module,
                            reason,
                        ) {
                            return None;
                        }
                        continue;
                    }
                };
                if bytes.len() as u64 != metadata_len {
                    if omit_or_refuse_candidate(
                        &origin,
                        &mut selection_omissions,
                        &record.unit,
                        &record.module,
                        CacheOfferOmission::InvalidRecord,
                    ) {
                        return None;
                    }
                    continue;
                }
                if <[u8; 32]>::from(Sha256::digest(&bytes)) != digest {
                    if omit_or_refuse_candidate(
                        &origin,
                        &mut selection_omissions,
                        &record.unit,
                        &record.module,
                        CacheOfferOmission::InvalidRecord,
                    ) {
                        return None;
                    }
                    continue;
                }
                let wire = match package_validation
                    .inventory
                    .decode_value(&bytes, crate::execution_source::GRAPH_BYTES_LIMIT)
                {
                    Ok(wire) => wire,
                    Err(error) => {
                        let reason = if operation_parse_budget_error(&error) {
                            CacheOfferOmission::OperationBudget
                        } else {
                            CacheOfferOmission::InvalidRecord
                        };
                        if omit_or_refuse_candidate(
                            &origin,
                            &mut selection_omissions,
                            &record.unit,
                            &record.module,
                            reason,
                        ) {
                            return None;
                        }
                        continue;
                    }
                };
                if package_validation
                    .inventory
                    .charge_value_copies(&wire, 2)
                    .is_err()
                {
                    if omit_or_refuse_candidate(
                        &origin,
                        &mut selection_omissions,
                        &record.unit,
                        &record.module,
                        CacheOfferOmission::OperationBudget,
                    ) {
                        return None;
                    }
                    continue;
                }
                drop(wire);
                let graph =
                    match crate::execution_source::CertifiedExecutionSourceGraph::recover_verified(
                        bytes, digest,
                    ) {
                        Ok(graph) => graph,
                        Err(_) => {
                            if omit_or_refuse_candidate(
                                &origin,
                                &mut selection_omissions,
                                &record.unit,
                                &record.module,
                                CacheOfferOmission::InvalidRecord,
                            ) {
                                return None;
                            }
                            continue;
                        }
                    };
                graph_bytes = next_graph_bytes;
                graph
            };
            if validate_original_execution(&record, Arc::clone(&graph), &mut package_validation)
                .is_none()
            {
                if omit_or_refuse_candidate(
                    &origin,
                    &mut selection_omissions,
                    &record.unit,
                    &record.module,
                    CacheOfferOmission::InvalidRecord,
                ) {
                    return None;
                }
                continue;
            }
            recovered_graphs.insert(digest, Arc::clone(&graph));
            record.execution_source = Some(graph);
        } else if record.execution_source.is_some() {
            if omit_or_refuse_candidate(
                &origin,
                &mut selection_omissions,
                &record.unit,
                &record.module,
                CacheOfferOmission::InvalidRecord,
            ) {
                return None;
            }
            continue;
        }
        let canonical = match (&record.module_interface_proof, &record.module_interface) {
            (Some(interface), _) => interface.clone(),
            (None, Some(reference)) => match crate::recovery_artifacts::recover_module_interface(
                &record_dir(endpoint_identity),
                reference,
                &mut package_validation,
            ) {
                Ok(interface) => interface,
                Err(_) => {
                    if omit_or_refuse_candidate(
                        &origin,
                        &mut selection_omissions,
                        &record.unit,
                        &record.module,
                        CacheOfferOmission::InvalidRecord,
                    ) {
                        return None;
                    }
                    continue;
                }
            },
            (None, None) => continue,
        };
        let Some(source_sha256) = parse_sha(&record.source_sha256) else {
            if omit_or_refuse_candidate(
                &origin,
                &mut selection_omissions,
                &record.unit,
                &record.module,
                CacheOfferOmission::InvalidRecord,
            ) {
                return None;
            }
            continue;
        };
        if let Err(error) =
            crate::certified_products::validate_canonical_native_bytes_with_operation(
                &computed_owner(&record),
                &record.original_certification,
                &record.interface,
                &record.package_imports,
                Some(source_sha256),
                &canonical,
                &package_validation.inventory,
            )
        {
            let reason = if operation_budget_error(&error) {
                CacheOfferOmission::OperationBudget
            } else {
                CacheOfferOmission::InvalidRecord
            };
            if omit_or_refuse_candidate(
                &origin,
                &mut selection_omissions,
                &record.unit,
                &record.module,
                reason,
            ) {
                return None;
            }
            continue;
        }
        let owner_key = (record.unit.clone(), record.module.clone());
        let Some(record) = ValidatedRecord::admit(record, canonical) else {
            if omit_or_refuse_candidate(
                &origin,
                &mut selection_omissions,
                &owner_key.0,
                &owner_key.1,
                CacheOfferOmission::InvalidRecord,
            ) {
                return None;
            }
            continue;
        };
        if generation_dependent(&product) {
            continue;
        }
        let key = owner_key;
        if validated
            .insert(key, (record, origin, product, package_roots))
            .is_some()
        {
            return None;
        }
    }
    tracing::info!(target: "tidepool_toolchain::module_candidates",
        phase = "candidate_record_validation", validation_ms = validation_elapsed.as_millis() as u64,
        product_decode_ms = decode_elapsed.as_millis() as u64, decoded_product_bytes = decoded_bytes,
        validated = validated.len());
    if let Some(context) = context {
        retain_compatible_dependencies(&mut validated, context, &include);
    }
    loop {
        let mut available = context
            .map(|context| context.interface_seals.clone())
            .unwrap_or_default();
        for (key, (record, _, _, _)) in &validated {
            available
                .entry(key.clone())
                .or_insert_with(|| record.canonical.interface_sha256());
        }
        let rejected = validated
            .iter()
            .filter_map(|(key, (record, _, _, _))| {
                let canonical = &record.canonical;
                let expected_producer =
                    crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
                        endpoint_identity,
                    )
                    .sha256();
                let reason = if canonical.producer_sha256() != expected_producer
                    || hex(&canonical.source_sha256()) != record.source_sha256
                {
                    Some(CandidateInterfaceUnavailable::ProducerOrSourceMismatch)
                } else {
                    canonical
                        .requirements()
                        .iter()
                        .find_map(|(owner, expected)| {
                            (available.get(owner) != Some(expected)).then(|| {
                                CandidateInterfaceUnavailable::RequiredInterface {
                                    owner: owner.clone(),
                                    expected: *expected,
                                    available: available.get(owner).copied(),
                                }
                            })
                        })
                };
                reason.map(|reason| (key.clone(), reason))
            })
            .collect::<Vec<_>>();
        if rejected.is_empty() {
            break;
        }
        for (owner, reason) in rejected {
            selection_omissions.omit(
                &owner.0,
                &owner.1,
                if matches!(
                    reason,
                    CandidateInterfaceUnavailable::RequiredInterface { .. }
                ) {
                    CacheOfferOmission::RequiredInterfaceUnavailable
                } else {
                    CacheOfferOmission::InvalidRecord
                },
            );
            tracing::debug!(target: "tidepool_toolchain::module_candidates", unit = owner.0.as_str(), module = owner.1.as_str(), ?reason,
                "candidate declined because its canonical interface closure is unavailable");
            validated.remove(&owner);
        }
    }
    tracing::info!(target: "tidepool_toolchain::module_candidates",
        phase = "candidate_offer_omissions",
        required_interface_unavailable = selection_omissions.omissions.get(&CacheOfferOmission::RequiredInterfaceUnavailable).copied().unwrap_or_default(),
        operation_budget = selection_omissions.omissions.get(&CacheOfferOmission::OperationBudget).copied().unwrap_or_default(),
        invalid_record = selection_omissions.omissions.get(&CacheOfferOmission::InvalidRecord).copied().unwrap_or_default(),
        total = selection_omissions.total(),
        "ordinary optional cache offer omissions");
    let mut inventory = inventory::InventoryTables::new(
        validated
            .values()
            .flat_map(|(_, _, product, _)| product.groups.iter()),
    )?;
    let mut by_owner = BTreeMap::new();
    let mut manifest = Vec::new();
    for (_, (mut record, origin, product, _)) in validated {
        let product_sha: [u8; 32] = Sha256::digest(&record.products).into();
        let Some(evidence_sha) = record.evidence.json_sha256().map(|digest| hex(&digest)) else {
            continue;
        };
        let Some(module_evidence) = record.evidence.modules.iter().find(|module| {
            module.unit == record.unit
                && module.module == record.module
                && !module.boot
                && absolute(&module.source).as_ref() == Some(&record.source)
        }) else {
            continue;
        };
        let imports = module_evidence
            .imports
            .iter()
            .map(|imported| {
                Value::Array(vec![
                    Value::Text(String::from(imported.qualifier.clone())),
                    Value::Text(imported.module.clone()),
                    Value::Bool(imported.boot),
                    Value::Text(
                        imported
                            .selected
                            .as_ref()
                            .map_or_else(String::new, |path| path.to_string_lossy().into_owned()),
                    ),
                ])
            })
            .collect();
        let owner = record.original_owner.owner();
        let (iface_path, package_imports_path) = match &origin {
            CandidateOrigin::Deployment {
                interface,
                packages,
            } => (interface.clone(), packages.clone()),
            CandidateOrigin::Ordinary => {
                let iface_path = scratch.join(format!(
                    "candidate-{}.hi",
                    sha(format!("{}:{}", record.unit, record.module).as_bytes())
                ));
                tidepool_atomic_write::write_best_effort(&iface_path, &record.interface).ok()?;
                let package_path = PathBuf::from(format!("{}.packages", iface_path.display()));
                tidepool_atomic_write::write_best_effort(&package_path, &record.package_imports)
                    .ok()?;
                (iface_path, package_path)
            }
        };
        let canonical = &record.canonical;
        let canonical_reference = crate::recovery_artifacts::materialize_module_interface(
            &scratch,
            canonical,
            &mut package_validation,
            crate::recovery_artifacts::MaterializationMode::Scratch,
        )
        .ok()?;
        let core_reference = canonical_reference.core.as_ref()?;
        let bundle = CandidateBundle {
            owner: owner.clone(),
            product: CandidateProduct {
                bytes: std::mem::take(&mut record.data.products),
                decoded: product,
            },
            source: record.source.clone(),
            source_sha256: record.source_sha256.clone(),
            iface_path: iface_path.clone(),
            iface_sha256: sha(&record.interface),
            package_imports_path: package_imports_path.clone(),
            package_imports_sha256: sha(&record.package_imports),
            package_imports_bytes: std::mem::take(&mut record.data.package_imports),
            evidence: record.evidence.clone(),
            target_source: record.target_source.clone(),
            origin,
            original_module_interface: record.canonical.clone(),
            original_execution: record.execution_source.as_ref().map(|graph| {
                OriginalCandidateExecution {
                    graph: Arc::clone(graph),
                }
            }),
            execution_admitted: false,
        };
        if by_owner
            .insert((owner.unit.clone(), owner.module.clone()), bundle)
            .is_some()
        {
            return None;
        }
        let selected = &by_owner[&(record.unit.clone(), record.module.clone())];
        let product_path =
            scratch.join(format!("candidate-{}.tpmod", hex(&owner.module_version.0)));
        tidepool_atomic_write::write_best_effort(&product_path, selected.product.bytes()).ok()?;
        manifest.push(candidate_manifest_row(CandidateManifestRow {
            unit: &owner.unit,
            module: &owner.module,
            source: &record.source,
            source_sha256: &record.data.source_sha256,
            interface: &iface_path,
            interface_sha256: &sha(&record.interface),
            module_version: &hex(&owner.module_version.0),
            product_sha256: &hex(&product_sha),
            evidence_sha256: &evidence_sha,
            imports,
            groups: inventory.groups(&selected.product.groups)?,
            packages: &package_imports_path,
            packages_sha256: &sha(&selected.package_imports_bytes),
            product: &product_path,
            canonical_requirements: canonical.requirements(),
            certificate: &scratch.join(&canonical_reference.certificate_path),
            certificate_sha256: &canonical_reference.certificate_sha256,
            core: &scratch.join(&core_reference.path),
            core_sha256: &core_reference.sha256,
        }));
    }
    retain_closed_execution_capabilities(
        &mut by_owner,
        context.map_or(&[], |context| context.originals.as_slice()),
    );
    let graph_bytes = by_owner
        .values()
        .filter(|bundle| bundle.execution_admitted)
        .filter_map(|bundle| bundle.original_execution.as_ref())
        .map(|proof| (proof.graph.digest(), proof.graph.bytes().len()))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .try_fold(0usize, |total, bytes| total.checked_add(bytes))?;
    if graph_bytes > crate::execution_source::GRAPH_BYTES_LIMIT {
        tracing::info!(target: "tidepool_toolchain::module_candidates", reason = ?CandidateExecutionUnavailable::GraphBound,
            graph_bytes, limit = crate::execution_source::GRAPH_BYTES_LIMIT, "candidate offer omitted because its execution provenance exceeds the retained graph bound");
        return None;
    }
    let (symbols, globals) = inventory.into_wire_tables();
    let value = candidate_manifest_value(
        symbols,
        globals,
        manifest,
        by_owner
            .values()
            .filter(|bundle| bundle.execution_admitted)
            .filter_map(|bundle| bundle.original_execution.as_ref())
            .map(|proof| (proof.graph.digest(), &proof.graph))
            .collect::<BTreeMap<_, _>>()
            .into_iter()
            .map(|(digest, graph)| {
                let path = graph.capture_descriptor(&scratch).ok()?;
                Some(Value::Array(vec![
                    Value::Text(hex(&digest)),
                    Value::Text(path.to_string_lossy().into_owned()),
                ]))
            })
            .collect::<Option<Vec<_>>>()?,
        by_owner
            .values()
            .filter_map(|bundle| {
                if !bundle.execution_admitted {
                    return None;
                }
                let proof = bundle.original_execution.as_ref()?;
                let owner = &bundle.owner;
                Some(Value::Array(vec![
                    Value::Text(owner.unit.clone()),
                    Value::Text(owner.module.clone()),
                    Value::Text(hex(&owner.module_version.0)),
                    Value::Text(hex(&owner.skinny_iface_sha256)),
                    Value::Text(hex(&owner.product_sha256)),
                    Value::Text(hex(&proof.graph.digest())),
                ]))
            })
            .collect(),
        &crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
            endpoint_identity,
        )
        .hex(),
    );
    let mut encoded = Vec::new();
    ciborium::ser::into_writer(&value, &mut encoded).ok()?;
    if encoded.len() > MANIFEST_LIMIT {
        tracing::info!(target: "tidepool_toolchain::module_candidates", reason = ?CandidateExecutionUnavailable::ManifestBound,
            manifest_bytes = encoded.len(), limit = MANIFEST_LIMIT,
            "candidate offer omitted because its execution provenance exceeds the manifest bound");
        return None;
    }
    let manifest_path = scratch.join("module-candidates.cbor");
    tidepool_atomic_write::write_best_effort(&manifest_path, &encoded).ok()?;
    Some(CandidateSet {
        manifest_path,
        by_owner,
    })
}

/// Encoding data is separate from the retained executable candidate set.
struct CandidateManifestRow<'a> {
    unit: &'a str,
    module: &'a str,
    source: &'a Path,
    source_sha256: &'a str,
    interface: &'a Path,
    interface_sha256: &'a str,
    module_version: &'a str,
    product_sha256: &'a str,
    evidence_sha256: &'a str,
    imports: Vec<Value>,
    groups: Value,
    packages: &'a Path,
    packages_sha256: &'a str,
    product: &'a Path,
    canonical_requirements: &'a BTreeMap<(String, String), [u8; 32]>,
    certificate: &'a Path,
    certificate_sha256: &'a [u8; 32],
    core: &'a Path,
    core_sha256: &'a [u8; 32],
}

fn candidate_manifest_row(row: CandidateManifestRow<'_>) -> Value {
    let path = |path: &Path| Value::Text(path.to_string_lossy().into_owned());
    Value::Array(vec![
        Value::Text(row.unit.into()),
        Value::Text(row.module.into()),
        path(row.source),
        Value::Text(row.source_sha256.into()),
        path(row.interface),
        Value::Text(row.interface_sha256.into()),
        Value::Text(row.module_version.into()),
        Value::Text(row.product_sha256.into()),
        Value::Text(row.evidence_sha256.into()),
        Value::Array(row.imports),
        row.groups,
        path(row.packages),
        Value::Text(row.packages_sha256.into()),
        path(row.product),
        Value::Array(
            row.canonical_requirements
                .keys()
                .map(|(unit, module)| {
                    Value::Array(vec![Value::Text(unit.clone()), Value::Text(module.clone())])
                })
                .collect(),
        ),
        Value::Array(vec![
            Value::Text("module".into()),
            path(row.certificate),
            Value::Text(hex(row.certificate_sha256)),
            path(row.core),
            Value::Text(hex(row.core_sha256)),
        ]),
    ])
}

fn candidate_manifest_value(
    symbols: Value,
    globals: Value,
    rows: Vec<Value>,
    graphs: Vec<Value>,
    owners: Vec<Value>,
    producer: &str,
) -> Value {
    Value::Array(vec![
        Value::Text("TPMCAN".into()),
        Value::Text("10".into()),
        symbols,
        globals,
        Value::Array(rows),
        Value::Array(vec![Value::Array(graphs), Value::Array(owners)]),
        Value::Text(producer.into()),
    ])
}

#[derive(Clone, Debug, thiserror::Error)]
enum CandidateInterfaceUnavailable {
    #[error("canonical module recovery descriptor is absent")]
    MissingCanonicalReference,
    #[error("canonical module recovery descriptor differs from its admitted proof")]
    CanonicalReferenceMismatch,
    #[error("canonical module producer or source differs")]
    ProducerOrSourceMismatch,
    #[error("required interface {owner:?} needs {expected:?}; available {available:?}")]
    RequiredInterface {
        owner: (String, String),
        expected: [u8; 32],
        available: Option<[u8; 32]>,
    },
}

#[derive(Clone, Copy, Debug)]
enum CandidateExecutionUnavailable {
    ManifestBound,
    GraphBound,
}

fn retain_closed_execution_capabilities(
    bundles: &mut BTreeMap<(String, String), CandidateBundle>,
    originals: &[crate::recovery_artifacts::CertifiedRecoveryProduct],
) {
    let inventory =
        dependencies::CandidateDependencyInventory::from_candidates(bundles.values(), originals);
    for bundle in bundles.values_mut() {
        let Some(proof) = bundle.original_execution.as_ref() else {
            continue;
        };
        let result = inventory.verify(&bundle.owner, &proof.graph);
        bundle.execution_admitted = result.is_ok();
        if let Err(reason) = result {
            tracing::debug!(target: "tidepool_toolchain::module_candidates",
                unit = bundle.owner.unit.as_str(), module = bundle.owner.module.as_str(),
                ?reason, "candidate retains native products without execution capability");
        }
    }
}

fn generation_dependent(product: &RawModuleProduct) -> bool {
    product.groups.iter().any(|group| {
        group
            .globals()
            .iter()
            .any(|global| global.required_generation.is_some())
    })
}

type ValidatedCandidate = (
    ValidatedRecord,
    CandidateOrigin,
    RawModuleProduct,
    crate::recovery_artifacts::ValidatedPackageImports,
);

/// Authenticated direct package selections and closed compiler-provided
/// categories alone classify pathless imports. Missing home/Val evidence
/// cannot acquire package authority from its spelling.
pub(crate) fn package_edge_matches(
    edge: &crate::cache::ModuleImportEvidence,
    evidence: &crate::recovery_artifacts::ValidatedPackageImports,
) -> bool {
    use crate::cache::ImportQualifier;
    use crate::recovery_artifacts::CompilerProvidedImport;
    if edge.boot || edge.selected.is_some() {
        return false;
    }
    let primitive = edge.module == "GHC.Prim"
        && evidence
            .compiler_provided()
            .contains(&CompilerProvidedImport::Primitive);
    match &edge.qualifier {
        ImportQualifier::OtherUnit(unit) => {
            evidence
                .roots()
                .contains_key(&(unit.clone(), edge.module.clone()))
                || (unit == "ghc-prim" && primitive)
        }
        ImportQualifier::Unqualified => {
            evidence
                .roots()
                .keys()
                .filter(|(_, module)| module == &edge.module)
                .count()
                + usize::from(primitive)
                == 1
        }
        ImportQualifier::ThisUnit(_) => false,
    }
}

/// Keep the greatest closed subset whose dependencies are compatible with
/// either another candidate or an already selected immutable original.
fn retain_compatible_dependencies(
    candidates: &mut BTreeMap<(String, String), ValidatedCandidate>,
    context: &ExactCandidateContext,
    include: &[PathBuf],
) {
    use dependencies::{CandidateDependencyInventory, DependencyKind};
    loop {
        let inventory = CandidateDependencyInventory::new(
            candidates
                .values()
                .map(|(record, _, _, _)| {
                    (
                        record.original_owner.owner(),
                        record.execution_source.clone(),
                        DependencyKind::Candidate,
                    )
                })
                .chain(context.originals.iter().map(|original| {
                    (
                        original.owner().clone(),
                        original.execution_source().cloned(),
                        DependencyKind::Original,
                    )
                })),
        );
        let rejected: Vec<_> = candidates
            .iter()
            .filter_map(|(key, (record, _, _, packages))| {
                let owner = record.original_owner.owner();
                let originals = match &record.execution_source {
                    Some(graph) => match inventory.direct_originals(&owner, graph) {
                        Ok(originals) => originals,
                        Err(reason) => {
                            tracing::debug!(target: "tidepool_toolchain::module_candidates",
                            unit = owner.unit.as_str(), module = owner.module.as_str(),
                            ?reason, "candidate dependency proof refused");
                            return Some(key.clone());
                        }
                    },
                    None => BTreeMap::new(),
                };
                // Generated modules are reserved even if an old recipe happens
                // to contain a matching product for the same spelling.
                if originals
                    .keys()
                    .any(|(_, module)| context.reserved.contains(module))
                {
                    return Some(key.clone());
                }
                let source_owner = record.evidence.modules.iter().find(|m| {
                    m.unit == record.unit
                        && m.module == record.module
                        && !m.boot
                        && absolute(&m.source).as_ref() == Some(&record.source)
                });
                let closed = source_owner.is_some_and(|source_owner| {
                    source_owner.imports.iter().all(|edge| {
                        use crate::cache::ImportQualifier;
                        if edge.boot {
                            return false;
                        }
                        if edge.selected.is_none() {
                            if package_edge_matches(edge, packages) {
                                return !matches!(edge.qualifier, ImportQualifier::Unqualified)
                                    || matches!(
                                        current_home_path(&edge.module, include),
                                        Ok(None)
                                    );
                            }
                            // A pathless home edge needs the graph's exact owner
                            // proof; its module spelling cannot grant authority.
                            return originals
                                .keys()
                                .filter(|(unit, module)| {
                                    module == &edge.module
                                        && match &edge.qualifier {
                                            ImportQualifier::Unqualified => true,
                                            ImportQualifier::ThisUnit(requested) => {
                                                unit == requested
                                            }
                                            ImportQualifier::OtherUnit(_) => false,
                                        }
                                })
                                .count()
                                == 1;
                        }
                        let Some(selected) = edge.selected.as_ref().and_then(|p| absolute(p))
                        else {
                            return false;
                        };
                        let mut matches = record.evidence.modules.iter().filter(|m| {
                            m.module == edge.module
                                && !m.boot
                                && absolute(&m.source).as_ref() == Some(&selected)
                                && match &edge.qualifier {
                                    ImportQualifier::ThisUnit(unit) => &m.unit == unit,
                                    ImportQualifier::Unqualified => true,
                                    ImportQualifier::OtherUnit(_) => false,
                                }
                        });
                        let Some(dependency) = matches.next() else {
                            return false;
                        };
                        if matches.next().is_some() || context.reserved.contains(&dependency.module)
                        {
                            return false;
                        }
                        let dependency_key = (dependency.unit.clone(), dependency.module.clone());
                        if originals.contains_key(&dependency_key) {
                            return true;
                        }
                        !context.protected.contains(&dependency_key)
                            && candidates
                                .get(&dependency_key)
                                .is_some_and(|(body, _, _, _)| body.source == selected)
                    })
                });
                (!closed).then(|| key.clone())
            })
            .collect();
        if rejected.is_empty() {
            break;
        }
        for key in rejected {
            candidates.remove(&key);
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::cache::{ModuleEvidence, SourceEvidence};
    use proptest::prelude::*;
    use proptest::strategy::ValueTree;
    use proptest::test_runner::{Config, FileFailurePersistence, TestRunner};

    #[test]
    #[ignore = "requires retained Core record input and fresh output directory"]
    fn retained_core_candidate_production_emitter() {
        let input = PathBuf::from(
            std::env::var_os("TIDEPOOL_CANDIDATE_RECORD_INPUT").expect("retained Core record"),
        );
        let output = PathBuf::from(
            std::env::var_os("TIDEPOOL_CANDIDATE_RECORD_OUTPUT").expect("fresh output directory"),
        );
        assert!(!output.exists(), "diagnostic output must be fresh");
        let input_bytes = fs::read(&input).unwrap();
        assert!(input_bytes.len() <= RECORD_LIMIT + HEADER_LIMIT + 12);
        let record = read_record_path(&input).expect("authenticated production record");
        assert_eq!(record.unit, "main");
        assert_eq!(record.module, "Tidepool.Effects.Core");
        let endpoint = record.endpoint.clone();
        let include = record.include.clone();
        let original_products = record.products.clone();
        let original_interface = record.interface.clone();
        let original_packages = record.package_imports.clone();
        let original_version = version_hash(&record);
        // Ordinary selection exercises the production emitter without asserting
        // that this single vertex closes an exact-context candidate graph.
        let selected = select_records(
            &endpoint,
            &include,
            &output,
            vec![(record, CandidateOrigin::Ordinary)],
        )
        .expect("production candidate selection");
        assert_eq!(selected.by_owner.len(), 1);
        let bundle = &selected.by_owner[&("main".into(), "Tidepool.Effects.Core".into())];
        assert_eq!(bundle.product.groups.len(), 6037);
        assert!(!generation_dependent(&bundle.product));
        assert_eq!(bundle.owner.module_version.0, original_version);
        assert_eq!(bundle.product.bytes(), original_products);
        assert_eq!(fs::read(&bundle.iface_path).unwrap(), original_interface);
        assert_eq!(
            fs::read(&bundle.package_imports_path).unwrap(),
            original_packages
        );
        let original_product_path =
            output.join(format!("candidate-{}.tpmod", hex(&original_version)));
        assert_eq!(fs::read(&original_product_path).unwrap(), original_products);
        assert_eq!(fs::read(&input).unwrap(), input_bytes);
        fs::write(
            output.join("emitter-result.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "scope": "production ordinary emitter; not exact closure or worker admission",
                "record": input,
                "record_sha256": sha(&input_bytes),
                "manifest": selected.manifest_path,
                "module": bundle.owner.module,
                "group_rows": bundle.product.groups.len(),
                "module_version": hex(&original_version),
                "products": {"path": original_product_path, "bytes": original_products.len(), "sha256": sha(&original_products)},
                "interface": {"path": bundle.iface_path, "bytes": original_interface.len(), "sha256": sha(&original_interface)},
                "packages": {"path": bundle.package_imports_path, "bytes": original_packages.len(), "sha256": sha(&original_packages)}
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    #[ignore = "requires matched Haskell worker and Rust frontend"]
    #[serial_test::serial]
    fn real_worker_source_boot_products_reuse_and_refuse_changed_boot() {
        use crate::artifacts::compile_targets;
        use crate::certified_products::ProductOrigin;

        struct RestoreEnvironment(Vec<(&'static str, Option<std::ffi::OsString>)>);
        impl Drop for RestoreEnvironment {
            fn drop(&mut self) {
                for (name, value) in self.0.drain(..) {
                    match value {
                        Some(value) => unsafe { std::env::set_var(name, value) },
                        None => unsafe { std::env::remove_var(name) },
                    }
                }
            }
        }
        let names = [
            "TIDEPOOL_COMPILE_CACHE_DIR",
            tidepool_extract_cmd::DAEMON_SOCKET_ENV,
        ];
        let _restore = RestoreEnvironment(
            names
                .iter()
                .map(|&name| (name, std::env::var_os(name)))
                .collect(),
        );
        let cache = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", cache.path());
            // Every invocation below starts a new worker. The test must not
            // accidentally validate reuse through a surrounding shared daemon.
            std::env::remove_var(tidepool_extract_cmd::DAEMON_SOCKET_ENV);
        }
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../bridge/haskell/test-source-boot/fixtures");
        for name in ["CacheEven.hs", "CacheEven.hs-boot", "CacheOdd.hs"] {
            fs::copy(fixtures.join(name), work.path().join(name)).unwrap();
        }
        let wrapper = fs::read_to_string(fixtures.join("CacheEntry.hs")).unwrap();
        let compile = |salt: u32| {
            compile_targets(
                &format!("{wrapper}\n-- distinct consumer {salt}\n"),
                &["result"],
                &[work.path().to_path_buf()],
                |_, _, _| {},
            )
            .unwrap()
        };
        let assert_origin =
            |artifacts: &crate::artifacts::CompiledArtifacts, expected| {
                for name in ["CacheEven", "CacheOdd"] {
                    assert!(artifacts.certified_groups.iter().any(|group|
                    group.owner().module == name && group.origin() == expected),
                    "{name} did not have expected {expected:?} product origin");
                    assert!(!artifacts
                        .certified_groups
                        .iter()
                        .any(|group| group.owner().module == name && group.origin() != expected));
                }
            };
        let cold = compile(0);
        assert_origin(&cold, ProductOrigin::Fresh);
        let warm = compile(1);
        assert_origin(&warm, ProductOrigin::Cached);
        let fresh = compile(2);
        assert_origin(&fresh, ProductOrigin::Cached);
        let boot = work.path().join("CacheEven.hs-boot");
        let original = fs::read(&boot).unwrap();
        fs::write(&boot, b"module CacheEven where\neven' :: Bool -> Bool\n").unwrap();
        assert!(compile_targets(
            &format!("{wrapper}\n-- changed boot\n"),
            &["result"],
            &[work.path().to_path_buf()],
            |_, _, _| {}
        )
        .is_err());
        fs::write(&boot, &original).unwrap();
        assert_origin(&compile(3), ProductOrigin::Cached);
        fs::write(
            &boot,
            [b"{-# LANGUAGE CPP #-}\n".as_slice(), &original].concat(),
        )
        .unwrap();
        assert!(compile_targets(
            &format!("{wrapper}\n-- untracked boot CPP\n"),
            &["result"],
            &[work.path().to_path_buf()],
            |_, _, _| {}
        )
        .is_err());
        fs::write(&boot, &original).unwrap();
        assert_origin(&compile(5), ProductOrigin::Cached);
    }

    fn digest(bytes: &[u8]) -> String {
        sha(bytes)
    }

    pub(super) fn product_bytes(unit: &str, module: &str, iface: &[u8]) -> Vec<u8> {
        let value = Value::Array(vec![
            Value::Text("TPMOD".into()),
            Value::Integer(1.into()),
            Value::Array(vec![Value::Array(vec![
                Value::Text(unit.into()),
                Value::Text(module.into()),
                Value::Bytes(iface.into()),
                Value::Array(vec![]),
            ])]),
        ]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).unwrap();
        bytes
    }

    pub(super) fn package_imports(unit: &str, module: &str, iface: &[u8]) -> Vec<u8> {
        package_imports_with_roots(unit, module, iface, vec![])
    }

    pub(crate) fn package_imports_with_roots(
        unit: &str,
        module: &str,
        iface: &[u8],
        roots: Vec<Value>,
    ) -> Vec<u8> {
        let value = Value::Array(vec![
            Value::Text("TPPKGROOTS".into()),
            Value::Text("2".into()),
            Value::Array(vec![
                Value::Text(unit.into()),
                Value::Text(module.into()),
                Value::Text(sha(iface)),
            ]),
            Value::Array(roots),
            Value::Array(vec![]),
        ]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).unwrap();
        bytes
    }

    pub(super) fn package_bundle(unit: &str, module: &str, iface: &[u8]) -> Vec<u8> {
        package_bundle_with_sidecars(vec![(
            unit.into(),
            module.into(),
            package_imports(unit, module, iface),
        )])
    }

    pub(crate) fn package_bundle_with_sidecars(
        sidecars: Vec<(String, String, Vec<u8>)>,
    ) -> Vec<u8> {
        let value = Value::Array(vec![
            Value::Text("TPPKGBUNDLES".into()),
            Value::Integer(1.into()),
            Value::Array(
                sidecars
                    .into_iter()
                    .map(|(unit, module, sidecar)| {
                        Value::Array(vec![
                            Value::Text(unit),
                            Value::Text(module),
                            Value::Bytes(sidecar),
                        ])
                    })
                    .collect(),
            ),
        ]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn unrelated_module_does_not_change_product_owner() {
        let first = product_bytes("u", "Library", &[0x42]);
        let second = product_bytes("u", "Unrelated", &[0x43]);
        let Value::Array(one) = ciborium::de::from_reader::<Value, _>(first.as_slice()).unwrap()
        else {
            unreachable!()
        };
        let Value::Array(two) = ciborium::de::from_reader::<Value, _>(second.as_slice()).unwrap()
        else {
            unreachable!()
        };
        let Value::Array(mut first_rows) = one[2].clone() else {
            unreachable!()
        };
        let Value::Array(second_rows) = two[2].clone() else {
            unreachable!()
        };
        first_rows.extend(second_rows);
        let mut combined = Vec::new();
        ciborium::ser::into_writer(
            &Value::Array(vec![
                Value::Text("TPMOD".into()),
                Value::Integer(1.into()),
                Value::Array(first_rows),
            ]),
            &mut combined,
        )
        .unwrap();
        let requirements = crate::prepared_artifact::production_requirements().unwrap();
        let parsed = tidepool_repr::execution_schema::parse_module_products(
            &combined,
            &requirements,
            product_decode_limits(),
        )
        .unwrap();
        let split = split_module_product_bytes(&combined, &parsed).unwrap();
        assert_eq!(split[0], first);
        assert_eq!(split[1], second);
        assert_eq!(
            module_version_for_product(
                b"compiler",
                &[],
                "source-sha",
                &[0x42],
                &split[0],
                &package_imports("u", "Library", &[0x42])
            ),
            module_version_for_product(
                b"compiler",
                &[],
                "source-sha",
                &[0x42],
                &first,
                &package_imports("u", "Library", &[0x42])
            ),
        );
    }

    #[test]
    fn candidate_product_carrier_keeps_exact_decode_and_refuses_corrupt_bytes() {
        let bytes = product_bytes("u", "Library", &[0x42]);
        let carrier = CandidateProduct::decode(bytes.clone()).unwrap();
        assert_eq!(carrier.bytes(), bytes);
        assert_eq!(carrier.decoded().unit, "u");
        assert_eq!(carrier.decoded().module, "Library");
        assert_eq!(carrier.decoded().interface, [0x42]);
        assert!(CandidateProduct::decode(b"corrupt product".to_vec()).is_none());
        let mut trailing = bytes;
        trailing.push(0);
        assert!(CandidateProduct::decode(trailing).is_none());
    }

    #[test]
    fn execution_capability_requires_current_full_local_source_owner() {
        let root = tempfile::tempdir().unwrap();
        let (graph, owners) =
            crate::execution_source::test_graph_with_local_source_dependency(root.path());
        let record = candidate_fixture(root.path(), "Unrelated");
        let make_bundle = |owner: CachedHomeOwner, proof: bool| CandidateBundle {
            owner,
            product: CandidateProduct::decode(record.products.clone()).unwrap(),
            source: record.source.clone(),
            source_sha256: record.source_sha256.clone(),
            iface_path: root.path().join("fixture.hi"),
            iface_sha256: sha(&record.interface),
            package_imports_path: root.path().join("fixture.packages"),
            package_imports_sha256: sha(&record.package_imports),
            package_imports_bytes: record.package_imports.clone(),
            evidence: record.evidence.clone(),
            target_source: record.target_source.clone(),
            origin: CandidateOrigin::Ordinary,
            original_module_interface: record.module_interface_proof.as_ref().unwrap().clone(),
            original_execution: proof.then(|| OriginalCandidateExecution {
                graph: Arc::clone(&graph),
            }),
            execution_admitted: false,
        };
        let key = |owner: &CachedHomeOwner| (owner.unit.clone(), owner.module.clone());
        let mut bundles = BTreeMap::from([
            (key(&owners[0]), make_bundle(owners[0].clone(), true)),
            (key(&owners[1]), make_bundle(owners[1].clone(), false)),
        ]);
        retain_closed_execution_capabilities(&mut bundles, &[]);
        assert!(
            bundles[&key(&owners[0])].execution_admitted,
            "local dependency requires exact owner, not its own graph"
        );
        bundles
            .get_mut(&key(&owners[1]))
            .unwrap()
            .owner
            .module_version = ModuleVersion([99; 32]);
        retain_closed_execution_capabilities(&mut bundles, &[]);
        assert!(!bundles[&key(&owners[0])].execution_admitted);
        assert!(
            bundles[&key(&owners[0])].original_execution.is_some(),
            "original custody is retained"
        );
        assert_eq!(
            bundles.len(),
            2,
            "native candidates survive execution-only refusal"
        );
        bundles.get_mut(&key(&owners[1])).unwrap().owner = owners[1].clone();
        bundles.insert(key(&owners[2]), make_bundle(owners[2].clone(), true));
        retain_closed_execution_capabilities(&mut bundles, &[]);
        assert!(
            bundles[&key(&owners[0])].execution_admitted,
            "the same original proof resumes with its original dependency"
        );
        assert!(
            !bundles[&key(&owners[2])].execution_admitted,
            "unrelated unsupported root is refused independently"
        );
        assert!(Arc::ptr_eq(
            &bundles[&key(&owners[0])]
                .original_execution
                .as_ref()
                .unwrap()
                .graph,
            &graph
        ));
        let retained = crate::execution_source::test_graph_requiring_original(
            &graph,
            &owners[1],
            graph.digest(),
        );
        bundles
            .get_mut(&key(&owners[0]))
            .unwrap()
            .original_execution = Some(OriginalCandidateExecution { graph: retained });
        retain_closed_execution_capabilities(&mut bundles, &[]);
        assert!(
            !bundles[&key(&owners[0])].execution_admitted,
            "explicit retained edge requires its graph ref"
        );
        bundles
            .get_mut(&key(&owners[1]))
            .unwrap()
            .original_execution = Some(OriginalCandidateExecution {
            graph: Arc::clone(&graph),
        });
        retain_closed_execution_capabilities(&mut bundles, &[]);
        assert!(bundles[&key(&owners[0])].execution_admitted);
    }

    #[test]
    fn original_execution_proof_requires_original_owner_producer_semantic_and_seal() {
        use crate::execution_source::{
            CertifiedExecutionSourceGraph, ExecutionSourceAdmission, ExecutionSourceGraphInput,
        };
        let root = tempfile::tempdir().unwrap();
        let mut record = candidate_fixture(root.path(), "Library");
        let owner = computed_owner(&record);
        let input = root.path().join("Input.hs");
        fs::write(&input, &record.target_source).unwrap();
        let make_graph = |producer: &[u8], semantic_sha256| {
            let ExecutionSourceAdmission::Available(graph) =
                CertifiedExecutionSourceGraph::admit(ExecutionSourceGraphInput {
                    producer:
                        crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
                            producer,
                        ),
                    semantic_sha256,
                    include: &record.include,
                    source_path: &input,
                    source: &record.target_source,
                    evidence: &record.evidence,
                    exact_imports: &BTreeMap::new(),
                    owners: std::slice::from_ref(&owner),
                    fresh_owners: &BTreeSet::from([crate::declaration_join::ExactModuleIdentity {
                        unit: owner.unit.clone(),
                        module: owner.module.clone(),
                    }]),
                    retained_sources: &BTreeMap::new(),
                    packages: &BTreeMap::new(),
                })
                .unwrap()
            else {
                panic!("graph");
            };
            graph
        };
        let graph = make_graph(&record.endpoint, None);
        let wrong_producer = make_graph(b"another producer", None);
        let wrong_semantic = make_graph(&record.endpoint, Some([8; 32]));
        let seal = |graph: &crate::execution_source::CertifiedExecutionSourceGraph| {
            let legacy =
                crate::certified_products::encode_home_certification(&owner, &[], &BTreeMap::new())
                    .unwrap();
            crate::certified_products::bind_home_execution_source(
                &legacy,
                &owner,
                graph.digest(),
                &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
            )
            .unwrap()
        };
        record.original_certification = seal(&graph);
        record.execution_source_sha256 = Some(graph.digest());
        let validate =
            |record: &Record,
             graph: Arc<crate::execution_source::CertifiedExecutionSourceGraph>| {
                validate_original_execution(
                    record,
                    graph,
                    &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
                )
            };
        assert!(validate(&record, Arc::clone(&graph)).is_some());
        let mut changed = record.clone();
        changed.original_owner.module_version = [99; 32];
        assert!(validate(&changed, Arc::clone(&graph)).is_none());
        changed = record.clone();
        changed.original_certification = b"corrupt seal".to_vec();
        assert!(validate(&changed, Arc::clone(&graph)).is_none());
        changed = record.clone();
        changed.execution_source_sha256 = Some(wrong_producer.digest());
        changed.original_certification = seal(&wrong_producer);
        assert!(validate(&changed, wrong_producer).is_none());
        changed = record;
        changed.execution_source_sha256 = Some(wrong_semantic.digest());
        changed.original_certification = seal(&wrong_semantic);
        assert!(validate(&changed, wrong_semantic).is_none());
    }

    #[test]
    fn product_module_bound_admits_measured_display_graph() {
        assert!(product_decode_limits().max_module_bytes >= 33_955_557);
        assert_eq!(RECORD_LIMIT, 32 << 20);
    }

    #[test]
    fn package_bundle_requires_exact_owner_pair_and_changes_module_version() {
        let product = product_bytes("u", "Library", &[0x42]);
        let requirements = crate::prepared_artifact::production_requirements().unwrap();
        let parsed = tidepool_repr::execution_schema::parse_module_products(
            &product,
            &requirements,
            product_decode_limits(),
        )
        .unwrap();
        let bundle = package_bundle("u", "Library", &[0x42]);
        let selected = split_package_imports(&bundle, &parsed).unwrap();
        let roots = &selected[&("u".into(), "Library".into())];
        let original =
            module_version_for_product(b"compiler", &[], "source-sha", &[0x42], &product, roots);
        let altered_roots = package_imports("u", "Library", &[0x43]);
        let altered = module_version_for_product(
            b"compiler",
            &[],
            "source-sha",
            &[0x42],
            &product,
            &altered_roots,
        );
        assert_ne!(original, altered);
        assert!(split_package_imports(&package_bundle("u", "Other", &[0x42]), &parsed).is_none());
        let mut trailing = bundle;
        trailing.push(0);
        assert!(split_package_imports(&trailing, &parsed).is_none());
    }

    fn fixture_record_dir(root: &Path) -> PathBuf {
        root_shard(
            &root.join(RECORD_DIR).join(sha(b"endpoint")),
            &absolute(root).unwrap(),
        )
    }

    fn write_record(
        root: &Path,
        source: &Path,
        unit: &str,
        module: &str,
        interface: &[u8],
        products: Vec<u8>,
    ) -> crate::recovery_artifacts::RecoveryModuleInterfaceRef {
        write_record_with_interface(root, source, unit, module, products, interface.to_vec())
    }

    fn write_record_with_interface(
        root: &Path,
        source: &Path,
        unit: &str,
        module: &str,
        products: Vec<u8>,
        interface: Vec<u8>,
    ) -> crate::recovery_artifacts::RecoveryModuleInterfaceRef {
        let source = fs::canonicalize(source).unwrap();
        let source_bytes = fs::read(&source).unwrap();
        let target_source = "target".to_owned();
        let evidence = DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: vec![
                SourceEvidence {
                    path: "@generated-source".into(),
                    sha256: digest(target_source.as_bytes()),
                },
                SourceEvidence {
                    path: source.clone(),
                    sha256: digest(&source_bytes),
                },
            ],
            resolutions: vec![],
            packages: vec![],
            modules: vec![ModuleEvidence {
                unit: unit.into(),
                module: module.into(),
                boot: false,
                source: source.clone(),
                imports: vec![],
                product: ProductAvailability::Ready,
            }],
        };
        let evidence: shared_evidence::SharedEvidence = evidence.into();
        let record = Record {
            evidence: evidence.clone(),
            module_interface_proof: None,
            execution_source: None,
            data: RecordData {
                evidence: evidence
                    .reference()
                    .expect("bounded dependency evidence")
                    .clone(),
                tag: "TPMCAN".into(),
                version: RECORD_VERSION,
                endpoint: b"endpoint".to_vec(),
                include: vec![absolute(root).unwrap()],
                products,
                unit: unit.into(),
                module: module.into(),
                source: source.clone(),
                source_sha256: digest(&source_bytes),
                interface: interface.to_vec(),
                package_imports: package_imports(unit, module, &interface),
                target_source,
                version_origin: CandidateVersionOrigin::Ordinary,
                original_owner: OriginalOwner {
                    unit: unit.into(),
                    module: module.into(),
                    module_version: [0; 32],
                    skinny_iface_sha256: [0; 32],
                    product_sha256: [0; 32],
                },
                original_certification: Vec::new(),
                module_interface: None,
                execution_source_sha256: None,
            },
        };
        let mut record = record;
        record.original_owner = OriginalOwner::from_owner(&computed_owner(&record));
        let owner = computed_owner(&record);
        let original = crate::recovery_artifacts::CertifiedRecoveryProduct::from_certification(
            owner.clone(),
            record.interface.clone(),
            record.products.clone(),
            record.package_imports.clone(),
            crate::certified_products::encode_home_certification(&owner, &[], &BTreeMap::new())
                .unwrap(),
        )
        .with_source_sha256(Sha256::digest(&source_bytes).into());
        let original = crate::certified_products::fixture_finalized_product(
            original,
            crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
                &record.endpoint,
            )
            .sha256(),
        );
        let dir = fixture_record_dir(root);
        fs::create_dir_all(&dir).unwrap();
        let producer_dir = dir.parent().unwrap();
        record.module_interface = Some(
            crate::recovery_artifacts::materialize_module_interface(
                producer_dir,
                original.module_interface().unwrap(),
                &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
                crate::recovery_artifacts::MaterializationMode::Durable,
            )
            .unwrap(),
        );
        let module_interface = record.module_interface.clone().unwrap();
        record.original_certification = original.certification_bytes().to_vec();
        let bytes = encode_record(&record).unwrap();
        shared_evidence::publish(producer_dir, &record.evidence).unwrap();
        let name = format!(
            "{}.cbor",
            sha(format!("{unit}:{module}:{}", source.display()).as_bytes())
        );
        fs::write(dir.join(name), bytes).unwrap();
        module_interface
    }

    pub(super) fn candidate_fixture(root: &Path, module: &str) -> Record {
        let source = root.join(format!("{module}.hs"));
        fs::write(&source, format!("module {module} where\n")).unwrap();
        // Defining owners have distinct interface bytes. A real GHC interface
        // contains its owner; sharing one marker across owners fabricates a
        // package-sidecar conflict at the immutable interface-content path.
        let interface = format!("u:{module}").into_bytes();
        let module_interface = write_record(
            root,
            &source,
            "u",
            module,
            &interface,
            product_bytes("u", module, &interface),
        );
        let mut record = fs::read_dir(fixture_record_dir(root))
            .unwrap()
            .map(|entry| read_record_path(&entry.unwrap().path()).unwrap())
            .find(|r| r.module == module)
            .unwrap();
        // The durable record and retained proof select the same authenticated carrier.
        assert_eq!(record.module_interface.as_ref(), Some(&module_interface));
        record.module_interface_proof = Some(
            crate::recovery_artifacts::recover_module_interface(
                fixture_record_dir(root).parent().unwrap(),
                record.module_interface.as_ref().unwrap(),
                &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
            )
            .unwrap(),
        );
        record
    }

    fn publication_fixture_report(record: &Record) -> (Vec<Record>, Vec<PublicationDisposition>) {
        let parsed = crate::certified_products::ParsedModuleProducts::decode(
            &record.products,
            &package_bundle(&record.unit, &record.module, &record.interface),
        )
        .unwrap();
        let mut dispositions = Vec::new();
        let (_, records) = eligible_records_with_report(
            &record.endpoint,
            &record.include,
            &record.evidence,
            parsed,
            &record.target_source,
            &CandidateVersionOrigin::Ordinary,
            &[],
            &mut |_, _, disposition| dispositions.push(disposition),
        );
        (records, dispositions)
    }

    #[test]
    fn candidate_admission_requires_exact_durable_and_in_memory_canonical_identity() {
        use crate::recovery_artifacts::{
            RecoveryCoreRef, RecoveryJoinRef, RecoveryModuleInterfaceRef,
        };
        let root = tempfile::tempdir().unwrap();
        let record = candidate_fixture(root.path(), "Library");
        let canonical = record.module_interface_proof.as_ref().unwrap().clone();
        let reference = record.module_interface.as_ref().unwrap();
        let core = reference.core.as_ref().unwrap();
        assert!(ValidatedRecord::admit(record.clone(), canonical.clone()).is_some());
        let absent = Record {
            data: RecordData {
                module_interface: None,
                ..record.data.clone()
            },
            ..record.clone()
        };
        assert!(ValidatedRecord::admit(absent, canonical.clone()).is_none());
        let mismatches = [
            RecoveryModuleInterfaceRef {
                interface: RecoveryJoinRef {
                    toolchain_identity_sha256: [91; 32],
                    ..reference.interface.clone()
                },
                ..reference.clone()
            },
            RecoveryModuleInterfaceRef {
                interface: RecoveryJoinRef {
                    unit: "other".into(),
                    ..reference.interface.clone()
                },
                ..reference.clone()
            },
            RecoveryModuleInterfaceRef {
                interface: RecoveryJoinRef {
                    module: "Other".into(),
                    ..reference.interface.clone()
                },
                ..reference.clone()
            },
            RecoveryModuleInterfaceRef {
                interface: RecoveryJoinRef {
                    skinny_iface_sha256: [91; 32],
                    ..reference.interface.clone()
                },
                ..reference.clone()
            },
            RecoveryModuleInterfaceRef {
                interface: RecoveryJoinRef {
                    package_imports_sha256: [91; 32],
                    ..reference.interface.clone()
                },
                ..reference.clone()
            },
            RecoveryModuleInterfaceRef {
                certificate_sha256: [91; 32],
                ..reference.clone()
            },
            RecoveryModuleInterfaceRef {
                core: None,
                ..reference.clone()
            },
            RecoveryModuleInterfaceRef {
                core: Some(RecoveryCoreRef {
                    sha256: [91; 32],
                    ..core.clone()
                }),
                ..reference.clone()
            },
            RecoveryModuleInterfaceRef {
                core: Some(RecoveryCoreRef {
                    bytes: core.bytes + 1,
                    ..core.clone()
                }),
                ..reference.clone()
            },
        ];
        for reference in mismatches {
            let inconsistent = Record {
                data: RecordData {
                    module_interface: Some(reference),
                    ..record.data.clone()
                },
                ..record.clone()
            };
            assert!(ValidatedRecord::admit(inconsistent, canonical.clone()).is_none());
        }
        let other = candidate_fixture(root.path(), "Other");
        assert!(ValidatedRecord::admit(record, other.module_interface_proof.unwrap(),).is_none());
    }

    #[test]
    fn publication_preparation_defers_writes_and_preserves_original_frames() {
        let root = tempfile::tempdir().unwrap();
        let record = candidate_fixture(root.path(), "Library");
        fs::rename(
            fixture_record_dir(root.path()),
            root.path().join("original-fixture-records"),
        )
        .unwrap();
        let package = package_bundle(&record.unit, &record.module, &record.interface);
        let parsed =
            crate::certified_products::ParsedModuleProducts::decode(&record.products, &package)
                .unwrap();
        let (products, prepared) = prepare_publication(
            &record.endpoint,
            &record.include,
            &record.evidence,
            parsed,
            &record.target_source,
            CandidateVersionOrigin::Ordinary,
            &[],
        );
        assert_eq!(products.len(), 1);
        assert_eq!(prepared.records.len(), 1);
        assert_eq!(prepared.records[0].products, record.products);
        assert_eq!(prepared.records[0].package_imports, record.package_imports);
        assert_eq!(version_hash(&prepared.records[0]), version_hash(&record));
        assert!(!fixture_record_dir(root.path()).exists());
        drop(prepared); // Discarding a prepared request has no durable side effects.
        assert!(!fixture_record_dir(root.path()).exists());
        let parsed = crate::certified_products::ParsedModuleProducts::decode(
            &record.products,
            b"invalid package bundle",
        )
        .unwrap();
        let (_, rejected) = prepare_publication(
            &record.endpoint,
            &record.include,
            &record.evidence,
            parsed,
            &record.target_source,
            CandidateVersionOrigin::Ordinary,
            &[],
        );
        assert!(rejected.records.is_empty());
        assert!(!fixture_record_dir(root.path()).exists());
    }

    #[test]
    fn publication_materialized_generated_dependency_is_eligible_but_request_target_is_not() {
        let root = tempfile::tempdir().unwrap();
        let mut record = candidate_fixture(root.path(), "Tidepool.Effects.Core");
        let source = root.path().join("Tidepool/Effects/Core.hs");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::rename(&record.source, &source).unwrap();
        record.source = fs::canonicalize(source).unwrap();
        record.evidence.make_mut().sources[1].path = record.source.clone();
        record.evidence.make_mut().modules[0].source = record.source.clone();
        let (eligible, dispositions) = publication_fixture_report(&record);
        assert_eq!(dispositions, [PublicationDisposition::Eligible]);
        assert_eq!(eligible.len(), 1);
        assert_eq!(selected_record_root(&eligible[0]), Some(root.path().into()));

        record.target_source = fs::read_to_string(&record.source).unwrap();
        record.evidence.make_mut().sources.remove(1);
        record.evidence.make_mut().sources[0].sha256 = sha(record.target_source.as_bytes());
        record.evidence.make_mut().modules[0].source = "@generated-source".into();
        let (eligible, dispositions) = publication_fixture_report(&record);
        assert!(eligible.is_empty());
        assert_eq!(
            dispositions,
            [PublicationDisposition::GeneratedRequestTarget]
        );
    }

    #[test]
    fn publication_disposition_retains_source_and_package_proof_refusals() {
        let root = tempfile::tempdir().unwrap();
        let mut record = candidate_fixture(root.path(), "Library");
        record.evidence.make_mut().sources[1].sha256 = sha(b"unconsumed source");
        let (eligible, dispositions) = publication_fixture_report(&record);
        assert!(eligible.is_empty());
        assert_eq!(
            dispositions,
            [PublicationDisposition::InvocationProofRejected]
        );
        record.evidence.make_mut().sources[1].sha256 = sha(&fs::read(&record.source).unwrap());
        let parsed = crate::certified_products::ParsedModuleProducts::decode(
            &record.products,
            &package_bundle(&record.unit, &record.module, &[0x43]),
        )
        .unwrap();
        let mut dispositions = Vec::new();
        let (_, eligible) = eligible_records_with_report(
            &record.endpoint,
            &record.include,
            &record.evidence,
            parsed,
            &record.target_source,
            &CandidateVersionOrigin::Ordinary,
            &[],
            &mut |_, _, disposition| dispositions.push(disposition),
        );
        assert!(eligible.is_empty());
        assert_eq!(
            dispositions,
            [PublicationDisposition::PackageWitnessRejected]
        );
    }

    #[test]
    fn publication_payload_limit_reports_actual_encoded_size_with_byte_strings() {
        let root = tempfile::tempdir().unwrap();
        let mut record = candidate_fixture(root.path(), "Library");
        record.products = vec![0xff; RECORD_LIMIT - 1];
        let rejection = encode_record_checked(&record).unwrap_err();
        assert_eq!(rejection.disposition, PublicationDisposition::PayloadLimit);
        assert!(record.products.len() < RECORD_LIMIT);
        assert!(rejection.payload_bytes.unwrap() > RECORD_LIMIT);
    }

    #[test]
    fn publication_diagnostics_preserve_record_encoding_bytes() {
        let root = tempfile::tempdir().unwrap();
        let record = candidate_fixture(root.path(), "Library");
        let bytes = encode_record_checked(&record).unwrap();
        let payload = shared_evidence::encode_record(&record).unwrap();
        let header = serde_json::to_vec(&RecordHeader::for_record(&record, &payload)).unwrap();
        assert_eq!(&bytes[..8], RECORD_MAGIC);
        assert_eq!(&bytes[8..12], &(header.len() as u32).to_be_bytes());
        assert_eq!(&bytes[12..12 + header.len()], header);
        assert_eq!(&bytes[12 + header.len()..], payload);
    }

    #[test]
    fn candidate_byte_strings_preserve_opaque_buffers_and_module_identity() {
        let root = tempfile::tempdir().unwrap();
        let record = candidate_fixture(root.path(), "Library");
        let identity = version_hash(&record);
        let framed = encode_record_checked(&record).unwrap();
        let header_len = u32::from_be_bytes(framed[8..12].try_into().unwrap()) as usize;
        let payload = &framed[12 + header_len..];
        let Value::Map(fields) = ciborium::de::from_reader::<Value, _>(payload).unwrap() else {
            panic!("record must be a map")
        };
        for (name, bytes) in [
            ("endpoint", &record.endpoint),
            ("products", &record.products),
            ("interface", &record.interface),
            ("package_imports", &record.package_imports),
        ] {
            assert_eq!(
                fields
                    .iter()
                    .find(|(key, _)| key.as_text() == Some(name))
                    .unwrap()
                    .1,
                Value::Bytes(bytes.clone())
            );
        }
        let header = RecordHeader::for_record(&record, payload);
        let decoded = shared_evidence::decode_record(
            payload,
            &header,
            fixture_record_dir(root.path()).parent().unwrap(),
            &mut shared_evidence::ReadBudget::default(),
        )
        .unwrap();
        assert_eq!(decoded.endpoint, record.endpoint);
        assert_eq!(decoded.products, record.products);
        assert_eq!(decoded.interface, record.interface);
        assert_eq!(decoded.package_imports, record.package_imports);
        assert_eq!(version_hash(&decoded), identity);
        assert_eq!(encode_record_checked(&decoded).unwrap(), framed);
    }

    #[test]
    fn candidate_byte_strings_refuse_legacy_arrays_and_oversized_fields() {
        let root = tempfile::tempdir().unwrap();
        let record = candidate_fixture(root.path(), "Library");
        let payload = shared_evidence::encode_record(&record).unwrap();
        let Value::Map(fields) = ciborium::de::from_reader::<Value, _>(payload.as_slice()).unwrap()
        else {
            unreachable!()
        };
        for name in ["endpoint", "products", "interface", "package_imports"] {
            let mut legacy = fields.clone();
            let field = &mut legacy
                .iter_mut()
                .find(|(key, _)| key.as_text() == Some(name))
                .unwrap()
                .1;
            let Value::Bytes(bytes) = field else {
                unreachable!()
            };
            *field = Value::Array(bytes.iter().map(|b| Value::Integer((*b).into())).collect());
            let mut encoded = Vec::new();
            ciborium::ser::into_writer(&Value::Map(legacy), &mut encoded).unwrap();
            assert!(ciborium::de::from_reader::<RecordData, _>(encoded.as_slice()).is_err());
        }
        for (name, limit) in [("endpoint", 4096), ("package_imports", 4 << 20)] {
            let mut oversized = fields.clone();
            oversized
                .iter_mut()
                .find(|(key, _)| key.as_text() == Some(name))
                .unwrap()
                .1 = Value::Bytes(vec![0; limit + 1]);
            let mut encoded = Vec::new();
            ciborium::ser::into_writer(&Value::Map(oversized), &mut encoded).unwrap();
            assert!(ciborium::de::from_reader::<RecordData, _>(encoded.as_slice()).is_err());
        }
    }

    #[test]
    fn candidate_byte_string_migration_refuses_previous_framing_and_record_version() {
        let root = tempfile::tempdir().unwrap();
        let mut record = candidate_fixture(root.path(), "Library");
        let mut framed = encode_record_checked(&record).unwrap();
        framed[..8].copy_from_slice(b"TPCREC9\n");
        let path = root.path().join("old-framing.cbor");
        fs::write(&path, framed).unwrap();
        assert!(read_record_path(&path).is_none());
        record.version = RECORD_VERSION - 1;
        let scratch = tempfile::tempdir().unwrap();
        let include = record.include.clone();
        assert!(select_records(
            b"endpoint",
            &include,
            scratch.path(),
            vec![(record, CandidateOrigin::Ordinary)]
        )
        .unwrap()
        .by_owner
        .is_empty());
    }

    fn import_candidate(record: &mut Record, dependency: &Record) {
        use crate::cache::{ImportQualifier, ModuleImportEvidence, ResolutionEvidence};
        let selected = dependency.source.clone();
        record.evidence.make_mut().modules[0]
            .imports
            .push(ModuleImportEvidence {
                qualifier: ImportQualifier::ThisUnit(dependency.unit.clone()),
                module: dependency.module.clone(),
                boot: false,
                selected: Some(selected.clone()),
            });
        record
            .evidence
            .make_mut()
            .resolutions
            .push(ResolutionEvidence {
                qualifier: ImportQualifier::ThisUnit(dependency.unit.clone()),
                module: dependency.module.clone(),
                boot: false,
                selected: Some(selected.clone()),
                candidates: vec![selected],
            });
        record.evidence.make_mut().sources.extend(
            dependency
                .evidence
                .sources
                .iter()
                .filter(|s| s.path != Path::new("@generated-source"))
                .cloned(),
        );
        record
            .evidence
            .make_mut()
            .modules
            .extend(dependency.evidence.modules.iter().cloned());
        record
            .evidence
            .make_mut()
            .resolutions
            .extend(dependency.evidence.resolutions.iter().cloned());
        assert!(record.evidence.valid(&record.target_source));
    }

    fn select_context_records(
        scratch: &Path,
        records: Vec<Record>,
        context: &ExactCandidateContext,
    ) -> CandidateSet {
        select_records_inner(
            b"endpoint",
            &[],
            scratch,
            records
                .into_iter()
                .map(|r| (r, CandidateOrigin::Ordinary))
                .collect(),
            Some(context),
        )
        .unwrap()
    }

    #[test]
    fn exact_candidates_keep_greatest_closed_subset_and_original_bytes() {
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let mut a = candidate_fixture(root.path(), "A");
        let mut b = candidate_fixture(root.path(), "B");
        let c = candidate_fixture(root.path(), "C");
        let d = candidate_fixture(root.path(), "D");
        let expected_products = d.products.clone();
        let expected_packages = d.package_imports.clone();
        import_candidate(&mut b, &c);
        import_candidate(&mut a, &b);
        let context =
            ExactCandidateContext::new(BTreeSet::from([("u".into(), "C".into())]), BTreeSet::new());
        let selected = select_context_records(scratch.path(), vec![a, b, c, d], &context);
        assert_eq!(
            selected.by_owner.keys().cloned().collect::<Vec<_>>(),
            vec![("u".into(), "D".into())]
        );
        let manifest: Value =
            ciborium::de::from_reader(fs::File::open(&selected.manifest_path).unwrap()).unwrap();
        let fields = manifest.as_array().unwrap();
        assert_eq!(fields[1].as_text(), Some("10"));
        let row = fields[4].as_array().unwrap()[0].as_array().unwrap();
        assert_eq!(row.len(), 16);
        let original = fs::read(row[13].as_text().unwrap()).unwrap();
        let bundle = &selected.by_owner[&("u".into(), "D".into())];
        assert_eq!(original, expected_products);
        assert_eq!(original, bundle.product.bytes());
        let packages = fs::read(row[11].as_text().unwrap()).unwrap();
        assert_eq!(packages, expected_packages);
        assert_eq!(packages, bundle.package_imports_bytes);
        assert_eq!(row[12].as_text(), Some(sha(&packages).as_str()));
        assert_eq!(row[7].as_text(), Some(sha(&original).as_str()));
        assert_eq!(
            row[6].as_text(),
            Some(hex(&bundle.owner.module_version.0).as_str())
        );
    }

    #[test]
    fn exact_candidates_reuse_a_dependency_original_without_offering_its_root() {
        use crate::declaration_join::ExactModuleIdentity;
        use crate::execution_source::{
            CertifiedExecutionSourceGraph, ExecutionSourceAdmission, ExecutionSourceGraphInput,
        };
        use crate::recovery_artifacts::CertifiedRecoveryProduct;
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let mut a = candidate_fixture(root.path(), "A");
        let b = candidate_fixture(root.path(), "B");
        assert_ne!(a.interface, b.interface);
        assert_ne!(a.module_interface, b.module_interface);
        // Reissue the defining product after its authored dependency changes.
        // The canonical carrier and native seal must name the same exact B.
        fs::write(&a.source, "module A where\nimport B\n").unwrap();
        a.source_sha256 = sha(&fs::read(&a.source).unwrap());
        for source in &mut a.evidence.make_mut().sources {
            if source.path == a.data.source {
                source.sha256 = a.data.source_sha256.clone();
            }
        }
        import_candidate(&mut a, &b);
        let owners = vec![computed_owner(&a), computed_owner(&b)];
        a.original_owner = OriginalOwner::from_owner(&owners[0]);
        let canonical_requirements = BTreeMap::from([(
            (b.unit.clone(), b.module.clone()),
            owners[1].skinny_iface_sha256,
        )]);
        let fresh_a = CertifiedRecoveryProduct::from_certification(
            owners[0].clone(),
            a.interface.clone(),
            a.products.clone(),
            a.package_imports.clone(),
            crate::certified_products::encode_home_certification(&owners[0], &[], &BTreeMap::new())
                .unwrap(),
        )
        .with_source_sha256(parse_sha(&a.source_sha256).unwrap());
        let finalized_a = crate::certified_products::fixture_finalized_product_with_requirements(
            fresh_a,
            crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(&a.endpoint)
                .sha256(),
            Some(canonical_requirements.clone()),
        );
        let canonical_a = finalized_a.module_interface().unwrap().clone();
        assert_eq!(canonical_a.requirements(), &canonical_requirements);
        assert_eq!(
            canonical_a.source_sha256(),
            parse_sha(&a.source_sha256).unwrap()
        );
        a.module_interface = Some(
            crate::recovery_artifacts::materialize_module_interface(
                fixture_record_dir(root.path()).parent().unwrap(),
                &canonical_a,
                &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
                crate::recovery_artifacts::MaterializationMode::Durable,
            )
            .unwrap(),
        );
        a.module_interface_proof = Some(canonical_a);
        a.original_certification = finalized_a.certification_bytes().to_vec();
        let input = root.path().join("Input.hs");
        fs::write(&input, &a.target_source).unwrap();
        let ExecutionSourceAdmission::Available(graph) =
            CertifiedExecutionSourceGraph::admit(ExecutionSourceGraphInput {
                producer: crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
                    &a.endpoint,
                ),
                semantic_sha256: None,
                include: &a.include,
                source_path: &input,
                source: &a.target_source,
                evidence: &a.evidence,
                exact_imports: &BTreeMap::new(),
                owners: &owners,
                fresh_owners: &owners
                    .iter()
                    .map(|owner| ExactModuleIdentity {
                        unit: owner.unit.clone(),
                        module: owner.module.clone(),
                    })
                    .collect(),
                retained_sources: &BTreeMap::new(),
                packages: &BTreeMap::new(),
            })
            .unwrap()
        else {
            panic!("fixture graph unavailable")
        };
        a.original_certification = crate::certified_products::bind_home_execution_source(
            &a.original_certification,
            &owners[0],
            graph.digest(),
            &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
        )
        .unwrap();
        a.execution_source_sha256 = Some(graph.digest());
        a.execution_source = Some(graph);
        crate::certified_products::validate_canonical_native_bytes(
            &owners[0],
            &a.original_certification,
            &a.interface,
            &a.package_imports,
            Some(parse_sha(&a.source_sha256).unwrap()),
            a.module_interface_proof.as_ref().unwrap(),
        )
        .unwrap();
        let original = CertifiedRecoveryProduct::from_certification(
            owners[1].clone(),
            b.interface.clone(),
            b.products.clone(),
            b.package_imports.clone(),
            b.original_certification.clone(),
        )
        .with_source_sha256(parse_sha(&b.source_sha256).unwrap())
        .with_module_interface(b.module_interface_proof.as_ref().unwrap().clone())
        .unwrap();
        let context =
            ExactCandidateContext::new(BTreeSet::from([("u".into(), "B".into())]), BTreeSet::new())
                .with_originals(vec![original])
                .with_interface_seals(canonical_requirements.clone());
        let selected = select_context_records(scratch.path(), vec![a.clone(), b.clone()], &context);
        assert_eq!(
            selected.by_owner.keys().cloned().collect::<Vec<_>>(),
            vec![("u".into(), "A".into())]
        );
        assert_eq!(
            selected.by_owner[&("u".into(), "A".into())].product.bytes(),
            a.products
        );

        let manifest: Value =
            ciborium::de::from_reader(fs::File::open(&selected.manifest_path).unwrap()).unwrap();
        let fields = manifest.as_array().unwrap();
        assert_eq!(fields.len(), 7);
        assert_eq!(fields[1].as_text(), Some("10"));
        let parcel = fields[5].as_array().unwrap();
        assert_eq!(parcel.len(), 2);
        let descriptors = parcel[0].as_array().unwrap();
        assert_eq!(descriptors.len(), 1);
        let descriptor = descriptors[0].as_array().unwrap();
        assert_eq!(descriptor.len(), 2);
        let graph_path = Path::new(descriptor[1].as_text().unwrap());
        assert_eq!(graph_path.parent(), selected.manifest_path.parent());
        let captured = fs::read(graph_path).unwrap();
        assert_eq!(descriptor[0].as_text(), Some(sha(&captured).as_str()));
        assert_eq!(captured, a.execution_source.as_ref().unwrap().bytes());

        // Missing original proof retains the old conservative refusal. A
        // same-name candidate cannot replace the protected dependency.
        let without_original =
            ExactCandidateContext::new(context.protected.clone(), BTreeSet::new())
                .with_interface_seals(canonical_requirements);
        assert!(select_context_records(
            scratch.path(),
            vec![a.clone(), b.clone()],
            &without_original
        )
        .by_owner
        .is_empty());
        // A planned generated module cannot be satisfied by an old original.
        let reserved = ExactCandidateContext {
            reserved: BTreeSet::from(["B".into()]),
            ..context
        };
        assert!(
            select_context_records(scratch.path(), vec![a, b], &reserved)
                .by_owner
                .is_empty()
        );
    }

    #[test]
    fn exact_candidates_use_selected_paths_and_ignore_unreached_transaction_owners() {
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let mut a = candidate_fixture(root.path(), "A");
        let mut b = candidate_fixture(root.path(), "B");
        let hidden = candidate_fixture(root.path(), "Hidden");
        // A selected the original B. A different same-named fresh body is not
        // proof of that edge, even when both files contain the same bytes.
        import_candidate(&mut a, &b);
        let relocated = root.path().join("FreshB.hs");
        fs::copy(&b.source, &relocated).unwrap();
        b.source = fs::canonicalize(relocated).unwrap();
        b.evidence.make_mut().sources[1].path = b.source.clone();
        b.evidence.make_mut().modules[0].source = b.source.clone();
        // Hidden was consumed in another target in the same compilation, but
        // there is no selected import edge from B to Hidden.
        b.evidence.make_mut().sources.extend(
            hidden
                .evidence
                .sources
                .iter()
                .filter(|s| s.path != Path::new("@generated-source"))
                .cloned(),
        );
        b.evidence
            .make_mut()
            .modules
            .extend(hidden.evidence.modules.clone());
        let context = ExactCandidateContext::new(
            BTreeSet::from([("u".into(), "Hidden".into())]),
            BTreeSet::new(),
        );
        let selected = select_context_records(scratch.path(), vec![a, b, hidden], &context);
        assert_eq!(
            selected.by_owner.keys().cloned().collect::<Vec<_>>(),
            vec![("u".into(), "B".into())]
        );
    }

    #[test]
    fn exact_candidates_refuse_missing_ambiguous_and_reserved_dependencies() {
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let mut a = candidate_fixture(root.path(), "A");
        let b = candidate_fixture(root.path(), "B");
        import_candidate(&mut a, &b);
        let empty = ExactCandidateContext::new(BTreeSet::new(), BTreeSet::new());
        assert!(
            select_context_records(scratch.path(), vec![a.clone()], &empty)
                .by_owner
                .is_empty()
        );
        let reserved = ExactCandidateContext::new(BTreeSet::new(), BTreeSet::from(["B".into()]));
        assert!(
            select_context_records(scratch.path(), vec![a.clone(), b.clone()], &reserved)
                .by_owner
                .is_empty()
        );
        a.evidence.make_mut().modules[0].imports[0].qualifier =
            crate::cache::ImportQualifier::Unqualified;
        a.evidence.make_mut().resolutions[0].qualifier = crate::cache::ImportQualifier::Unqualified;
        let mut ambiguous = a.evidence.make_mut().modules[1].clone();
        ambiguous.unit = "another-unit".into();
        a.evidence.make_mut().modules.push(ambiguous);
        assert!(a.evidence.valid(&a.target_source));
        let selected = select_context_records(scratch.path(), vec![a, b], &empty);
        assert_eq!(selected.by_owner.len(), 1);
        assert!(selected.by_owner.contains_key(&("u".into(), "B".into())));
    }

    fn store_fixture(root: &Path, record: &Record, name: &str) {
        let producer = root.join(RECORD_DIR).join(sha(b"endpoint"));
        fs::create_dir_all(&producer).unwrap();
        let dir = root_shard(&producer, &selected_record_root(record).unwrap());
        let mut record = record.clone();
        if let Some(canonical) = &record.module_interface_proof {
            record.module_interface = Some(
                crate::recovery_artifacts::materialize_module_interface(
                    &producer,
                    canonical,
                    &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
                    crate::recovery_artifacts::MaterializationMode::Durable,
                )
                .unwrap(),
            );
        }
        shared_evidence::publish(&producer, &record.evidence).unwrap();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("{name}.cbor")),
            encode_record(&record).unwrap(),
        )
        .unwrap();
    }

    #[test]
    #[serial_test::serial]
    fn ordinary_cache_offer_admits_a_deterministic_owner_subset_with_original_proofs() {
        let cache = tempfile::tempdir().unwrap();
        let sources = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let records = ["A", "B", "C"]
            .into_iter()
            .map(|module| candidate_fixture(sources.path(), module))
            .collect::<Vec<_>>();
        for (index, record) in records.iter().enumerate() {
            store_fixture(cache.path(), record, &format!("candidate-{index}"));
        }
        unsafe {
            std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", cache.path());
        }
        let include = records[0].include.clone();
        let limits = CacheOfferLimits {
            owners: 2,
            payload_bytes: PAYLOAD_LIMIT as u64,
        };
        let first = ordinary_records_with_limits(b"endpoint", &include, false, limits).unwrap();
        let second = ordinary_records_with_limits(b"endpoint", &include, false, limits).unwrap();
        let names = |offer: &CacheOffer| {
            offer
                .records
                .iter()
                .map(|record| record.module.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&first), vec!["A".to_owned(), "B".to_owned()]);
        assert_eq!(names(&second), vec!["A".to_owned(), "B".to_owned()]);
        assert_eq!(first.diagnostics.count(CacheOfferOmission::OwnerLimit), 1);

        let selected = select_records_inner(
            b"endpoint",
            &include,
            scratch.path(),
            first
                .records
                .into_iter()
                .map(|record| (record, CandidateOrigin::Ordinary))
                .collect(),
            None,
        )
        .unwrap();
        assert_eq!(
            selected.by_owner.keys().cloned().collect::<Vec<_>>(),
            vec![("u".into(), "A".into()), ("u".into(), "B".into())]
        );
        for record in &records[..2] {
            let bundle = &selected.by_owner[&(record.unit.clone(), record.module.clone())];
            assert_eq!(bundle.product.bytes(), record.products);
            assert_eq!(
                bundle.original_module_interface,
                record.module_interface_proof.clone().unwrap()
            );
        }
    }

    #[test]
    #[serial_test::serial]
    fn ordinary_cache_offer_skips_an_unfittable_owner_and_admits_a_later_small_record() {
        let cache = tempfile::tempdir().unwrap();
        let sources = tempfile::tempdir().unwrap();
        let mut large = candidate_fixture(sources.path(), "A");
        large.target_source = "x".repeat(4096);
        large.evidence.make_mut().sources[0].sha256 = sha(large.target_source.as_bytes());
        large.data.evidence = large.evidence.reference().unwrap().clone();
        let small = candidate_fixture(sources.path(), "B");
        store_fixture(cache.path(), &large, "large");
        store_fixture(cache.path(), &small, "small");
        let producer = cache.path().join(RECORD_DIR).join(sha(b"endpoint"));
        let shard = root_shard(&producer, &selected_record_root(&small).unwrap());
        let small_path = shard.join("small.cbor");
        let small_payload = read_header(&mut fs::File::open(small_path).unwrap())
            .unwrap()
            .payload_len;
        assert!(
            read_header(&mut fs::File::open(shard.join("large.cbor")).unwrap())
                .unwrap()
                .payload_len
                > small_payload
        );
        unsafe {
            std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", cache.path());
        }
        let offer = ordinary_records_with_limits(
            b"endpoint",
            &small.include,
            false,
            CacheOfferLimits {
                owners: 2,
                payload_bytes: small_payload,
            },
        )
        .unwrap();
        assert_eq!(offer.records.len(), 1);
        assert_eq!(offer.records[0].module, "B");
        assert_eq!(offer.diagnostics.count(CacheOfferOmission::ReadBudget), 1);
    }

    #[test]
    #[serial_test::serial]
    fn ordinary_cache_offer_keeps_independent_proof_when_a_selected_owner_lacks_closure() {
        let cache = tempfile::tempdir().unwrap();
        let sources = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let mut dependent = candidate_fixture(sources.path(), "A");
        let independent = candidate_fixture(sources.path(), "B");
        let beyond_limit = candidate_fixture(sources.path(), "C");
        let owner = computed_owner(&dependent);
        let fresh = crate::recovery_artifacts::CertifiedRecoveryProduct::from_certification(
            owner.clone(),
            dependent.interface.clone(),
            dependent.products.clone(),
            dependent.package_imports.clone(),
            crate::certified_products::encode_home_certification(&owner, &[], &BTreeMap::new())
                .unwrap(),
        )
        .with_source_sha256(parse_sha(&dependent.source_sha256).unwrap());
        let missing_requirement =
            BTreeMap::from([(("u".to_owned(), "Missing".to_owned()), [0x55; 32])]);
        let finalized = crate::certified_products::fixture_finalized_product_with_requirements(
            fresh,
            crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
                &dependent.endpoint,
            )
            .sha256(),
            Some(missing_requirement),
        );
        let canonical = finalized.module_interface().unwrap().clone();
        dependent.module_interface = Some(
            crate::recovery_artifacts::materialize_module_interface(
                fixture_record_dir(sources.path()).parent().unwrap(),
                &canonical,
                &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
                crate::recovery_artifacts::MaterializationMode::Durable,
            )
            .unwrap(),
        );
        dependent.module_interface_proof = Some(canonical);
        dependent.original_certification = finalized.certification_bytes().to_vec();
        dependent.original_owner = OriginalOwner::from_owner(&owner);
        for (index, record) in [&dependent, &independent, &beyond_limit]
            .into_iter()
            .enumerate()
        {
            store_fixture(cache.path(), record, &format!("candidate-{index}"));
        }
        unsafe {
            std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", cache.path());
        }
        let include = dependent.include.clone();
        let offer = ordinary_records_with_limits(
            b"endpoint",
            &include,
            false,
            CacheOfferLimits {
                owners: 2,
                payload_bytes: PAYLOAD_LIMIT as u64,
            },
        )
        .unwrap();
        assert_eq!(
            offer
                .records
                .iter()
                .map(|record| record.module.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "B"]
        );
        let selected = select_records_inner(
            b"endpoint",
            &include,
            scratch.path(),
            offer
                .records
                .into_iter()
                .map(|record| (record, CandidateOrigin::Ordinary))
                .collect(),
            None,
        )
        .unwrap();
        assert_eq!(
            selected.by_owner.keys().cloned().collect::<Vec<_>>(),
            vec![("u".into(), "B".into())]
        );
        assert_eq!(
            selected.by_owner[&("u".into(), "B".into())].product.bytes(),
            independent.products
        );
    }

    #[test]
    fn corrupt_canonical_certificate_remains_a_hard_deployment_refusal() {
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let mut record = candidate_fixture(root.path(), "Library");
        let include = record.include.clone();
        let deployment = || CandidateOrigin::Deployment {
            interface: root.path().join("Library.hi"),
            packages: root.path().join("Library.packages"),
        };
        let admitted = select_records_inner(
            b"endpoint",
            &include,
            scratch.path(),
            vec![(record.clone(), deployment())],
            None,
        )
        .unwrap();
        assert_eq!(admitted.by_owner.len(), 1);

        // Keep the same in-memory canonical proof and alter only its sealed
        // certification, so both controls traverse canonical validation.
        record.original_certification.push(0);
        let selected = select_records_inner(
            b"endpoint",
            &include,
            scratch.path(),
            vec![(record, deployment())],
            None,
        );
        assert!(selected.is_none());
    }

    #[test]
    #[serial_test::serial]
    fn publication_preserves_dependency_products_per_ordered_include_recipe() {
        let cache = tempfile::tempdir().unwrap();
        let sources = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let mut importer = candidate_fixture(sources.path(), "Importer");
        let dependency = candidate_fixture(sources.path(), "Dependency");
        import_candidate(&mut importer, &dependency);
        let sources = absolute(sources.path()).unwrap();
        let extra = absolute(extra.path()).unwrap();
        let full = vec![extra.clone(), sources.clone()];
        let narrow = vec![sources.clone()];
        let reversed = vec![sources, extra];
        importer.include = full.clone();
        unsafe {
            std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", cache.path());
        }
        let publish = |record: &Record| {
            let parsed = crate::certified_products::ParsedModuleProducts::decode(
                &record.products,
                &package_bundle(&record.unit, &record.module, &record.interface),
            )
            .unwrap();
            let (_, publication) = prepare_publication(
                &record.endpoint,
                &record.include,
                &record.evidence,
                parsed,
                &record.target_source,
                CandidateVersionOrigin::Ordinary,
                &[],
            );
            assert_eq!(publication.records.len(), 1);
            publish_prepared(publication);
        };
        publish(&importer);
        let mut expected = Vec::new();
        for (include, interface) in [(&full, 0x42), (&narrow, 0x43), (&reversed, 0x44)] {
            let mut record = dependency.clone();
            record.include = include.clone();
            record.interface = vec![interface];
            record.products = product_bytes(&record.unit, &record.module, &record.interface);
            record.package_imports =
                package_imports(&record.unit, &record.module, &record.interface);
            record.original_owner = OriginalOwner::from_owner(&computed_owner(&record));
            expected.push(version_hash(&record));
            publish(&record);
        }
        let context = ExactCandidateContext::new(BTreeSet::new(), BTreeSet::new());
        for (include, version) in [&full, &narrow, &reversed].into_iter().zip(expected) {
            for disjoint in [false, true] {
                let records = ordinary_records(b"endpoint", include, disjoint).unwrap();
                let selected = select_records_inner(
                    b"endpoint",
                    include,
                    scratch.path(),
                    records
                        .into_iter()
                        .map(|record| (record, CandidateOrigin::Ordinary))
                        .collect(),
                    disjoint.then_some(&context),
                )
                .unwrap();
                assert_eq!(
                    selected
                        .by_owner
                        .get(&("u".into(), "Dependency".into()))
                        .expect("dependency survives publication under another recipe")
                        .owner
                        .module_version
                        .0,
                    version,
                    "select the dependency from the exact ordered recipe"
                );
                if include == &full {
                    assert!(selected
                        .by_owner
                        .contains_key(&("u".into(), "Importer".into())));
                } else if !disjoint {
                    assert!(!selected
                        .by_owner
                        .contains_key(&("u".into(), "Importer".into())));
                }
            }
        }
        fs::write(&dependency.source, "module Dependency where\nchanged\n").unwrap();
        let records = ordinary_records(b"endpoint", &full, true).unwrap();
        let selected = select_records_inner(
            b"endpoint",
            &full,
            scratch.path(),
            records
                .into_iter()
                .map(|record| (record, CandidateOrigin::Ordinary))
                .collect(),
            Some(&context),
        )
        .unwrap();
        assert!(
            selected.by_owner.is_empty(),
            "changed dependency invalidates its importer"
        );
        let old_namespace = cache.path().join("module-candidates-v11");
        fs::rename(cache.path().join(RECORD_DIR), &old_namespace).unwrap();
        assert!(ordinary_records(b"endpoint", &full, false)
            .unwrap()
            .is_empty());
        assert!(old_namespace.is_dir(), "old records remain on disk");
    }

    #[test]
    #[serial_test::serial]
    fn exact_discovery_crosses_include_recipes_but_refuses_current_shadow() {
        let cache = tempfile::tempdir().unwrap();
        let sources = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let mut record = candidate_fixture(cache.path(), "Library");
        let source = sources.path().join("Library.hs");
        fs::copy(&record.source, &source).unwrap();
        record.source = absolute(&source).unwrap();
        record.evidence.make_mut().sources[1].path = record.source.clone();
        record.evidence.make_mut().modules[0].source = record.source.clone();
        record.include = vec![absolute(sources.path()).unwrap()];
        record.original_owner = OriginalOwner::from_owner(&computed_owner(&record));
        let original_version = version_hash(&record);
        store_fixture(cache.path(), &record, "original");
        unsafe {
            std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", cache.path());
        }
        let include = vec![extra.path().into(), sources.path().into()];
        assert!(ordinary_records(b"endpoint", &include, false)
            .unwrap()
            .is_empty());
        let records = ordinary_records(b"endpoint", &include, true).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(version_hash(&records[0]), original_version);
        let context = ExactCandidateContext::new(BTreeSet::new(), BTreeSet::new());
        let selected = select_records_inner(
            b"endpoint",
            &include,
            scratch.path(),
            records
                .into_iter()
                .map(|r| (r, CandidateOrigin::Ordinary))
                .collect(),
            Some(&context),
        )
        .unwrap();
        assert_eq!(selected.by_owner.len(), 1);
        assert_eq!(
            selected.by_owner[&("u".into(), "Library".into())]
                .owner
                .module_version
                .0,
            original_version
        );
        // Equal bytes at a newly higher-priority path are a different selection.
        fs::copy(&source, extra.path().join("Library.hs")).unwrap();
        assert!(ordinary_records(b"endpoint", &include, true)
            .unwrap()
            .is_empty());
    }

    #[test]
    #[serial_test::serial]
    fn discovery_requires_matching_header_payload_and_ignores_old_format() {
        let root = tempfile::tempdir().unwrap();
        let record = candidate_fixture(root.path(), "Library");
        unsafe {
            std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", root.path());
        }
        let dir = fixture_record_dir(root.path());
        for entry in fs::read_dir(&dir).unwrap() {
            fs::remove_file(entry.unwrap().path()).unwrap();
        }
        let mut framed = encode_record(&record).unwrap();
        *framed.last_mut().unwrap() ^= 1;
        fs::write(dir.join("tampered.cbor"), framed).unwrap();
        assert!(ordinary_records(b"endpoint", &[root.path().into()], true)
            .unwrap()
            .is_empty());
        fs::remove_file(dir.join("tampered.cbor")).unwrap();
        let mut legacy = Vec::new();
        ciborium::ser::into_writer(&record, &mut legacy).unwrap();
        fs::write(dir.join("legacy.cbor"), legacy).unwrap();
        assert!(ordinary_records(b"endpoint", &[root.path().into()], true)
            .unwrap()
            .is_empty());
        let payload = shared_evidence::encode_record(&record).unwrap();
        let mut header = RecordHeader::for_record(&record, &payload);
        header.module = "Other".into();
        let header = serde_json::to_vec(&header).unwrap();
        let mut mismatch = RECORD_MAGIC.to_vec();
        mismatch.extend_from_slice(&(header.len() as u32).to_be_bytes());
        mismatch.extend(header);
        mismatch.extend(payload);
        fs::write(dir.join("mismatch.cbor"), mismatch).unwrap();
        assert!(ordinary_records(b"endpoint", &[root.path().into()], true)
            .unwrap()
            .is_empty());
    }

    #[test]
    #[serial_test::serial]
    fn discovery_ignores_inactive_roots_and_flat_layout() {
        let cache = tempfile::tempdir().unwrap();
        let sources = tempfile::tempdir().unwrap();
        let mut record = candidate_fixture(sources.path(), "Library");
        record.include = vec![absolute(sources.path()).unwrap()];
        store_fixture(cache.path(), &record, "current");
        unsafe {
            std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", cache.path());
        }
        let producer = record_dir(b"endpoint");
        for n in 0..600 {
            let shard = root_shard(&producer, &cache.path().join(format!("historical-{n}")));
            fs::create_dir_all(&shard).unwrap();
            fs::write(shard.join("historical.cbor"), b"not decoded").unwrap();
            // Flat V7 and flat V8 records must not participate in discovery.
            fs::write(producer.join(format!("flat-{n}.cbor")), b"not decoded").unwrap();
        }
        let legacy = cache
            .path()
            .join("module-candidates-v7")
            .join(sha(b"endpoint"));
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("legacy.cbor"), encode_record(&record).unwrap()).unwrap();
        let include = vec![sources.path().into(), sources.path().join(".")];
        assert_eq!(
            ordinary_records(b"endpoint", &include, true).unwrap().len(),
            1
        );
    }

    #[test]
    #[serial_test::serial]
    fn active_history_keeps_current_candidate_and_source_refusals() {
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let current = candidate_fixture(root.path(), "Library");
        let historical = candidate_fixture(root.path(), "Historical");
        let historical_bytes = encode_record(&historical).unwrap();
        let dir = fixture_record_dir(root.path());
        // Historical advice retains its original authenticated producer and
        // product. Its source no longer exists in the current selection.
        fs::remove_file(&historical.source).unwrap();
        for n in 0..600 {
            fs::write(dir.join(format!("historical-{n}.cbor")), &historical_bytes).unwrap();
        }
        for n in 0..32 {
            fs::write(dir.join(format!("malformed-{n}.cbor")), b"not decoded").unwrap();
        }
        assert!(fs::read_dir(&dir).unwrap().count() > 512);
        let selected = select_in(root.path(), scratch.path()).unwrap();
        assert_eq!(selected.by_owner.len(), 1);
        let offered = &selected.by_owner[&("u".into(), "Library".into())];
        assert_eq!(offered.owner, computed_owner(&current));
        assert_eq!(offered.product.bytes(), current.products);
        assert_eq!(fs::read(&offered.iface_path).unwrap(), current.interface);

        let shadow = tempfile::tempdir().unwrap();
        fs::write(shadow.path().join("Library.hs"), "module Library where\n").unwrap();
        assert!(ordinary_records(
            b"endpoint",
            &[shadow.path().into(), root.path().into()],
            true,
        )
        .unwrap()
        .is_empty());
        fs::write(&current.source, "module Library where\nchanged = True\n").unwrap();
        assert!(select_in(root.path(), scratch.path())
            .unwrap()
            .by_owner
            .is_empty());
    }

    #[test]
    fn selected_root_requires_original_first_selection_and_unambiguous_source_convention() {
        let root = tempfile::tempdir().unwrap();
        let earlier = tempfile::tempdir().unwrap();
        let mut record = candidate_fixture(root.path(), "Library");
        record.include.insert(0, absolute(earlier.path()).unwrap());
        assert_eq!(selected_record_root(&record), absolute(root.path()));
        fs::copy(&record.source, earlier.path().join("Library.hs")).unwrap();
        assert!(selected_record_root(&record).is_none());
        fs::remove_file(earlier.path().join("Library.hs")).unwrap();
        fs::write(root.path().join("Library.lhs"), b"literate shadow").unwrap();
        assert!(selected_record_root(&record).is_none());
    }

    #[test]
    fn exact_package_edges_require_authenticated_unique_direct_roots() {
        use crate::cache::{ImportQualifier, ModuleImportEvidence, ResolutionEvidence};
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let mut record = candidate_fixture(root.path(), "Library");
        let package = root.path().join("List.hi");
        fs::write(&package, b"package-interface").unwrap();
        let mut sidecar: Value =
            ciborium::de::from_reader(record.package_imports.as_slice()).unwrap();
        sidecar.as_array_mut().unwrap()[3] = Value::Array(vec![Value::Array(vec![
            Value::Text("base".into()),
            Value::Text("Data.List".into()),
            Value::Text(package.to_str().unwrap().into()),
            Value::Text(sha(b"package-interface")),
        ])]);
        record.package_imports.clear();
        ciborium::ser::into_writer(&sidecar, &mut record.package_imports).unwrap();
        record.evidence.make_mut().modules[0]
            .imports
            .push(ModuleImportEvidence {
                qualifier: ImportQualifier::Unqualified,
                module: "Data.List".into(),
                boot: false,
                selected: None,
            });
        record
            .evidence
            .make_mut()
            .resolutions
            .push(ResolutionEvidence {
                qualifier: ImportQualifier::Unqualified,
                module: "Data.List".into(),
                boot: false,
                selected: None,
                candidates: vec![root.path().join("Data/List.hs")],
            });
        record.original_owner = OriginalOwner::from_owner(&computed_owner(&record));
        let empty = ExactCandidateContext::new(BTreeSet::new(), BTreeSet::new());
        assert_eq!(
            select_context_records(scratch.path(), vec![record.clone()], &empty)
                .by_owner
                .len(),
            1
        );
        record.package_imports = package_imports("u", "Library", &[0x42]);
        assert!(select_context_records(scratch.path(), vec![record], &empty)
            .by_owner
            .is_empty());
        let rows = sidecar.as_array_mut().unwrap()[3].as_array_mut().unwrap();
        let mut other = rows[0].clone();
        other.as_array_mut().unwrap()[0] = Value::Text("other".into());
        rows.push(other);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&sidecar, &mut bytes).unwrap();
        let roots = crate::recovery_artifacts::validate_package_import_evidence_with_validation(
            &bytes,
            "u",
            "Library",
            &Sha256::digest([0x42]).into(),
            Path::new("packages"),
            &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
        )
        .unwrap();
        let mut edge = ModuleImportEvidence {
            qualifier: ImportQualifier::Unqualified,
            module: "Data.List".into(),
            boot: false,
            selected: None,
        };
        assert!(!package_edge_matches(&edge, &roots));
        edge.qualifier = ImportQualifier::OtherUnit("base".into());
        assert!(package_edge_matches(&edge, &roots));
        edge.qualifier = ImportQualifier::ThisUnit("base".into());
        assert!(!package_edge_matches(&edge, &roots));
        sidecar.as_array_mut().unwrap()[4] = Value::Array(vec![Value::Array(vec![
            Value::Text("primitive".into()),
            Value::Text("ghc-prim".into()),
            Value::Text("GHC.Prim".into()),
        ])]);
        bytes.clear();
        ciborium::ser::into_writer(&sidecar, &mut bytes).unwrap();
        let roots = crate::recovery_artifacts::validate_package_import_evidence_with_validation(
            &bytes,
            "u",
            "Library",
            &Sha256::digest([0x42]).into(),
            Path::new("packages"),
            &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
        )
        .unwrap();
        edge.module = "GHC.Prim".into();
        edge.qualifier = ImportQualifier::OtherUnit("ghc-prim".into());
        assert!(package_edge_matches(&edge, &roots));
        edge.qualifier = ImportQualifier::OtherUnit("other".into());
        assert!(!package_edge_matches(&edge, &roots));
    }

    #[test]
    fn generation_bound_products_are_not_ordinary_source_products() {
        use tidepool_repr::execution_schema::{testing, GlobalDecl};
        let mut wire = testing::wire_program();
        wire.globals.push(GlobalDecl {
            identity: testing::identity("Val", "retained"),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: Some(7),
        });
        let group = testing::projected_group(wire, 0).unwrap();
        let product = RawModuleProduct {
            unit: "fixture".into(),
            module: "Fixture".into(),
            interface: vec![0x42],
            groups: vec![group],
        };
        assert!(generation_dependent(&product));
    }

    #[test]
    fn durable_old_record_and_unrelated_product_rows_are_not_offered() {
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let mut record = candidate_fixture(root.path(), "A");
        record.version = 5;
        let empty = ExactCandidateContext::new(BTreeSet::new(), BTreeSet::new());
        assert!(
            select_context_records(scratch.path(), vec![record.clone()], &empty)
                .by_owner
                .is_empty()
        );
        record.version = 6;
        let mut value: Value = ciborium::de::from_reader(record.products.as_slice()).unwrap();
        let extra: Value =
            ciborium::de::from_reader(product_bytes("u", "Hidden", &[0x42]).as_slice()).unwrap();
        if let Value::Array(fields) = &mut value {
            if let Value::Array(rows) = &mut fields[2] {
                rows.push(extra.as_array().unwrap()[2].as_array().unwrap()[0].clone());
            }
        }
        record.products.clear();
        ciborium::ser::into_writer(&value, &mut record.products).unwrap();
        assert!(select_context_records(scratch.path(), vec![record], &empty)
            .by_owner
            .is_empty());
    }

    fn select_in(root: &Path, scratch: &Path) -> Option<CandidateSet> {
        // The cache path is process-global; serialize this module's tests.
        unsafe {
            std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", root);
        }
        select(b"endpoint", &[root.into()], scratch)
    }

    #[test]
    #[serial_test::serial]
    fn cold_store_still_offers_empty_manifest_for_product_emission() {
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let selected = select_in(root.path(), scratch.path()).unwrap();
        assert!(selected.by_owner.is_empty());
        let manifest = fs::read(&selected.manifest_path).unwrap();
        let Value::Array(fields) =
            ciborium::de::from_reader::<Value, _>(manifest.as_slice()).unwrap()
        else {
            panic!("candidate manifest envelope")
        };
        assert_eq!(fields[0].as_text(), Some("TPMCAN"));
        assert_eq!(fields[1].as_text(), Some("10"));
        assert_eq!(fields.len(), 7);
        assert_eq!(fields[2], Value::Array(vec![]));
        assert_eq!(fields[3], Value::Array(vec![]));
        assert_eq!(fields[4], Value::Array(vec![]));
        assert_eq!(
            fields[5],
            Value::Array(vec![Value::Array(vec![]), Value::Array(vec![])])
        );
    }

    #[test]
    #[serial_test::serial]
    fn source_boot_witness_survives_selection_and_invalidates_ordinary_product() {
        use crate::cache::{ImportQualifier, ModuleImportEvidence, ResolutionEvidence};
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let source = root.path().join("CacheOdd.hs");
        let boot = root.path().join("CacheEven.hs-boot");
        fs::write(
            &source,
            "module CacheOdd where\nimport {-# SOURCE #-} CacheEven\n",
        )
        .unwrap();
        let boot_bytes = b"module CacheEven where\neven' :: Int -> Bool\n";
        fs::write(&boot, boot_bytes).unwrap();
        write_record(
            root.path(),
            &source,
            "u",
            "CacheOdd",
            &[0x42],
            product_bytes("u", "CacheOdd", &[0x42]),
        );
        let record_path = fs::read_dir(fixture_record_dir(root.path()))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut record = read_record_path(&record_path).unwrap();
        let boot = fs::canonicalize(boot).unwrap();
        record.evidence.make_mut().sources.push(SourceEvidence {
            path: boot.clone(),
            sha256: digest(boot_bytes),
        });
        record.evidence.make_mut().modules[0]
            .imports
            .push(ModuleImportEvidence {
                qualifier: ImportQualifier::Unqualified,
                module: "CacheEven".into(),
                boot: true,
                selected: Some(boot.clone()),
            });
        record.evidence.make_mut().modules.push(ModuleEvidence {
            unit: "u".into(),
            module: "CacheEven".into(),
            boot: true,
            source: boot.clone(),
            imports: vec![],
            product: ProductAvailability::Boot,
        });
        record
            .evidence
            .make_mut()
            .resolutions
            .push(ResolutionEvidence {
                qualifier: ImportQualifier::Unqualified,
                module: "CacheEven".into(),
                boot: true,
                selected: Some(boot.clone()),
                candidates: vec![boot.clone()],
            });
        assert!(record.evidence.valid(&record.target_source));
        shared_evidence::publish(
            record_path.parent().unwrap().parent().unwrap(),
            &record.evidence,
        )
        .unwrap();
        fs::write(&record_path, encode_record(&record).unwrap()).unwrap();
        let exact_record = read_record_path(&record_path).unwrap();
        let empty = ExactCandidateContext::new(BTreeSet::new(), BTreeSet::new());
        assert!(
            select_context_records(scratch.path(), vec![exact_record], &empty)
                .by_owner
                .is_empty()
        );
        let selected = select_in(root.path(), scratch.path()).unwrap();
        assert_eq!(selected.by_owner.len(), 1);
        let manifest: Value =
            ciborium::de::from_reader(fs::read(&selected.manifest_path).unwrap().as_slice())
                .unwrap();
        let fields = manifest.as_array().unwrap();
        let candidate = fields[4].as_array().unwrap()[0].as_array().unwrap();
        let imported = candidate[9].as_array().unwrap()[0].as_array().unwrap();
        assert_eq!(imported[1].as_text(), Some("CacheEven"));
        assert_eq!(imported[2], Value::Bool(true));
        assert_eq!(imported[3].as_text(), boot.to_str());
        // The defining ordinary source is unchanged. Its consumed boot input
        // must still invalidate the durable product before compiler admission.
        fs::write(&boot, b"module CacheEven where\neven' :: Bool -> Bool\n").unwrap();
        assert!(select_in(root.path(), scratch.path())
            .unwrap()
            .by_owner
            .is_empty());
        fs::write(&boot, boot_bytes).unwrap();
        assert_eq!(
            select_in(root.path(), scratch.path())
                .unwrap()
                .by_owner
                .len(),
            1
        );
        fs::remove_file(&boot).unwrap();
        assert!(select_in(root.path(), scratch.path())
            .unwrap()
            .by_owner
            .is_empty());
    }

    #[test]
    #[serial_test::serial]
    fn selection_rejects_changed_source_and_corrupt_product_pair() {
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let source = root.path().join("Library.hs");
        fs::write(&source, "module Library where").unwrap();
        write_record(
            root.path(),
            &source,
            "u",
            "Library",
            &[0x42],
            product_bytes("u", "Library", &[0x42]),
        );
        fs::write(&source, "module Library where\nchanged").unwrap();
        assert!(select_in(root.path(), scratch.path())
            .unwrap()
            .by_owner
            .is_empty());

        fs::write(&source, "module Library where").unwrap();
        let dir = fixture_record_dir(root.path());
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            fs::remove_file(path).unwrap();
        }
        write_record(
            root.path(),
            &source,
            "u",
            "Library",
            &[0x42],
            b"broken cbor".to_vec(),
        );
        assert!(select_in(root.path(), scratch.path())
            .unwrap()
            .by_owner
            .is_empty());
    }

    #[test]
    #[serial_test::serial]
    fn selection_rejects_mispaired_package_import_witness() {
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let source = root.path().join("Library.hs");
        fs::write(&source, "module Library where").unwrap();
        write_record(
            root.path(),
            &source,
            "u",
            "Library",
            &[0x42],
            product_bytes("u", "Library", &[0x42]),
        );
        unsafe {
            std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", root.path());
        }
        let dir = fixture_record_dir(root.path());
        let path = fs::read_dir(dir).unwrap().next().unwrap().unwrap().path();
        let mut record: Record = read_record_path(&path).unwrap();
        record.package_imports = package_imports("u", "Other", &[0x42]);
        fs::write(path, encode_record(&record).unwrap()).unwrap();
        assert!(select_in(root.path(), scratch.path())
            .unwrap()
            .by_owner
            .is_empty());
    }

    #[test]
    #[serial_test::serial]
    fn publish_and_select_accept_canonical_equivalent_source_paths() {
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("nested")).unwrap();
        let source = root.path().join("Library.hs");
        fs::write(&source, "module Library where").unwrap();
        write_record(
            root.path(),
            &source,
            "u",
            "Library",
            &[0x42],
            product_bytes("u", "Library", &[0x42]),
        );
        let dir = fixture_record_dir(root.path());
        let path = fs::read_dir(&dir).unwrap().next().unwrap().unwrap().path();
        let mut record: Record = read_record_path(&path).unwrap();
        fs::remove_file(path).unwrap();
        let equivalent = root.path().join("nested/../Library.hs");
        record.evidence.make_mut().sources[1].path = equivalent.clone();
        record.evidence.make_mut().modules[0].source = equivalent;
        let parsed = crate::certified_products::ParsedModuleProducts::decode(
            &record.products,
            &package_bundle("u", "Library", &[0x42]),
        )
        .unwrap();
        unsafe {
            std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", root.path());
        }
        let include = [root.path().into()];
        let (_, publication) = prepare_publication(
            b"endpoint",
            &include,
            &record.evidence,
            parsed,
            &record.target_source,
            CandidateVersionOrigin::Ordinary,
            &[],
        );
        publish_prepared(publication);
        let selected = select_in(root.path(), scratch.path()).unwrap();
        assert!(selected
            .by_owner
            .contains_key(&("u".into(), "Library".into())));
    }

    #[test]
    #[serial_test::serial]
    fn discovery_selects_one_intact_duplicate_record_per_owner() {
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let source = root.path().join("Library.hs");
        fs::write(&source, "module Library where").unwrap();
        write_record(
            root.path(),
            &source,
            "u",
            "Library",
            &[0x42],
            product_bytes("u", "Library", &[0x42]),
        );
        let dir = fixture_record_dir(root.path());
        let original = fs::read_dir(&dir).unwrap().next().unwrap().unwrap().path();
        fs::copy(original, dir.join("duplicate.cbor")).unwrap();
        assert_eq!(
            select_in(root.path(), scratch.path())
                .unwrap()
                .by_owner
                .len(),
            1
        );
    }

    #[test]
    #[serial_test::serial]
    fn selection_refuses_manifest_over_four_mebibytes() {
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let source = root.path().join("Library.hs");
        fs::write(&source, "module Library where").unwrap();
        let record = candidate_fixture(root.path(), "Library");
        let records = (0..12)
            .map(|n| {
                let mut record = record.clone();
                record.module = format!("{}{}", n, "M".repeat(390_000));
                record.evidence.make_mut().modules[0].module = record.module.clone();
                record.products = product_bytes("u", &record.module, &[0x42]);
                record.package_imports = package_imports("u", &record.module, &[0x42]);
                record.original_owner = OriginalOwner::from_owner(&computed_owner(&record));
                (record, CandidateOrigin::Ordinary)
            })
            .collect();
        assert!(
            select_records(b"endpoint", &[root.path().into()], scratch.path(), records).is_none()
        );
    }

    const QUERY_OWNER_COUNT: usize = 4;

    #[derive(Clone, Debug)]
    enum CatalogOp {
        Offer(u8, u8),
        Remove(u8),
        Query(u8),
    }

    fn catalog_operation() -> impl Strategy<Value = CatalogOp> {
        prop_oneof![
            3 => (0u8..QUERY_OWNER_COUNT as u8, 0u8..2)
                .prop_map(|(owner, revision)| CatalogOp::Offer(owner, revision)),
            2 => (0u8..QUERY_OWNER_COUNT as u8).prop_map(CatalogOp::Remove),
            3 => (0u8..(1 << QUERY_OWNER_COUNT)).prop_map(CatalogOp::Query),
        ]
    }

    fn catalog_history() -> impl Strategy<Value = Vec<CatalogOp>> {
        prop_oneof![
            prop::collection::vec(catalog_operation(), 1..25),
            targeted_catalog_history(),
        ]
    }

    fn targeted_catalog_history() -> impl Strategy<Value = Vec<CatalogOp>> {
        (
            prop::collection::vec(catalog_operation(), 0..5),
            prop::collection::vec(catalog_operation(), 0..5),
        )
            .prop_map(|(mut prefix, suffix)| {
                prefix.extend([
                    CatalogOp::Offer(0, 0),
                    CatalogOp::Query(0b0001), // warm the same root before replacement
                    CatalogOp::Offer(0, 1),   // same path and size, new source identity
                    CatalogOp::Query(0b0001),
                    CatalogOp::Offer(0, 1), // identical reoffer
                    CatalogOp::Remove(0),
                    CatalogOp::Query(0b0001), // deleted source leaves no query result
                    CatalogOp::Offer(0, 0),   // recreate and publish the original version
                    CatalogOp::Query(0b0001),
                    CatalogOp::Offer(2, 0),
                    CatalogOp::Query(0b0101), // sparse roots return a sparse owner set
                    CatalogOp::Remove(1),
                    CatalogOp::Query(0b0010), // absent root remains empty
                    CatalogOp::Offer(1, 0),
                    CatalogOp::Query(0b0011),
                ]);
                prefix.extend(suffix);
                prefix
            })
    }

    fn query_source(module: &str, revision: u8) -> Vec<u8> {
        format!("module {module} where\n-- revision {revision:02}\n").into_bytes()
    }

    fn query_fixture_record(root: &Path, module: &str, revision: u8) -> Record {
        let source = root.join(format!("{module}.hs"));
        fs::write(&source, query_source(module, revision)).unwrap();
        let interface = format!("u:{module}").into_bytes();
        let module_interface = write_record(
            root,
            &source,
            "u",
            module,
            &interface,
            product_bytes("u", module, &interface),
        );
        let mut record = fs::read_dir(fixture_record_dir(root))
            .unwrap()
            .map(|entry| read_record_path(&entry.unwrap().path()).unwrap())
            .find(|record| record.module == module)
            .unwrap();
        assert_eq!(record.module_interface.as_ref(), Some(&module_interface));
        record.module_interface_proof = Some(
            crate::recovery_artifacts::recover_module_interface(
                fixture_record_dir(root).parent().unwrap(),
                record.module_interface.as_ref().unwrap(),
                &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
            )
            .unwrap(),
        );
        record
    }

    fn publish_query_fixture(record: &Record) {
        let certified = crate::recovery_artifacts::CertifiedRecoveryProduct::from_certification(
            computed_owner(record),
            record.interface.clone(),
            record.products.clone(),
            record.package_imports.clone(),
            record.original_certification.clone(),
        )
        .with_source_sha256(parse_sha(&record.source_sha256).unwrap())
        .with_module_interface(record.module_interface_proof.as_ref().unwrap().clone())
        .unwrap();
        let parsed = crate::certified_products::ParsedModuleProducts::decode(
            &record.products,
            &package_bundle(&record.unit, &record.module, &record.interface),
        )
        .unwrap();
        let (_, prepared) = prepare_publication(
            &record.endpoint,
            &record.include,
            &record.evidence,
            parsed,
            &record.target_source,
            CandidateVersionOrigin::Ordinary,
            &[certified],
        );
        assert_eq!(prepared.records.len(), 1);
        publish_prepared(prepared);
    }

    #[derive(Clone, Copy, Debug, Default)]
    struct CatalogCoverage {
        offers: usize,
        replacements: usize,
        identical_reoffers: usize,
        removes: usize,
        absent_removes: usize,
        reoffers_after_remove: usize,
        queries: usize,
        sparse_queries: usize,
        empty_queries: usize,
        warm_mutations: usize,
    }

    impl CatalogCoverage {
        fn accumulate(&mut self, other: Self) {
            self.offers += other.offers;
            self.replacements += other.replacements;
            self.identical_reoffers += other.identical_reoffers;
            self.removes += other.removes;
            self.absent_removes += other.absent_removes;
            self.reoffers_after_remove += other.reoffers_after_remove;
            self.queries += other.queries;
            self.sparse_queries += other.sparse_queries;
            self.empty_queries += other.empty_queries;
            self.warm_mutations += other.warm_mutations;
        }
    }

    fn catalog_config() -> Config {
        let mut config = Config::default();
        if std::env::var_os("PROPTEST_CASES").is_none() {
            config.cases = 64;
        }
        if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
            config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
        }
        config
    }

    fn query_catalog(
        roots: &[tempfile::TempDir],
        variants: &[Vec<Record>],
        model: &[(usize, u8)],
        mask: u8,
    ) -> Result<(), TestCaseError> {
        // Candidate discovery is advisory. The fixture pool is valid, unique,
        // and below the offer budgets; final product admission is outside this
        // query oracle.
        let include = roots
            .iter()
            .enumerate()
            .filter(|(owner, _)| mask & (1u8 << *owner) != 0)
            .map(|(_, root)| root.path().to_path_buf())
            .collect::<Vec<_>>();
        let offer = ordinary_records_with_limits(
            b"endpoint",
            &include,
            true,
            CacheOfferLimits {
                owners: CANDIDATE_LIMIT,
                payload_bytes: PAYLOAD_LIMIT as u64,
            },
        )
        .expect("active temporary root shards are readable");
        prop_assert_eq!(offer.diagnostics.total(), 0);

        let mut expected = model
            .iter()
            .filter(|(owner, _)| mask & (1u8 << *owner) != 0)
            .map(|(owner, revision)| &variants[*owner][*revision as usize])
            .collect::<Vec<_>>();
        expected
            .sort_by(|left, right| (&left.unit, &left.module).cmp(&(&right.unit, &right.module)));
        let mut actual = offer.records.iter().collect::<Vec<_>>();
        actual.sort_by(|left, right| (&left.unit, &left.module).cmp(&(&right.unit, &right.module)));
        prop_assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.into_iter().zip(expected) {
            prop_assert_eq!(
                (&actual.unit, &actual.module),
                (&expected.unit, &expected.module)
            );
            prop_assert_eq!(&actual.source, &expected.source);
            prop_assert_eq!(&actual.source_sha256, &expected.source_sha256);
            prop_assert_eq!(&actual.interface, &expected.interface);
            prop_assert_eq!(&actual.products, &expected.products);
        }
        Ok(())
    }

    fn catalog_history_coverage(ops: &[CatalogOp]) -> CatalogCoverage {
        let mut model = Vec::<(usize, u8)>::new();
        let mut ever_offered = [false; QUERY_OWNER_COUNT];
        let mut covered = CatalogCoverage::default();
        let mut previous_was_query = false;
        for op in ops {
            match *op {
                CatalogOp::Offer(owner, revision) => {
                    let owner = owner as usize;
                    if previous_was_query {
                        covered.warm_mutations += 1;
                    }
                    match model.iter_mut().find(|(existing, _)| *existing == owner) {
                        Some((_, current)) if *current == revision => {
                            covered.identical_reoffers += 1;
                        }
                        Some((_, current)) => {
                            covered.replacements += 1;
                            *current = revision;
                        }
                        None => {
                            if ever_offered[owner] {
                                covered.reoffers_after_remove += 1;
                            }
                            model.push((owner, revision));
                        }
                    }
                    ever_offered[owner] = true;
                    covered.offers += 1;
                    previous_was_query = false;
                }
                CatalogOp::Remove(owner) => {
                    let owner = owner as usize;
                    if previous_was_query {
                        covered.warm_mutations += 1;
                    }
                    if let Some(position) =
                        model.iter().position(|(existing, _)| *existing == owner)
                    {
                        model.remove(position);
                        covered.removes += 1;
                    } else {
                        covered.absent_removes += 1;
                    }
                    previous_was_query = false;
                }
                CatalogOp::Query(mask) => {
                    if mask.count_ones() < QUERY_OWNER_COUNT as u32 {
                        covered.sparse_queries += 1;
                    }
                    if model.iter().all(|(owner, _)| mask & (1u8 << *owner) == 0) {
                        covered.empty_queries += 1;
                    }
                    covered.queries += 1;
                    previous_was_query = true;
                }
            }
        }
        covered
    }

    fn replay_catalog_history(
        cache: &Path,
        roots: &[tempfile::TempDir],
        variants: &[Vec<Record>],
        ops: &[CatalogOp],
    ) -> Result<CatalogCoverage, TestCaseError> {
        fs::remove_dir_all(cache).unwrap();
        fs::create_dir_all(cache).unwrap();
        for (owner, root) in roots.iter().enumerate() {
            let source = root.path().join(format!("CacheOwner{owner}.hs"));
            if source.exists() {
                fs::remove_file(source).unwrap();
            }
        }

        let mut model = Vec::<(usize, u8)>::new();
        let mut ever_offered = [false; QUERY_OWNER_COUNT];
        let mut covered = CatalogCoverage::default();
        let mut previous_was_query = false;
        for op in ops {
            let observation_mask = match *op {
                CatalogOp::Offer(owner, revision) => {
                    let owner = owner as usize;
                    let revision = revision as usize;
                    if previous_was_query {
                        covered.warm_mutations += 1;
                    }
                    match model.iter_mut().find(|(existing, _)| *existing == owner) {
                        Some((_, current)) if *current as usize == revision => {
                            covered.identical_reoffers += 1;
                        }
                        Some((_, current)) => {
                            covered.replacements += 1;
                            *current = revision as u8;
                        }
                        None => {
                            if ever_offered[owner] {
                                covered.reoffers_after_remove += 1;
                            }
                            model.push((owner, revision as u8));
                        }
                    }
                    let record = &variants[owner][revision];
                    fs::write(&record.source, query_source(&record.module, revision as u8))
                        .unwrap();
                    publish_query_fixture(record);
                    ever_offered[owner] = true;
                    covered.offers += 1;
                    1 << owner
                }
                CatalogOp::Remove(owner) => {
                    let owner = owner as usize;
                    if previous_was_query {
                        covered.warm_mutations += 1;
                    }
                    let position = model.iter().position(|(existing, _)| *existing == owner);
                    if let Some(position) = position {
                        model.remove(position);
                        covered.removes += 1;
                    } else {
                        covered.absent_removes += 1;
                    }
                    let source = roots[owner].path().join(format!("CacheOwner{owner}.hs"));
                    if source.exists() {
                        fs::remove_file(source).unwrap();
                    }
                    1 << owner
                }
                CatalogOp::Query(mask) => {
                    if mask.count_ones() < QUERY_OWNER_COUNT as u32 {
                        covered.sparse_queries += 1;
                    }
                    if model.iter().all(|(owner, _)| mask & (1u8 << *owner) == 0) {
                        covered.empty_queries += 1;
                    }
                    mask
                }
            };
            query_catalog(roots, variants, &model, observation_mask)?;
            if matches!(op, CatalogOp::Query(_)) {
                covered.queries += 1;
                previous_was_query = true;
            } else {
                previous_was_query = false;
            }
        }
        Ok(covered)
    }

    #[test]
    #[serial_test::serial]
    fn generated_ordinary_catalog_queries_match_a_vec_full_scan() {
        struct RestoreCache(Option<std::ffi::OsString>);
        impl Drop for RestoreCache {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(value) => unsafe {
                        std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", value)
                    },
                    None => unsafe { std::env::remove_var("TIDEPOOL_COMPILE_CACHE_DIR") },
                }
            }
        }

        let cache = tempfile::tempdir().unwrap();
        let roots = (0..QUERY_OWNER_COUNT)
            .map(|_| tempfile::tempdir().unwrap())
            .collect::<Vec<_>>();
        let _restore = RestoreCache(std::env::var_os("TIDEPOOL_COMPILE_CACHE_DIR"));
        unsafe {
            std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", cache.path());
        }
        let variants = roots
            .iter()
            .enumerate()
            .map(|(owner, root)| {
                let module = format!("CacheOwner{owner}");
                let records = (0..2)
                    .map(|revision| query_fixture_record(root.path(), &module, revision))
                    .collect::<Vec<_>>();
                assert_eq!(records[0].source, records[1].source);
                assert_eq!(
                    fs::metadata(&records[0].source).unwrap().len(),
                    query_source(&module, 0).len() as u64
                );
                assert_eq!(
                    query_source(&module, 0).len(),
                    query_source(&module, 1).len(),
                    "same-key source revisions keep the source size"
                );
                assert_ne!(records[0].source_sha256, records[1].source_sha256);
                records
            })
            .collect::<Vec<_>>();

        // Census only the deterministic history shape here; every production
        // replay stays inside the configured runner so it can shrink and persist.
        let mut support_runner = TestRunner::deterministic();
        let mut support = CatalogCoverage::default();
        for _ in 0..8 {
            let tree = targeted_catalog_history()
                .new_tree(&mut support_runner)
                .unwrap();
            support.accumulate(catalog_history_coverage(&tree.current()));
        }
        assert!(support.replacements >= 8);
        assert!(support.identical_reoffers >= 8);
        assert!(support.removes >= 8);
        assert!(support.reoffers_after_remove >= 8);
        assert!(support.queries >= 32);
        assert!(support.sparse_queries >= 16);
        assert!(support.empty_queries >= 8);
        assert!(support.warm_mutations >= 16);

        let mut config = proptest::test_runner::contextualize_config(catalog_config());
        config.source_file = Some(file!());
        config.test_name = Some(concat!(
            module_path!(),
            "::generated_ordinary_catalog_queries_match_a_vec_full_scan"
        ));
        let mut runner = TestRunner::new(config);
        let observed = std::cell::RefCell::new(CatalogCoverage::default());
        let result = runner.run(&catalog_history(), |ops| {
            observed.borrow_mut().accumulate(replay_catalog_history(
                cache.path(),
                &roots,
                &variants,
                &ops,
            )?);
            Ok(())
        });
        eprintln!(
            "ordinary candidate catalog observations: mixed={:?}, targeted_shape={support:?}",
            *observed.borrow()
        );
        if let Err(error) = result {
            panic!("ordinary candidate catalog property failed: {error}");
        }
    }
}
