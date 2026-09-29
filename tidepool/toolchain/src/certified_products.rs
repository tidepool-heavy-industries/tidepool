//! Final Rust admission of module products checked by the resident compiler.
//! Worker receipts describe GHC's selected owners; stored product bytes and
//! dependency witnesses remain independent inputs to this check.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ciborium::value::Value;
use sha2::{Digest, Sha256};
use tidepool_repr::execution_schema::{
    CachedHomeOwner, DecodeLimits, GlobalDecl, ModuleVersion, PreparedProgram, ProjectedGroup,
    RawModuleProduct, ResultContract, RuntimeRep, Signature, SymbolIdentity, parse_module_products,
};

use crate::cache::{DependencyEvidence, ProductAvailability};
use crate::module_candidates::CandidateSet;

const RECEIPT_LIMIT: usize = 4 << 20;
const MODULE_LIMIT: usize = 128;
// Tidepool.Effects.Core alone produces 5,930 neutral groups in a normal
// resident turn. The separate 4 MiB receipt bound limits aggregate memory.
const GROUP_LIMIT: usize = 8192;
const GLOBAL_LIMIT: usize = 65536;
const PACKAGE_LIMIT: usize = 4096;
const PACKAGE_INTERFACE_LIMIT: u64 = 32 << 20;
const SOURCE_LIMIT: u64 = 32 << 20;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProductOrigin {
    Fresh,
    Cached,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PendingImportOwner {
    Source {
        owner: CachedHomeOwner,
        original_ordinal: u32,
        binder: SymbolIdentity,
    },
    Retained {
        identity: SymbolIdentity,
        generation: u64,
    },
    Package {
        unit: String,
        module: String,
        binder: SymbolIdentity,
        interface_digest: [u8; 32],
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReceiptImportOwner {
    Source {
        unit: String,
        module: String,
        module_version: Option<ModuleVersion>,
        original_ordinal: u32,
        binder: SymbolIdentity,
    },
    Retained {
        identity: SymbolIdentity,
        generation: u64,
    },
    Package {
        unit: String,
        module: String,
        binder: SymbolIdentity,
        interface_digest: [u8; 32],
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedGlobal {
    pub identity: SymbolIdentity,
    pub rep: RuntimeRep,
    pub entry_signature: Option<Signature>,
    pub required_evaluated: bool,
    pub owner: ReceiptImportOwner,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedGroup {
    pub original_ordinal: u32,
    pub globals: Vec<AcceptedGlobal>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedModuleReceipt {
    pub origin: ProductOrigin,
    pub unit: String,
    pub module: String,
    pub module_version: Option<ModuleVersion>,
    pub skinny_iface_sha256: [u8; 32],
    pub product_sha256: [u8; 32],
    pub source_sha256: [u8; 32],
    pub dependency_witness_sha256: [u8; 32],
    pub groups: Vec<AcceptedGroup>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedReceipt {
    pub modules: Vec<CertifiedModuleReceipt>,
    pub targets: BTreeMap<String, Vec<AcceptedGlobal>>,
    pub packages: BTreeMap<(String, String), PackageInterfaceWitness>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageInterfaceWitness {
    pub selected_path: PathBuf,
    pub sha256: [u8; 32],
}

/// A group whose retained globals still need the authoritative lexical
/// `SessionVarId` and live handle. Runtime resolves those under checkout,
/// then constructs `CertifiedGroup`; it never infers a retained ID from text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingCertifiedGroup {
    origin: ProductOrigin,
    owner: CachedHomeOwner,
    group: ProjectedGroup,
    imports: Vec<PendingImportOwner>,
}

impl PendingCertifiedGroup {
    pub fn origin(&self) -> ProductOrigin {
        self.origin
    }
    pub fn owner(&self) -> &CachedHomeOwner {
        &self.owner
    }

    pub fn group(&self) -> &ProjectedGroup {
        &self.group
    }

    pub fn imports(&self) -> &[PendingImportOwner] {
        &self.imports
    }

    pub fn into_parts(self) -> (CachedHomeOwner, ProjectedGroup, Vec<PendingImportOwner>) {
        (self.owner, self.group, self.imports)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CertificationError {
    #[error("malformed bounded compiler product receipt: {0}")]
    Receipt(&'static str),
    #[error("compiler product certificate disagrees with {0}")]
    Mismatch(&'static str),
    #[error("compiler product evidence is no longer valid")]
    StaleEvidence,
    #[error("invalid original module product: {0}")]
    Product(#[from] tidepool_repr::execution_schema::ParseError),
}

type CertResult<T> = Result<T, CertificationError>;

fn array(value: &Value) -> CertResult<&[Value]> {
    match value {
        Value::Array(items) => Ok(items),
        _ => Err(CertificationError::Receipt("expected array")),
    }
}

fn sized<'a>(value: &'a Value, size: usize) -> CertResult<&'a [Value]> {
    let items = array(value)?;
    if items.len() != size {
        return Err(CertificationError::Receipt("wrong array arity"));
    }
    Ok(items)
}

fn string(value: &Value) -> CertResult<&str> {
    match value {
        Value::Text(value) => Ok(value),
        _ => Err(CertificationError::Receipt("expected text")),
    }
}

fn number(value: &Value) -> CertResult<u64> {
    match value {
        Value::Integer(value) => u64::try_from(*value)
            .map_err(|_| CertificationError::Receipt("expected unsigned integer")),
        _ => Err(CertificationError::Receipt("expected unsigned integer")),
    }
}

fn boolean(value: &Value) -> CertResult<bool> {
    match value {
        Value::Bool(value) => Ok(*value),
        _ => Err(CertificationError::Receipt("expected bool")),
    }
}

fn digest(value: &Value) -> CertResult<[u8; 32]> {
    let encoded = string(value)?.as_bytes();
    if encoded.len() != 64 {
        return Err(CertificationError::Receipt("digest is not SHA-256 hex"));
    }
    let mut decoded = [0; 32];
    for (index, pair) in encoded.chunks_exact(2).enumerate() {
        let hi = (pair[0] as char)
            .to_digit(16)
            .ok_or(CertificationError::Receipt("invalid SHA-256 hex"))?;
        let lo = (pair[1] as char)
            .to_digit(16)
            .ok_or(CertificationError::Receipt("invalid SHA-256 hex"))?;
        decoded[index] = ((hi << 4) | lo) as u8;
    }
    Ok(decoded)
}

fn optional_version(value: &Value) -> CertResult<Option<ModuleVersion>> {
    match value {
        Value::Null => Ok(None),
        value => Ok(Some(ModuleVersion(digest(value)?))),
    }
}

fn identity(value: &Value) -> CertResult<SymbolIdentity> {
    let row = sized(value, 5)?;
    Ok(SymbolIdentity {
        unit: string(&row[0])?.to_owned(),
        module: string(&row[1])?.to_owned(),
        namespace: string(&row[2])?.to_owned(),
        occurrence: string(&row[3])?.to_owned(),
        record_parent: match &row[4] {
            Value::Null => None,
            value => Some(string(value)?.to_owned()),
        },
    })
}

fn rep(value: &Value) -> CertResult<RuntimeRep> {
    let row = sized(value, 2)?;
    let bits = u8::try_from(number(&row[1])?)
        .map_err(|_| CertificationError::Receipt("representation width"))?;
    let rep = match (string(&row[0])?, bits) {
        ("void", 0) => RuntimeRep::Void,
        ("lifted", 0) => RuntimeRep::LiftedRef,
        ("unlifted", 0) => RuntimeRep::UnliftedRef,
        ("address", 0) => RuntimeRep::Address,
        ("int", bits) => RuntimeRep::Int(bits),
        ("word", bits) => RuntimeRep::Word(bits),
        ("float", bits) => RuntimeRep::Float(bits),
        _ => return Err(CertificationError::Receipt("representation tag")),
    };
    Ok(rep)
}

fn signature(value: &Value) -> CertResult<Option<Signature>> {
    if matches!(value, Value::Null) {
        return Ok(None);
    }
    let row = sized(value, 2)?;
    let arguments = array(&row[0])?.iter().map(rep).collect::<CertResult<_>>()?;
    let result = sized(&row[1], 2)?;
    let returned = array(&result[1])?;
    let results = match string(&result[0])? {
        "returns" => ResultContract::Returns(returned.iter().map(rep).collect::<CertResult<_>>()?),
        "no_success" if returned.is_empty() => ResultContract::NoSuccess,
        "caller_result" if returned.is_empty() => ResultContract::CallerResult,
        _ => return Err(CertificationError::Receipt("result contract")),
    };
    Ok(Some(Signature { arguments, results }))
}

fn owner(value: &Value) -> CertResult<ReceiptImportOwner> {
    let row = array(value)?;
    let Some(tag) = row.first() else {
        return Err(CertificationError::Receipt("empty import owner"));
    };
    match string(tag)? {
        "source" => {
            if row.len() != 6 {
                return Err(CertificationError::Receipt("source owner arity"));
            }
            Ok(ReceiptImportOwner::Source {
                unit: string(&row[1])?.to_owned(),
                module: string(&row[2])?.to_owned(),
                module_version: optional_version(&row[3])?,
                original_ordinal: u32::try_from(number(&row[4])?)
                    .map_err(|_| CertificationError::Receipt("source group ordinal"))?,
                binder: identity(&row[5])?,
            })
        }
        "retained" => {
            if row.len() != 3 {
                return Err(CertificationError::Receipt("retained owner arity"));
            }
            Ok(ReceiptImportOwner::Retained {
                identity: identity(&row[1])?,
                generation: number(&row[2])?,
            })
        }
        "package" => {
            if row.len() != 5 {
                return Err(CertificationError::Receipt("package owner arity"));
            }
            Ok(ReceiptImportOwner::Package {
                unit: string(&row[1])?.to_owned(),
                module: string(&row[2])?.to_owned(),
                interface_digest: digest(&row[3])?,
                binder: identity(&row[4])?,
            })
        }
        _ => Err(CertificationError::Receipt("import owner tag")),
    }
}

fn accepted_global(value: &Value) -> CertResult<AcceptedGlobal> {
    let row = sized(value, 5)?;
    Ok(AcceptedGlobal {
        identity: identity(&row[0])?,
        rep: rep(&row[1])?,
        entry_signature: signature(&row[2])?,
        required_evaluated: boolean(&row[3])?,
        owner: owner(&row[4])?,
    })
}

fn validate_global_witness(
    declaration: &GlobalDecl,
    signatures: &[Signature],
    selected: &AcceptedGlobal,
) -> CertResult<ReceiptImportOwner> {
    let actual_signature = declaration
        .entry_signature
        .and_then(|id| signatures.get(id.0 as usize))
        .cloned();
    if declaration.identity != selected.identity
        || declaration.rep != selected.rep
        || declaration.required_evaluated != selected.required_evaluated
        || actual_signature != selected.entry_signature
    {
        return Err(CertificationError::Mismatch(
            "global representation/signature",
        ));
    }
    let aligned = match &selected.owner {
        ReceiptImportOwner::Retained {
            identity,
            generation,
        } => {
            identity == &declaration.identity
                && declaration.required_generation == Some(*generation)
        }
        ReceiptImportOwner::Source { binder, .. } => {
            binder == &declaration.identity && declaration.required_generation.is_none()
        }
        ReceiptImportOwner::Package {
            unit,
            module,
            binder,
            ..
        } => {
            binder == &declaration.identity
                && &binder.unit == unit
                && &binder.module == module
                && declaration.required_generation.is_none()
        }
    };
    if !aligned {
        return Err(CertificationError::Mismatch("global owner"));
    }
    Ok(selected.owner.clone())
}

/// Decode the worker's bounded `TPCERT` v2 CBOR tuple. Original module
/// groups and each executable target retain their own ordered global rows.
pub fn decode_receipt(bytes: &[u8]) -> CertResult<CertifiedReceipt> {
    if bytes.len() > RECEIPT_LIMIT {
        return Err(CertificationError::Receipt("receipt size"));
    }
    let value: Value =
        ciborium::de::from_reader(bytes).map_err(|_| CertificationError::Receipt("CBOR"))?;
    let header = sized(&value, 5)?;
    if string(&header[0])? != "TPCERT" || number(&header[1])? != 2 {
        return Err(CertificationError::Receipt("receipt header"));
    }
    let modules = array(&header[2])?;
    if modules.len() > MODULE_LIMIT {
        return Err(CertificationError::Receipt("module count"));
    }
    let modules = modules
        .iter()
        .map(|module| {
            let row = sized(module, 9)?;
            let origin = match string(&row[0])? {
                "fresh" => ProductOrigin::Fresh,
                "cached" => ProductOrigin::Cached,
                _ => return Err(CertificationError::Receipt("product origin")),
            };
            let groups = array(&row[8])?;
            if groups.len() > GROUP_LIMIT {
                return Err(CertificationError::Receipt("group count"));
            }
            let groups = groups
                .iter()
                .map(|group| {
                    let row = sized(group, 2)?;
                    let globals = array(&row[1])?;
                    if globals.len() > GLOBAL_LIMIT {
                        return Err(CertificationError::Receipt("global count"));
                    }
                    Ok(AcceptedGroup {
                        original_ordinal: u32::try_from(number(&row[0])?)
                            .map_err(|_| CertificationError::Receipt("group ordinal"))?,
                        globals: globals
                            .iter()
                            .map(accepted_global)
                            .collect::<CertResult<_>>()?,
                    })
                })
                .collect::<CertResult<_>>()?;
            Ok(CertifiedModuleReceipt {
                origin,
                unit: string(&row[1])?.to_owned(),
                module: string(&row[2])?.to_owned(),
                module_version: optional_version(&row[3])?,
                skinny_iface_sha256: digest(&row[5])?,
                product_sha256: digest(&row[6])?,
                source_sha256: digest(&row[4])?,
                dependency_witness_sha256: digest(&row[7])?,
                groups,
            })
        })
        .collect::<CertResult<Vec<_>>>()?;
    let target_rows = array(&header[3])?;
    if target_rows.len() > MODULE_LIMIT {
        return Err(CertificationError::Receipt("target count"));
    }
    let mut targets = BTreeMap::new();
    for target in target_rows {
        let row = sized(target, 2)?;
        let name = string(&row[0])?.to_owned();
        if name.is_empty() {
            return Err(CertificationError::Receipt("empty target name"));
        }
        let globals = array(&row[1])?;
        if globals.len() > GLOBAL_LIMIT {
            return Err(CertificationError::Receipt("target global count"));
        }
        if targets
            .insert(
                name,
                globals
                    .iter()
                    .map(accepted_global)
                    .collect::<CertResult<_>>()?,
            )
            .is_some()
        {
            return Err(CertificationError::Receipt("duplicate target"));
        }
    }
    let package_rows = array(&header[4])?;
    if package_rows.len() > PACKAGE_LIMIT {
        return Err(CertificationError::Receipt("package count"));
    }
    let mut packages = BTreeMap::new();
    for package in package_rows {
        let row = sized(package, 4)?;
        let unit = string(&row[0])?.to_owned();
        let module = string(&row[1])?.to_owned();
        let selected_path = PathBuf::from(string(&row[2])?);
        if unit.is_empty() || module.is_empty() || !selected_path.is_absolute() {
            return Err(CertificationError::Receipt("package witness identity/path"));
        }
        if packages
            .insert(
                (unit, module),
                PackageInterfaceWitness {
                    selected_path,
                    sha256: digest(&row[3])?,
                },
            )
            .is_some()
        {
            return Err(CertificationError::Receipt("duplicate package witness"));
        }
    }
    Ok(CertifiedReceipt {
        modules,
        targets,
        packages,
    })
}

fn sha(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn read_bounded(path: &Path, limit: u64) -> CertResult<Vec<u8>> {
    if !path.is_absolute() {
        return Err(CertificationError::StaleEvidence);
    }
    let metadata = std::fs::metadata(path).map_err(|_| CertificationError::StaleEvidence)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(CertificationError::StaleEvidence);
    }
    let bytes = std::fs::read(path).map_err(|_| CertificationError::StaleEvidence)?;
    if bytes.len() as u64 > limit {
        return Err(CertificationError::StaleEvidence);
    }
    Ok(bytes)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

fn ready_source_sha(
    evidence: &DependencyEvidence,
    unit: &str,
    module: &str,
) -> CertResult<[u8; 32]> {
    let matching: Vec<_> = evidence
        .modules
        .iter()
        .filter(|row| {
            row.unit == unit
                && row.module == module
                && !row.boot
                && row.product == ProductAvailability::Ready
        })
        .collect();
    let [module] = matching.as_slice() else {
        return Err(CertificationError::Mismatch("ready source module"));
    };
    let source = evidence
        .sources
        .iter()
        .find(|source| source.path == module.source)
        .ok_or(CertificationError::Mismatch("source evidence"))?;
    let value = Value::Text(source.sha256.clone());
    digest(&value)
}

fn matching_product<'a>(
    products: &'a [RawModuleProduct],
    unit: &str,
    module: &str,
) -> CertResult<&'a RawModuleProduct> {
    let matching: Vec<_> = products
        .iter()
        .filter(|product| product.unit == unit && product.module == module)
        .collect();
    let [product] = matching.as_slice() else {
        return Err(CertificationError::Mismatch("unique module product"));
    };
    Ok(product)
}

fn fresh_module_version(
    endpoint_identity: &[u8],
    include: &[PathBuf],
    source_sha: &[u8; 32],
    interface: &[u8],
    product_bytes: &[u8],
) -> CertResult<ModuleVersion> {
    let include = crate::module_candidates::context_paths(include)
        .ok_or(CertificationError::Mismatch("include context"))?;
    Ok(crate::module_candidates::module_version_for_product(
        endpoint_identity,
        &include,
        &hex(source_sha),
        interface,
        product_bytes,
    ))
}

type SourceGroupMap =
    BTreeMap<(String, String, u32, SymbolIdentity), (CachedHomeOwner, ProductOrigin)>;

fn resolve_receipt_owner(
    import: ReceiptImportOwner,
    sources: &SourceGroupMap,
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
) -> CertResult<PendingImportOwner> {
    match import {
        ReceiptImportOwner::Source {
            unit,
            module,
            module_version,
            original_ordinal,
            binder,
        } => {
            let (resolved, origin) = sources
                .get(&(unit, module, original_ordinal, binder.clone()))
                .ok_or(CertificationError::Mismatch("source binder/group closure"))?;
            if *origin == ProductOrigin::Cached && module_version.is_none() {
                return Err(CertificationError::Mismatch(
                    "missing cached source version",
                ));
            }
            if module_version
                .as_ref()
                .is_some_and(|version| version != &resolved.module_version)
            {
                return Err(CertificationError::Mismatch("source module version"));
            }
            Ok(PendingImportOwner::Source {
                owner: resolved.clone(),
                original_ordinal,
                binder,
            })
        }
        ReceiptImportOwner::Retained {
            identity,
            generation,
        } => Ok(PendingImportOwner::Retained {
            identity,
            generation,
        }),
        ReceiptImportOwner::Package {
            unit,
            module,
            binder,
            interface_digest,
        } => {
            let witness = packages
                .get(&(unit.clone(), module.clone()))
                .ok_or(CertificationError::Mismatch("package interface witness"))?;
            if witness.sha256 != interface_digest || !witness.selected_path.is_absolute() {
                return Err(CertificationError::Mismatch("package interface witness"));
            }
            let bytes = read_bounded(&witness.selected_path, PACKAGE_INTERFACE_LIMIT)?;
            if sha(&bytes) != interface_digest {
                return Err(CertificationError::StaleEvidence);
            }
            Ok(PendingImportOwner::Package {
                unit,
                module,
                binder,
                interface_digest,
            })
        }
    }
}

fn certified_source_map(groups: &[PendingCertifiedGroup]) -> CertResult<SourceGroupMap> {
    let mut sources = SourceGroupMap::new();
    for group in groups {
        for binder in group.group.binders() {
            let key = (
                group.owner.unit.clone(),
                group.owner.module.clone(),
                group.group.original_ordinal(),
                binder.clone(),
            );
            if sources
                .insert(key, (group.owner.clone(), group.origin))
                .is_some()
            {
                return Err(CertificationError::Mismatch("duplicate source binder"));
            }
        }
    }
    Ok(sources)
}

/// Recheck original/fresh sidecars and dependency bytes against a worker
/// receipt. A candidate is never admitted merely because its name or hash
/// appears in the receipt: every original global and source edge is checked.
pub(crate) fn certify_products(
    candidates: Option<&CandidateSet>,
    receipt: &CertifiedReceipt,
    fresh_products: &[RawModuleProduct],
    fresh_product_bytes: &[u8],
    fresh_evidence_bytes: &[u8],
    fresh_input_path: &Path,
    final_evidence: &DependencyEvidence,
    final_target_source: &str,
    endpoint_identity: &[u8],
    include: &[PathBuf],
) -> CertResult<Vec<PendingCertifiedGroup>> {
    let normalized = DependencyEvidence::from_worker(
        fresh_evidence_bytes,
        fresh_input_path,
        final_target_source,
    )
    .ok_or(CertificationError::StaleEvidence)?;
    if serde_json::to_vec(&normalized)
        .map_err(|_| CertificationError::Mismatch("fresh evidence encoding"))?
        != serde_json::to_vec(final_evidence)
            .map_err(|_| CertificationError::Mismatch("fresh evidence encoding"))?
    {
        return Err(CertificationError::Mismatch("fresh evidence bytes"));
    }
    if !final_evidence.valid(final_target_source) {
        return Err(CertificationError::StaleEvidence);
    }
    let requirements = crate::prepared_artifact::production_requirements()
        .map_err(|_| CertificationError::Mismatch("production requirements"))?;
    let parsed_fresh = parse_module_products(
        fresh_product_bytes,
        &requirements,
        crate::module_candidates::product_decode_limits(),
    )?;
    if parsed_fresh != fresh_products {
        return Err(CertificationError::Mismatch("fresh product bytes"));
    }
    let fresh_sidecars =
        crate::module_candidates::split_module_product_bytes(fresh_product_bytes, &parsed_fresh)
            .ok_or(CertificationError::Mismatch("fresh module product framing"))?;
    let fresh_sidecars: BTreeMap<_, _> = parsed_fresh
        .iter()
        .zip(fresh_sidecars)
        .map(|(product, bytes)| ((product.unit.clone(), product.module.clone()), bytes))
        .collect();
    let mut seen_modules = BTreeSet::new();
    let mut fresh_modules = BTreeSet::new();
    let mut groups = Vec::new();
    for accepted in &receipt.modules {
        let key = (accepted.unit.clone(), accepted.module.clone());
        if !seen_modules.insert(key.clone()) {
            return Err(CertificationError::Mismatch("duplicate receipt module"));
        }
        let (product, receipt_bytes, module_bytes, evidence, source_sha, version) =
            match accepted.origin {
                ProductOrigin::Fresh => {
                    fresh_modules.insert(key.clone());
                    if accepted.module_version.is_some() {
                        return Err(CertificationError::Mismatch(
                            "fresh version must be derived",
                        ));
                    }
                    let product = matching_product(&parsed_fresh, &key.0, &key.1)?;
                    let module_bytes = fresh_sidecars
                        .get(&key)
                        .ok_or(CertificationError::Mismatch("fresh module product"))?;
                    let source_sha = ready_source_sha(final_evidence, &key.0, &key.1)?;
                    let version = fresh_module_version(
                        endpoint_identity,
                        include,
                        &source_sha,
                        &product.interface,
                        module_bytes,
                    )?;
                    (
                        product,
                        fresh_product_bytes,
                        module_bytes.as_slice(),
                        final_evidence,
                        source_sha,
                        version,
                    )
                }
                ProductOrigin::Cached => {
                    let bundle = candidates
                        .and_then(|set| set.by_owner.get(&key))
                        .ok_or(CertificationError::Mismatch("selected candidate"))?;
                    if accepted.module_version.as_ref() != Some(&bundle.owner.module_version)
                        || bundle.owner.skinny_iface_sha256 != accepted.skinny_iface_sha256
                        || bundle.owner.product_sha256 != accepted.product_sha256
                        || !bundle.evidence.valid(&bundle.target_source)
                        || bundle.source_sha256 != hex(&accepted.source_sha256)
                        || bundle.iface_sha256 != hex(&accepted.skinny_iface_sha256)
                    {
                        return Err(CertificationError::Mismatch("candidate owner/evidence"));
                    }
                    if sha(&read_bounded(&bundle.source, SOURCE_LIMIT)?) != accepted.source_sha256
                        || sha(&read_bounded(&bundle.iface_path, PACKAGE_INTERFACE_LIMIT)?)
                            != accepted.skinny_iface_sha256
                    {
                        return Err(CertificationError::StaleEvidence);
                    }
                    let original = parse_module_products(
                        &bundle.product_bytes,
                        &requirements,
                        crate::module_candidates::product_decode_limits(),
                    )?;
                    if matching_product(&original, &key.0, &key.1)? != &bundle.product {
                        return Err(CertificationError::Mismatch("original product bytes"));
                    }
                    if ready_source_sha(final_evidence, &key.0, &key.1)? != accepted.source_sha256 {
                        return Err(CertificationError::Mismatch("candidate current source"));
                    }
                    let original_imports = bundle
                        .evidence
                        .modules
                        .iter()
                        .find(|row| row.unit == key.0 && row.module == key.1 && !row.boot)
                        .map(|row| &row.imports);
                    let current_imports = final_evidence
                        .modules
                        .iter()
                        .find(|row| row.unit == key.0 && row.module == key.1 && !row.boot)
                        .map(|row| &row.imports);
                    if original_imports.is_none()
                        || original_imports.map(|rows| rows.len())
                            != current_imports.map(|rows| rows.len())
                        || original_imports
                            .unwrap()
                            .iter()
                            .zip(current_imports.unwrap())
                            .any(|(old, new)| {
                                old.qualifier != new.qualifier
                                    || old.module != new.module
                                    || old.boot != new.boot
                                    || old.selected != new.selected
                            })
                    {
                        return Err(CertificationError::Mismatch("candidate direct imports"));
                    }
                    // `bundle.product` is a clone of the exact parsed original.
                    (
                        &bundle.product,
                        bundle.product_bytes.as_slice(),
                        bundle.product_bytes.as_slice(),
                        &bundle.evidence,
                        ready_source_sha(&bundle.evidence, &key.0, &key.1)?,
                        bundle.owner.module_version.clone(),
                    )
                }
            };
        let owner = CachedHomeOwner {
            unit: key.0.clone(),
            module: key.1.clone(),
            module_version: version,
            skinny_iface_sha256: accepted.skinny_iface_sha256,
            product_sha256: sha(module_bytes),
        };
        let evidence_digest = match accepted.origin {
            ProductOrigin::Fresh => sha(fresh_evidence_bytes),
            ProductOrigin::Cached => sha(&serde_json::to_vec(evidence)
                .map_err(|_| CertificationError::Mismatch("dependency witness encoding"))?),
        };
        if source_sha != accepted.source_sha256
            || sha(receipt_bytes) != accepted.product_sha256
            || sha(&product.interface) != owner.skinny_iface_sha256
            || evidence_digest != accepted.dependency_witness_sha256
        {
            return Err(CertificationError::Mismatch(
                "product/iface/source/evidence digest",
            ));
        }
        if product.groups.len() != accepted.groups.len() {
            return Err(CertificationError::Mismatch("original group count"));
        }
        let mut seen_ordinals = BTreeSet::new();
        for (group, witness) in product.groups.iter().zip(&accepted.groups) {
            if group.original_ordinal() != witness.original_ordinal
                || !seen_ordinals.insert(witness.original_ordinal)
                || group.globals().len() != witness.globals.len()
            {
                return Err(CertificationError::Mismatch("original group/globals"));
            }
            let mut imports = Vec::with_capacity(witness.globals.len());
            for (declaration, selected) in group.globals().iter().zip(&witness.globals) {
                imports.push(validate_global_witness(
                    declaration,
                    group.definitions().signatures(),
                    selected,
                )?);
            }
            groups.push((accepted.origin, owner.clone(), group.clone(), imports));
        }
    }
    for product in &parsed_fresh {
        if !fresh_modules.contains(&(product.unit.clone(), product.module.clone())) {
            return Err(CertificationError::Mismatch("unwitnessed fresh module"));
        }
    }
    let mut source_groups = SourceGroupMap::new();
    for (origin, owner, group, _) in &groups {
        for binder in group.binders() {
            let key = (
                owner.unit.clone(),
                owner.module.clone(),
                group.original_ordinal(),
                binder.clone(),
            );
            if source_groups
                .insert(key, (owner.clone(), *origin))
                .is_some()
            {
                return Err(CertificationError::Mismatch("duplicate source binder"));
            }
        }
    }
    groups
        .into_iter()
        .map(|(origin, owner, group, imports)| {
            let imports = imports
                .into_iter()
                .map(|import| resolve_receipt_owner(import, &source_groups, &receipt.packages))
                .collect::<CertResult<_>>()?;
            Ok(PendingCertifiedGroup {
                origin,
                owner,
                group,
                imports,
            })
        })
        .collect()
}

/// Bind a target's declared globals to the same compiler transaction's
/// source inventory. Retained identities remain unresolved until runtime
/// checks their lexical scope and assigns authoritative session IDs.
pub fn certify_target_owners(
    prepared: &PreparedProgram,
    accepted: &[AcceptedGlobal],
    groups: &[PendingCertifiedGroup],
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
) -> CertResult<Vec<PendingImportOwner>> {
    if prepared.globals().len() != accepted.len() {
        return Err(CertificationError::Mismatch("target global count"));
    }
    let sources = certified_source_map(groups)?;
    prepared
        .globals()
        .iter()
        .zip(accepted)
        .map(|(declaration, selected)| {
            let owner = validate_global_witness(declaration, prepared.signatures(), selected)?;
            resolve_receipt_owner(owner, &sources, packages)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{ModuleEvidence, SourceEvidence};
    use tidepool_repr::execution_schema::testing;

    fn sidecar() -> Vec<u8> {
        let value = Value::Array(vec![
            Value::Text("TPMOD".into()),
            Value::Integer(1.into()),
            Value::Array(vec![Value::Array(vec![
                Value::Text("main".into()),
                Value::Text("Fresh".into()),
                Value::Bytes(vec![0x42]),
                Value::Array(vec![]),
            ])]),
        ]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).unwrap();
        bytes
    }

    fn evidence(source: &str) -> DependencyEvidence {
        DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: vec![SourceEvidence {
                path: "@generated-source".into(),
                sha256: hex(&sha(source.as_bytes())),
            }],
            resolutions: vec![],
            packages: vec![],
            modules: vec![ModuleEvidence {
                unit: "main".into(),
                module: "Fresh".into(),
                boot: false,
                source: "@generated-source".into(),
                imports: vec![],
                product: ProductAvailability::Ready,
            }],
        }
    }

    fn receipt(
        bytes: &[u8],
        evidence: &DependencyEvidence,
        source: &str,
    ) -> CertifiedModuleReceipt {
        CertifiedModuleReceipt {
            origin: ProductOrigin::Fresh,
            unit: "main".into(),
            module: "Fresh".into(),
            module_version: None,
            skinny_iface_sha256: sha(&[0x42]),
            product_sha256: sha(bytes),
            source_sha256: sha(source.as_bytes()),
            dependency_witness_sha256: sha(&serde_json::to_vec(evidence).unwrap()),
            groups: vec![],
        }
    }

    #[test]
    fn fresh_product_requires_exact_sidecar_and_evidence() {
        let source = "module Fresh where";
        let bytes = sidecar();
        let evidence = evidence(source);
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("Fresh.hs");
        std::fs::write(&input, source).unwrap();
        let mut worker_evidence = evidence.clone();
        worker_evidence.sources[0].path = input.clone();
        worker_evidence.modules[0].source = input.clone();
        let raw_evidence = serde_json::to_vec(&worker_evidence).unwrap();
        let parsed = parse_module_products(
            &bytes,
            &crate::prepared_artifact::production_requirements().unwrap(),
            DecodeLimits::default(),
        )
        .unwrap();
        let mut accepted = receipt(&bytes, &evidence, source);
        accepted.dependency_witness_sha256 = sha(&raw_evidence);
        assert!(
            certify_products(
                None,
                &CertifiedReceipt {
                    modules: vec![accepted.clone()],
                    targets: BTreeMap::new(),
                    packages: BTreeMap::new()
                },
                &parsed,
                &bytes,
                &raw_evidence,
                &input,
                &evidence,
                source,
                b"producer",
                &[],
            )
            .unwrap()
            .is_empty()
        );
        let mut substituted = evidence.clone();
        substituted.packages.push("unrelated selection".into());
        assert!(matches!(
            certify_products(
                None,
                &CertifiedReceipt {
                    modules: vec![accepted.clone()],
                    targets: BTreeMap::new(),
                    packages: BTreeMap::new(),
                },
                &parsed,
                &bytes,
                &raw_evidence,
                &input,
                &substituted,
                source,
                b"producer",
                &[],
            ),
            Err(CertificationError::Mismatch("fresh evidence bytes"))
        ));
        let mut changed = accepted.clone();
        changed.product_sha256 = [9; 32];
        assert!(matches!(
            certify_products(
                None,
                &CertifiedReceipt {
                    modules: vec![changed],
                    targets: BTreeMap::new(),
                    packages: BTreeMap::new()
                },
                &parsed,
                &bytes,
                &raw_evidence,
                &input,
                &evidence,
                source,
                b"producer",
                &[]
            ),
            Err(CertificationError::Mismatch(
                "product/iface/source/evidence digest"
            ))
        ));
        let mut changed = accepted;
        changed.dependency_witness_sha256 = [7; 32];
        assert!(
            certify_products(
                None,
                &CertifiedReceipt {
                    modules: vec![changed],
                    targets: BTreeMap::new(),
                    packages: BTreeMap::new()
                },
                &parsed,
                &bytes,
                &raw_evidence,
                &input,
                &evidence,
                source,
                b"producer",
                &[],
            )
            .is_err()
        );
    }

    #[test]
    fn receipt_decoder_requires_bounded_exact_tuple() {
        let source = "module Fresh where";
        let bytes = sidecar();
        let evidence = evidence(source);
        let accepted = receipt(&bytes, &evidence, source);
        let value = Value::Array(vec![
            Value::Text("TPCERT".into()),
            Value::Integer(2.into()),
            Value::Array(vec![Value::Array(vec![
                Value::Text("fresh".into()),
                Value::Text("main".into()),
                Value::Text("Fresh".into()),
                Value::Null,
                Value::Text(hex(&accepted.source_sha256)),
                Value::Text(hex(&accepted.skinny_iface_sha256)),
                Value::Text(hex(&accepted.product_sha256)),
                Value::Text(hex(&accepted.dependency_witness_sha256)),
                Value::Array(vec![]),
            ])]),
            Value::Array(vec![Value::Array(vec![
                Value::Text("target".into()),
                Value::Array(vec![]),
            ])]),
            Value::Array(vec![Value::Array(vec![
                Value::Text("base".into()),
                Value::Text("Selected".into()),
                Value::Text("/tmp/Selected.hi".into()),
                Value::Text(hex(&[5; 32])),
            ])]),
        ]);
        let mut encoded = Vec::new();
        ciborium::ser::into_writer(&value, &mut encoded).unwrap();
        assert_eq!(
            decode_receipt(&encoded).unwrap(),
            CertifiedReceipt {
                modules: vec![accepted],
                targets: BTreeMap::from([("target".into(), vec![])]),
                packages: BTreeMap::from([(
                    ("base".into(), "Selected".into()),
                    PackageInterfaceWitness {
                        selected_path: PathBuf::from("/tmp/Selected.hi"),
                        sha256: [5; 32],
                    }
                )]),
            }
        );
        encoded.resize(RECEIPT_LIMIT + 1, 0);
        assert!(matches!(
            decode_receipt(&encoded),
            Err(CertificationError::Receipt("receipt size"))
        ));

        let large_groups = |count: usize| {
            let mut receipt = value.clone();
            let Value::Array(header) = &mut receipt else {
                unreachable!()
            };
            let Value::Array(modules) = &mut header[2] else {
                unreachable!()
            };
            let Value::Array(module) = &mut modules[0] else {
                unreachable!()
            };
            module[8] = Value::Array(
                (0..count)
                    .map(|ordinal| {
                        Value::Array(vec![
                            Value::Integer((ordinal as u64).into()),
                            Value::Array(vec![]),
                        ])
                    })
                    .collect(),
            );
            let mut encoded = Vec::new();
            ciborium::ser::into_writer(&receipt, &mut encoded).unwrap();
            encoded
        };
        // A real resident Tidepool.Effects.Core product has 5,930 groups.
        assert_eq!(
            decode_receipt(&large_groups(5_930)).unwrap().modules[0]
                .groups
                .len(),
            5_930
        );
        assert!(matches!(
            decode_receipt(&large_groups(GROUP_LIMIT + 1)),
            Err(CertificationError::Receipt("group count"))
        ));
    }

    #[test]
    fn target_retained_owner_keeps_identity_and_generation_unresolved() {
        let mut wire = testing::wire_program();
        let identity = testing::identity("Val.G7", "retained");
        wire.globals.push(GlobalDecl {
            identity: identity.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: Some(7),
        });
        let prepared = testing::prepare(wire).unwrap();
        let selected = AcceptedGlobal {
            identity: identity.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            owner: ReceiptImportOwner::Retained {
                identity: identity.clone(),
                generation: 7,
            },
        };
        assert_eq!(
            certify_target_owners(&prepared, &[selected.clone()], &[], &BTreeMap::new()).unwrap(),
            vec![PendingImportOwner::Retained {
                identity,
                generation: 7,
            }]
        );
        let mut stale = selected;
        stale.owner = ReceiptImportOwner::Retained {
            identity: stale.identity.clone(),
            generation: 8,
        };
        assert!(certify_target_owners(&prepared, &[stale], &[], &BTreeMap::new()).is_err());
    }

    #[test]
    fn package_owner_requires_selected_interface_bytes_at_final_admission() {
        let directory = tempfile::tempdir().unwrap();
        let selected_path = directory.path().join("Selected.hi");
        std::fs::write(&selected_path, b"selected interface").unwrap();
        let interface_digest = sha(b"selected interface");
        let binder = testing::identity("base:Selected", "member");
        let import = ReceiptImportOwner::Package {
            unit: "base".into(),
            module: "Selected".into(),
            binder: binder.clone(),
            interface_digest,
        };
        let packages = BTreeMap::from([(
            ("base".into(), "Selected".into()),
            PackageInterfaceWitness {
                selected_path: selected_path.clone(),
                sha256: interface_digest,
            },
        )]);
        assert!(matches!(
            resolve_receipt_owner(import.clone(), &SourceGroupMap::new(), &packages),
            Ok(PendingImportOwner::Package { .. })
        ));
        std::fs::write(&selected_path, b"changed interface").unwrap();
        assert!(matches!(
            resolve_receipt_owner(import, &SourceGroupMap::new(), &packages),
            Err(CertificationError::StaleEvidence)
        ));
    }
}
