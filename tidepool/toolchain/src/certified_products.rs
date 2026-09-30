//! Final Rust admission of module products checked by the resident compiler.
//! Worker receipts describe GHC's selected owners; stored product bytes and
//! dependency witnesses remain independent inputs to this check.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ciborium::value::Value;
use sha2::{Digest, Sha256};
use tidepool_repr::execution_schema::{
    parse_module_products, CachedHomeOwner, GlobalDecl, ModuleVersion, PreparedProgram,
    ProjectedGroup, RawModuleProduct, ResultContract, RuntimeRep, Signature, SymbolIdentity,
};

use crate::cache::{DependencyEvidence, ProductAvailability};
use crate::module_candidates::CandidateSet;

const RECEIPT_LIMIT: usize = 4 << 20;
const MODULE_LIMIT: usize = 128;
const GROUP_LIMIT: usize = 8192;
const GLOBAL_LIMIT: usize = 65536;
// Dictionary references retain full witnesses after decoding. Bound both the
// number of references and their expanded canonical bytes independently of
// the compact encoded receipt, so sharing cannot hide unbounded allocation.
const GLOBAL_REFERENCE_LIMIT: usize = 65536;
const EXPANDED_GLOBAL_BYTES_LIMIT: usize = 16 << 20;
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

/// The selected package closure of one sealed target. A digest from another
/// transaction cannot authorize a local package definition in this target.
#[derive(Clone, Debug, Default)]
pub struct CertifiedTargetPackageInterfaces {
    target: Option<std::sync::Arc<PreparedProgram>>,
    interfaces: BTreeMap<(String, String), [u8; 32]>,
}

impl CertifiedTargetPackageInterfaces {
    pub fn matches_target(&self, target: &PreparedProgram) -> bool {
        self.target
            .as_deref()
            .is_some_and(|bound| std::ptr::eq(bound, target) || bound == target)
    }

    pub fn interface_digest(&self, unit: &str, module: &str) -> Option<[u8; 32]> {
        self.interfaces
            .get(&(unit.to_owned(), module.to_owned()))
            .copied()
    }
}

pub(crate) fn certify_target_package_interfaces(
    target: &std::sync::Arc<PreparedProgram>,
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
) -> CertResult<CertifiedTargetPackageInterfaces> {
    let mut interfaces = BTreeMap::new();
    for (owner, witness) in packages {
        if owner.0.is_empty() || owner.1.is_empty() || !witness.selected_path.is_absolute() {
            return Err(CertificationError::Mismatch(
                "target package interface owner",
            ));
        }
        if sha(&read_bounded(
            &witness.selected_path,
            PACKAGE_INTERFACE_LIMIT,
        )?) != witness.sha256
        {
            return Err(CertificationError::StaleEvidence);
        }
        interfaces.insert(owner.clone(), witness.sha256);
    }
    Ok(CertifiedTargetPackageInterfaces {
        target: Some(target.clone()),
        interfaces,
    })
}

pub(crate) fn inherited_package_witnesses(
    products: &[crate::recovery_artifacts::CertifiedRecoveryProduct],
) -> CertResult<BTreeMap<(String, String), PackageInterfaceWitness>> {
    let mut selected = BTreeMap::new();
    for product in products {
        let witness = decode_home_witness(product.certification_bytes())?;
        if &witness.owner != product.owner() {
            return Err(CertificationError::Mismatch(
                "inherited package product owner",
            ));
        }
        for (owner, interface) in witness.packages {
            if !interface.selected_path.is_absolute()
                || sha(&read_bounded(
                    &interface.selected_path,
                    PACKAGE_INTERFACE_LIMIT,
                )?) != interface.sha256
            {
                return Err(CertificationError::StaleEvidence);
            }
            if selected
                .insert(owner, interface.clone())
                .is_some_and(|old| old != interface)
            {
                return Err(CertificationError::Mismatch("inherited package selection"));
            }
        }
    }
    Ok(selected)
}

/// A group whose retained globals still need an exact live binding or native
/// export owner. Runtime resolves those through its binding table or owning
/// machine's export ledger before constructing `CertifiedGroup`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingCertifiedGroup {
    origin: ProductOrigin,
    owner: CachedHomeOwner,
    group: ProjectedGroup,
    imports: Vec<PendingImportOwner>,
}

pub(crate) struct CertifiedProducts {
    pub groups: Vec<PendingCertifiedGroup>,
    pub recovery_products: Vec<crate::recovery_artifacts::CertifiedRecoveryProduct>,
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

/// Decode the worker's bounded `TPCERT` tuple. Version 3 shares complete exact
/// global rows through an immutable dictionary; version 2 retains inline rows.
/// Original groups and executable targets preserve their ordered witnesses.
pub fn decode_receipt(bytes: &[u8]) -> CertResult<CertifiedReceipt> {
    if bytes.len() > RECEIPT_LIMIT {
        return Err(CertificationError::Receipt("receipt size"));
    }
    let mut cursor = std::io::Cursor::new(bytes);
    let value: Value = ciborium::de::from_reader_with_recursion_limit(&mut cursor, 32)
        .map_err(|_| CertificationError::Receipt("CBOR"))?;
    if cursor.position() != bytes.len() as u64 {
        return Err(CertificationError::Receipt("trailing bytes"));
    }
    decode_receipt_value(&value)
}

fn decode_receipt_value(value: &Value) -> CertResult<CertifiedReceipt> {
    let header = array(value)?;
    if header.len() < 2 || string(&header[0])? != "TPCERT" {
        return Err(CertificationError::Receipt("receipt header"));
    }
    let version = number(&header[1])?;
    let mut dictionary = match (version, header.len()) {
        (2, 5) => None,
        (3, 6) => Some(GlobalDictionary::decode(&header[5])?),
        _ => return Err(CertificationError::Receipt("receipt header")),
    };
    let mut read_globals = |value: &Value| {
        let globals = array(value)?;
        if globals.len() > GLOBAL_LIMIT {
            return Err(CertificationError::Receipt("global count"));
        }
        match dictionary.as_mut() {
            Some(dictionary) => dictionary.resolve(globals),
            None => globals
                .iter()
                .map(accepted_global)
                .collect::<CertResult<_>>(),
        }
    };
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
                    Ok(AcceptedGroup {
                        original_ordinal: u32::try_from(number(&row[0])?)
                            .map_err(|_| CertificationError::Receipt("group ordinal"))?,
                        globals: read_globals(&row[1])?,
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
        if targets.insert(name, read_globals(&row[1])?).is_some() {
            return Err(CertificationError::Receipt("duplicate target"));
        }
    }
    if dictionary
        .as_ref()
        .is_some_and(|dictionary| dictionary.used.len() != dictionary.rows.len())
    {
        return Err(CertificationError::Receipt(
            "unreferenced global dictionary row",
        ));
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

struct GlobalDictionary {
    rows: Vec<(AcceptedGlobal, usize)>,
    used: BTreeSet<usize>,
    references: usize,
    expanded_bytes: usize,
}

impl GlobalDictionary {
    fn decode(value: &Value) -> CertResult<Self> {
        let rows = array(value)?;
        if rows.len() > GLOBAL_LIMIT {
            return Err(CertificationError::Receipt("global dictionary count"));
        }
        let mut unique = BTreeSet::new();
        let rows = rows
            .iter()
            .map(|value| {
                let global = accepted_global(value)?;
                let mut canonical = Vec::new();
                ciborium::ser::into_writer(&value_global(&global), &mut canonical)
                    .map_err(|_| CertificationError::Receipt("global dictionary encoding"))?;
                let length = canonical.len();
                if !unique.insert(canonical) {
                    return Err(CertificationError::Receipt(
                        "duplicate global dictionary row",
                    ));
                }
                Ok((global, length))
            })
            .collect::<CertResult<_>>()?;
        Ok(Self {
            rows,
            used: BTreeSet::new(),
            references: 0,
            expanded_bytes: 0,
        })
    }

    fn resolve(&mut self, indices: &[Value]) -> CertResult<Vec<AcceptedGlobal>> {
        if indices.len() > GLOBAL_REFERENCE_LIMIT - self.references {
            return Err(CertificationError::Receipt("expanded global count"));
        }
        let selected = indices
            .iter()
            .map(|value| {
                let index = usize::try_from(number(value)?)
                    .map_err(|_| CertificationError::Receipt("global dictionary index"))?;
                let (global, length) = self
                    .rows
                    .get(index)
                    .ok_or(CertificationError::Receipt("global dictionary index"))?;
                if *length > EXPANDED_GLOBAL_BYTES_LIMIT - self.expanded_bytes {
                    return Err(CertificationError::Receipt("expanded global bytes"));
                }
                self.expanded_bytes += length;
                self.used.insert(index);
                Ok(global.clone())
            })
            .collect::<CertResult<_>>()?;
        self.references += indices.len();
        Ok(selected)
    }
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
    package_imports: &[u8],
) -> CertResult<ModuleVersion> {
    let include = crate::module_candidates::context_paths(include)
        .ok_or(CertificationError::Mismatch("include context"))?;
    Ok(crate::module_candidates::module_version_for_product(
        endpoint_identity,
        &include,
        &hex(source_sha),
        interface,
        product_bytes,
        package_imports,
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
            if sources
                .keys()
                .any(|(home_unit, home_module, _, _)| home_unit == &unit && home_module == &module)
            {
                return Err(CertificationError::Mismatch(
                    "home owner downgraded to package",
                ));
            }
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

fn check_direct_package_agreement(
    sidecar: &[u8],
    owner: &CachedHomeOwner,
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
) -> CertResult<()> {
    let roots = crate::recovery_artifacts::validate_package_imports(
        sidecar,
        &owner.unit,
        &owner.module,
        &owner.skinny_iface_sha256,
        Path::new("module-package-imports.cbor"),
    )
    .map_err(|_| CertificationError::Mismatch("direct package import witness"))?;
    for (key, (path, digest)) in roots {
        if let Some(witness) = packages.get(&key) {
            if witness.selected_path != path || hex(&witness.sha256) != digest {
                return Err(CertificationError::Mismatch(
                    "direct package/receipt selection",
                ));
            }
        }
    }
    Ok(())
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

/// Exact artifact authenticated by the run-owned recovery reference.
pub struct InheritedProductInput<'a> {
    pub artifact: &'a crate::recovery_artifacts::VerifiedRecoveryArtifact,
}

struct HomeCertification {
    owner: CachedHomeOwner,
    groups: Vec<(u32, Vec<SymbolIdentity>, Vec<AcceptedGlobal>)>,
    sources: BTreeMap<(String, String), CachedHomeOwner>,
    packages: BTreeMap<(String, String), PackageInterfaceWitness>,
}

fn home_owner(value: &Value) -> CertResult<CachedHomeOwner> {
    let row = sized(value, 5)?;
    let unit = string(&row[0])?.to_owned();
    let module = string(&row[1])?.to_owned();
    if unit.is_empty() || module.is_empty() {
        return Err(CertificationError::Receipt("empty home owner"));
    }
    Ok(CachedHomeOwner {
        unit,
        module,
        module_version: ModuleVersion(digest(&row[2])?),
        skinny_iface_sha256: digest(&row[3])?,
        product_sha256: digest(&row[4])?,
    })
}

fn value_text(text: impl Into<String>) -> Value {
    Value::Text(text.into())
}
fn value_array(items: impl IntoIterator<Item = Value>) -> Value {
    Value::Array(items.into_iter().collect())
}
fn value_identity(symbol: &SymbolIdentity) -> Value {
    value_array([
        value_text(&symbol.unit),
        value_text(&symbol.module),
        value_text(&symbol.namespace),
        value_text(&symbol.occurrence),
        symbol
            .record_parent
            .as_ref()
            .map_or(Value::Null, value_text),
    ])
}
fn value_home(owner: &CachedHomeOwner) -> Value {
    value_array([
        value_text(&owner.unit),
        value_text(&owner.module),
        value_text(hex(&owner.module_version.0)),
        value_text(hex(&owner.skinny_iface_sha256)),
        value_text(hex(&owner.product_sha256)),
    ])
}
fn value_rep(rep: &RuntimeRep) -> Value {
    let (tag, bits) = match rep {
        RuntimeRep::Void => ("void", 0),
        RuntimeRep::LiftedRef => ("lifted", 0),
        RuntimeRep::UnliftedRef => ("unlifted", 0),
        RuntimeRep::Address => ("address", 0),
        RuntimeRep::Int(bits) => ("int", *bits),
        RuntimeRep::Word(bits) => ("word", *bits),
        RuntimeRep::Float(bits) => ("float", *bits),
    };
    value_array([value_text(tag), Value::Integer(bits.into())])
}
fn value_signature(signature: &Option<Signature>) -> Value {
    let Some(signature) = signature else {
        return Value::Null;
    };
    let (tag, results) = match &signature.results {
        ResultContract::Returns(results) => ("returns", results.as_slice()),
        ResultContract::NoSuccess => ("no_success", &[][..]),
        ResultContract::CallerResult => ("caller_result", &[][..]),
    };
    value_array([
        value_array(signature.arguments.iter().map(value_rep)),
        value_array([value_text(tag), value_array(results.iter().map(value_rep))]),
    ])
}
fn value_import(owner: &ReceiptImportOwner) -> Value {
    match owner {
        ReceiptImportOwner::Source {
            unit,
            module,
            module_version,
            original_ordinal,
            binder,
        } => value_array([
            value_text("source"),
            value_text(unit),
            value_text(module),
            module_version
                .as_ref()
                .map_or(Value::Null, |version| value_text(hex(&version.0))),
            Value::Integer((*original_ordinal).into()),
            value_identity(binder),
        ]),
        ReceiptImportOwner::Retained {
            identity,
            generation,
        } => value_array([
            value_text("retained"),
            value_identity(identity),
            Value::Integer((*generation).into()),
        ]),
        ReceiptImportOwner::Package {
            unit,
            module,
            binder,
            interface_digest,
        } => value_array([
            value_text("package"),
            value_text(unit),
            value_text(module),
            value_text(hex(interface_digest)),
            value_identity(binder),
        ]),
    }
}
fn value_global(global: &AcceptedGlobal) -> Value {
    value_array([
        value_identity(&global.identity),
        value_rep(&global.rep),
        value_signature(&global.entry_signature),
        Value::Bool(global.required_evaluated),
        value_import(&global.owner),
    ])
}
fn encode_home_witness(witness: &HomeCertification) -> CertResult<Vec<u8>> {
    let value = value_array([
        value_text("TPHOMEOWNERS"),
        Value::Integer(1.into()),
        value_home(&witness.owner),
        value_array(witness.groups.iter().map(|(ordinal, binders, globals)| {
            value_array([
                Value::Integer((*ordinal).into()),
                value_array(binders.iter().map(value_identity)),
                value_array(globals.iter().map(value_global)),
            ])
        })),
        value_array(witness.sources.values().map(value_home)),
        value_array(
            witness
                .packages
                .iter()
                .map(|((unit, module), package)| {
                    Ok(value_array([
                        value_text(unit),
                        value_text(module),
                        value_text(
                            package
                                .selected_path
                                .to_str()
                                .ok_or(CertificationError::Mismatch("package witness path"))?
                                .to_owned(),
                        ),
                        value_text(hex(&package.sha256)),
                    ]))
                })
                .collect::<CertResult<Vec<_>>>()?,
        ),
    ]);
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&value, &mut bytes)
        .map_err(|_| CertificationError::Receipt("home witness encoding"))?;
    if bytes.len() > RECEIPT_LIMIT {
        return Err(CertificationError::Receipt("receipt size"));
    }
    Ok(bytes)
}

fn decode_home_witness(bytes: &[u8]) -> CertResult<HomeCertification> {
    if bytes.len() > RECEIPT_LIMIT {
        return Err(CertificationError::Receipt("receipt size"));
    }
    let mut cursor = std::io::Cursor::new(bytes);
    let value: Value = ciborium::de::from_reader_with_recursion_limit(&mut cursor, 32)
        .map_err(|_| CertificationError::Receipt("home witness encoding"))?;
    if cursor.position() != bytes.len() as u64 {
        return Err(CertificationError::Receipt("trailing bytes"));
    }
    let row = sized(&value, 6)?;
    if string(&row[0])? != "TPHOMEOWNERS" || number(&row[1])? != 1 {
        return Err(CertificationError::Receipt("home witness version"));
    }
    let owner = home_owner(&row[2])?;
    let groups = array(&row[3])?;
    if groups.len() > GROUP_LIMIT {
        return Err(CertificationError::Receipt("group count"));
    }
    let mut ordinals = BTreeSet::new();
    let mut binders_seen = BTreeSet::new();
    let mut globals_count = 0;
    let groups = groups
        .iter()
        .map(|group| {
            let group = sized(group, 3)?;
            let ordinal = u32::try_from(number(&group[0])?)
                .map_err(|_| CertificationError::Receipt("source group ordinal"))?;
            if !ordinals.insert(ordinal) {
                return Err(CertificationError::Receipt("duplicate group ordinal"));
            }
            let binders = array(&group[1])?
                .iter()
                .map(identity)
                .collect::<CertResult<Vec<_>>>()?;
            if binders.is_empty()
                || binders.iter().any(|binder| {
                    binder.unit != owner.unit
                        || binder.module != owner.module
                        || !binders_seen.insert(binder.clone())
                })
            {
                return Err(CertificationError::Receipt("home witness binders"));
            }
            let globals = array(&group[2])?;
            globals_count += globals.len();
            if globals_count > GLOBAL_LIMIT {
                return Err(CertificationError::Receipt("global count"));
            }
            Ok((
                ordinal,
                binders,
                globals
                    .iter()
                    .map(accepted_global)
                    .collect::<CertResult<Vec<_>>>()?,
            ))
        })
        .collect::<CertResult<Vec<_>>>()?;
    let mut sources = BTreeMap::new();
    if array(&row[4])?.len() > MODULE_LIMIT {
        return Err(CertificationError::Receipt("source owner count"));
    }
    for source in array(&row[4])? {
        let source = home_owner(source)?;
        if sources
            .insert((source.unit.clone(), source.module.clone()), source)
            .is_some()
        {
            return Err(CertificationError::Receipt("duplicate source owner"));
        }
    }
    let mut packages = BTreeMap::new();
    if array(&row[5])?.len() > PACKAGE_LIMIT {
        return Err(CertificationError::Receipt("package count"));
    }
    for package in array(&row[5])? {
        let fields = sized(package, 4)?;
        let key = (
            string(&fields[0])?.to_owned(),
            string(&fields[1])?.to_owned(),
        );
        let selected_path = PathBuf::from(string(&fields[2])?);
        if key.0.is_empty()
            || key.1.is_empty()
            || !selected_path.is_absolute()
            || packages
                .insert(
                    key,
                    PackageInterfaceWitness {
                        selected_path,
                        sha256: digest(&fields[3])?,
                    },
                )
                .is_some()
        {
            return Err(CertificationError::Receipt("package witness"));
        }
    }
    let mut used_sources = BTreeSet::new();
    let mut used_packages = BTreeSet::new();
    for (_, _, globals) in &groups {
        for global in globals {
            match &global.owner {
                ReceiptImportOwner::Source {
                    unit,
                    module,
                    module_version,
                    binder,
                    ..
                } => {
                    let key = (unit.clone(), module.clone());
                    let source = sources
                        .get(&key)
                        .ok_or(CertificationError::Mismatch("home source witness"))?;
                    if module_version.as_ref() != Some(&source.module_version)
                        || binder != &global.identity
                        || binder.unit != *unit
                        || binder.module != *module
                    {
                        return Err(CertificationError::Mismatch("home source witness"));
                    }
                    used_sources.insert(key);
                }
                ReceiptImportOwner::Package {
                    unit,
                    module,
                    interface_digest,
                    binder,
                } => {
                    let key = (unit.clone(), module.clone());
                    if packages.get(&key).map(|witness| witness.sha256) != Some(*interface_digest)
                        || binder != &global.identity
                        || binder.unit != *unit
                        || binder.module != *module
                    {
                        return Err(CertificationError::Mismatch("home package witness"));
                    }
                    used_packages.insert(key);
                }
                ReceiptImportOwner::Retained { identity, .. } if identity != &global.identity => {
                    return Err(CertificationError::Mismatch("home retained witness"));
                }
                _ => {}
            }
        }
    }
    if used_sources.len() != sources.len() || used_packages.len() != packages.len() {
        return Err(CertificationError::Receipt("unused owner witness"));
    }
    let witness = HomeCertification {
        owner,
        groups,
        sources,
        packages,
    };
    if encode_home_witness(&witness)? != bytes {
        return Err(CertificationError::Receipt("noncanonical home witness"));
    }
    Ok(witness)
}

/// Called by the recovery owner before retaining or materializing a seal.
pub fn validate_home_certification(bytes: &[u8], owner: &CachedHomeOwner) -> CertResult<()> {
    verify_home_witness(bytes, owner).map(|_| ())
}

pub(crate) fn certified_home_requirements(
    bytes: &[u8],
    owner: &CachedHomeOwner,
) -> CertResult<Vec<CachedHomeOwner>> {
    Ok(verify_home_witness(bytes, owner)?
        .sources
        .into_values()
        .collect())
}

fn verify_home_witness(bytes: &[u8], owner: &CachedHomeOwner) -> CertResult<HomeCertification> {
    let witness = decode_home_witness(bytes)?;
    if &witness.owner != owner {
        return Err(CertificationError::Mismatch("home certification owner"));
    }
    for package in witness.packages.values() {
        if sha(&read_bounded(
            &package.selected_path,
            PACKAGE_INTERFACE_LIMIT,
        )?) != package.sha256
        {
            return Err(CertificationError::StaleEvidence);
        }
    }
    Ok(witness)
}

/// Seal original Rust-admitted ownership, without selecting new owners from
/// spellings. Even an instance-only Home with zero groups has an explicit seal.
pub fn encode_home_certification(
    owner: &CachedHomeOwner,
    groups: &[PendingCertifiedGroup],
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
) -> CertResult<Vec<u8>> {
    let mut witness = HomeCertification {
        owner: owner.clone(),
        groups: Vec::new(),
        sources: BTreeMap::new(),
        packages: BTreeMap::new(),
    };
    for group in groups {
        if group.owner() != owner {
            return Err(CertificationError::Mismatch(
                "home certification group owner",
            ));
        }
        if group.group.globals().len() != group.imports.len() {
            return Err(CertificationError::Mismatch(
                "home certification global count",
            ));
        }
        let mut globals = Vec::new();
        for (declaration, selected) in group.group.globals().iter().zip(&group.imports) {
            let selected = match selected {
                PendingImportOwner::Source {
                    owner,
                    original_ordinal,
                    binder,
                } => {
                    let key = (owner.unit.clone(), owner.module.clone());
                    if witness
                        .sources
                        .insert(key, owner.clone())
                        .is_some_and(|old| old != *owner)
                    {
                        return Err(CertificationError::Mismatch("ambiguous home source owner"));
                    }
                    ReceiptImportOwner::Source {
                        unit: owner.unit.clone(),
                        module: owner.module.clone(),
                        module_version: Some(owner.module_version.clone()),
                        original_ordinal: *original_ordinal,
                        binder: binder.clone(),
                    }
                }
                PendingImportOwner::Retained {
                    identity,
                    generation,
                } => ReceiptImportOwner::Retained {
                    identity: identity.clone(),
                    generation: *generation,
                },
                PendingImportOwner::Package {
                    unit,
                    module,
                    binder,
                    interface_digest,
                } => {
                    let key = (unit.clone(), module.clone());
                    let package = packages
                        .get(&key)
                        .ok_or(CertificationError::Mismatch("package interface witness"))?;
                    witness.packages.insert(key, package.clone());
                    ReceiptImportOwner::Package {
                        unit: unit.clone(),
                        module: module.clone(),
                        binder: binder.clone(),
                        interface_digest: *interface_digest,
                    }
                }
            };
            let global = AcceptedGlobal {
                identity: declaration.identity.clone(),
                rep: declaration.rep.clone(),
                entry_signature: declaration
                    .entry_signature
                    .and_then(|id| group.group.definitions().signatures().get(id.0 as usize))
                    .cloned(),
                required_evaluated: declaration.required_evaluated,
                owner: selected,
            };
            validate_global_witness(declaration, group.group.definitions().signatures(), &global)?;
            globals.push(global);
        }
        witness.groups.push((
            group.group.original_ordinal(),
            group.group.binders().to_vec(),
            globals,
        ));
    }
    let bytes = encode_home_witness(&witness)?;
    validate_home_certification(&bytes, owner)?;
    Ok(bytes)
}

/// Re-admit complete original products using durable ownership receipts. Source
/// cycles are resolved after the entire current/inherited inventory is built.
pub fn certify_inherited_products(
    inputs: &[InheritedProductInput<'_>],
    current_groups: &[PendingCertifiedGroup],
) -> CertResult<Vec<PendingCertifiedGroup>> {
    let requirements = crate::prepared_artifact::production_requirements()
        .map_err(|_| CertificationError::Mismatch("production requirements"))?;
    let mut modules = BTreeSet::new();
    let mut parsed = Vec::new();
    for input in inputs {
        let artifact = input.artifact;
        let reference = &artifact.reference;
        if !modules.insert((reference.unit.clone(), reference.module.clone())) {
            return Err(CertificationError::Mismatch("duplicate inherited module"));
        }
        let owner = CachedHomeOwner {
            unit: reference.unit.clone(),
            module: reference.module.clone(),
            module_version: ModuleVersion(reference.module_version),
            skinny_iface_sha256: reference.skinny_iface_sha256,
            product_sha256: reference.product_sha256,
        };
        if sha(&artifact.interface_bytes) != owner.skinny_iface_sha256
            || sha(&artifact.product_bytes) != owner.product_sha256
            || sha(&artifact.certification_bytes) != reference.certification_sha256
        {
            return Err(CertificationError::Mismatch("inherited artifact bytes"));
        }
        let products = parse_module_products(
            &artifact.product_bytes,
            &requirements,
            crate::module_candidates::product_decode_limits(),
        )?;
        let [product] = products.as_slice() else {
            return Err(CertificationError::Mismatch("inherited per-module product"));
        };
        if product.unit != owner.unit
            || product.module != owner.module
            || product.interface != artifact.interface_bytes
        {
            return Err(CertificationError::Mismatch(
                "inherited interface/product pair",
            ));
        }
        let witness = verify_home_witness(&artifact.certification_bytes, &owner)?;
        check_direct_package_agreement(&artifact.package_imports_bytes, &owner, &witness.packages)?;
        parsed.push((product.clone(), witness));
    }
    certify_inherited_inventory(parsed, current_groups)
}

fn certify_inherited_inventory(
    parsed: Vec<(RawModuleProduct, HomeCertification)>,
    current_groups: &[PendingCertifiedGroup],
) -> CertResult<Vec<PendingCertifiedGroup>> {
    let mut sources = certified_source_map(current_groups)?;
    let mut homes = BTreeMap::new();
    let mut shared = BTreeSet::new();
    let mut inherited_seen = BTreeSet::new();
    for current in current_groups {
        let owner = current.owner();
        if homes
            .insert((owner.unit.clone(), owner.module.clone()), owner.clone())
            .is_some_and(|old| old != *owner)
        {
            return Err(CertificationError::Mismatch(
                "ambiguous current home module",
            ));
        }
    }
    for (product, witness) in &parsed {
        let owner = &witness.owner;
        let key = (owner.unit.clone(), owner.module.clone());
        if !inherited_seen.insert(key.clone()) {
            return Err(CertificationError::Mismatch(
                "duplicate inherited home module",
            ));
        }
        if let Some(current_owner) = homes.get(&key) {
            if current_owner != owner {
                return Err(CertificationError::Mismatch(
                    "ambiguous inherited/current home module",
                ));
            }
            let current: Vec<_> = current_groups
                .iter()
                .filter(|group| group.owner() == owner)
                .collect();
            if current.len() != product.groups.len()
                || product
                    .groups
                    .iter()
                    .any(|original| !current.iter().any(|group| group.group() == original))
            {
                return Err(CertificationError::Mismatch("shared original home groups"));
            }
            shared.insert(key);
        } else {
            homes.insert(key, owner.clone());
        }
        if product.groups.len() != witness.groups.len() {
            return Err(CertificationError::Mismatch(
                "inherited original group count",
            ));
        }
        for (group, (ordinal, binders, globals)) in product.groups.iter().zip(&witness.groups) {
            if group.original_ordinal() != *ordinal
                || group.binders() != binders
                || group.globals().len() != globals.len()
            {
                return Err(CertificationError::Mismatch(
                    "inherited original group/globals",
                ));
            }
            for binder in group.binders() {
                if shared.contains(&(owner.unit.clone(), owner.module.clone())) {
                    continue;
                }
                if sources
                    .insert(
                        (
                            owner.unit.clone(),
                            owner.module.clone(),
                            *ordinal,
                            binder.clone(),
                        ),
                        (owner.clone(), ProductOrigin::Cached),
                    )
                    .is_some()
                {
                    return Err(CertificationError::Mismatch("duplicate source binder"));
                }
            }
        }
    }
    let mut result = Vec::new();
    for (product, witness) in parsed {
        for source in witness.sources.values() {
            if homes.get(&(source.unit.clone(), source.module.clone())) != Some(source) {
                return Err(CertificationError::Mismatch(
                    "inherited source owner closure",
                ));
            }
        }
        for group in product.groups {
            let (_, _, globals) = witness
                .groups
                .iter()
                .find(|(ordinal, _, _)| *ordinal == group.original_ordinal())
                .ok_or(CertificationError::Mismatch("inherited group witness"))?;
            let imports = group
                .globals()
                .iter()
                .zip(globals)
                .map(|(declaration, selected)| {
                    if let ReceiptImportOwner::Package { unit, module, .. } = &selected.owner {
                        if homes.contains_key(&(unit.clone(), module.clone())) {
                            return Err(CertificationError::Mismatch(
                                "home owner downgraded to package",
                            ));
                        }
                    }
                    let import = validate_global_witness(
                        declaration,
                        group.definitions().signatures(),
                        selected,
                    )?;
                    resolve_receipt_owner(import, &sources, &witness.packages)
                })
                .collect::<CertResult<Vec<_>>>()?;
            if shared.contains(&(witness.owner.unit.clone(), witness.owner.module.clone())) {
                let current = current_groups
                    .iter()
                    .find(|current| current.owner() == &witness.owner && current.group() == &group)
                    .ok_or(CertificationError::Mismatch("shared original home groups"))?;
                if current.imports() != imports {
                    return Err(CertificationError::Mismatch("shared home import ownership"));
                }
                continue;
            }
            result.push(PendingCertifiedGroup {
                origin: ProductOrigin::Cached,
                owner: witness.owner.clone(),
                group,
                imports,
            });
        }
    }
    Ok(result)
}

/// Recheck original/fresh sidecars and dependency bytes against a worker
/// receipt. A candidate is never admitted merely because its name or hash
/// appears in the receipt: every original global and source edge is checked.
pub(crate) fn certify_products(
    candidates: Option<&CandidateSet>,
    receipt: &CertifiedReceipt,
    fresh_products: &[RawModuleProduct],
    fresh_product_bytes: &[u8],
    fresh_package_bundle_bytes: &[u8],
    fresh_evidence_bytes: &[u8],
    fresh_input_path: &Path,
    final_evidence: &DependencyEvidence,
    final_target_source: &str,
    endpoint_identity: &[u8],
    include: &[PathBuf],
    exact: Option<&crate::declaration_context::ExactProductAdmission<'_>>,
) -> CertResult<CertifiedProducts> {
    let normalized = match exact {
        None => DependencyEvidence::from_worker(
            fresh_evidence_bytes,
            fresh_input_path,
            final_target_source,
        )
        .ok_or(CertificationError::StaleEvidence)?,
        Some(admission) => {
            admission
                .source
                .validate_ineligible_evidence(fresh_evidence_bytes)
                .map_err(|_| CertificationError::Mismatch("exact fresh evidence"))?;
            admission.source.evidence.clone()
        }
    };
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
    let fresh_package_imports =
        crate::module_candidates::split_package_imports(fresh_package_bundle_bytes, &parsed_fresh)
            .ok_or(CertificationError::Mismatch("fresh package import framing"))?;
    for product in &parsed_fresh {
        let key = (product.unit.clone(), product.module.clone());
        let sidecar = fresh_package_imports
            .get(&key)
            .ok_or(CertificationError::Mismatch("fresh package imports"))?;
        let iface_sha: [u8; 32] = sha(&product.interface);
        crate::recovery_artifacts::validate_package_imports(
            sidecar,
            &product.unit,
            &product.module,
            &iface_sha,
            Path::new("module-package-imports.cbor"),
        )
        .map_err(|_| CertificationError::Mismatch("fresh package import witness"))?;
    }
    let mut seen_modules = BTreeSet::new();
    let mut fresh_modules = BTreeSet::new();
    let mut groups = Vec::new();
    let mut module_bytes = Vec::with_capacity(receipt.modules.len());
    for accepted in &receipt.modules {
        let key = (accepted.unit.clone(), accepted.module.clone());
        if !seen_modules.insert(key.clone()) {
            return Err(CertificationError::Mismatch("duplicate receipt module"));
        }
        let (product, receipt_bytes, product_bytes, package_bytes, evidence, source_sha, version) =
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
                    let version = if let Some(admission) = exact {
                        let mut digest = Sha256::new();
                        for field in [
                            b"tidepool-exact-source-home-v1".as_slice(),
                            endpoint_identity,
                            admission.request.semantic_sha256.as_slice(),
                            key.0.as_bytes(),
                            key.1.as_bytes(),
                            source_sha.as_slice(),
                            product.interface.as_slice(),
                            module_bytes.as_slice(),
                            fresh_package_imports
                                .get(&key)
                                .ok_or(CertificationError::Mismatch("fresh package imports"))?
                                .as_slice(),
                        ] {
                            digest.update((field.len() as u64).to_be_bytes());
                            digest.update(field);
                        }
                        ModuleVersion(digest.finalize().into())
                    } else {
                        fresh_module_version(
                            endpoint_identity,
                            include,
                            &source_sha,
                            &product.interface,
                            module_bytes,
                            fresh_package_imports
                                .get(&key)
                                .ok_or(CertificationError::Mismatch("fresh package imports"))?,
                        )?
                    };
                    (
                        product,
                        fresh_product_bytes,
                        module_bytes.as_slice(),
                        fresh_package_imports
                            .get(&key)
                            .ok_or(CertificationError::Mismatch("fresh package imports"))?
                            .as_slice(),
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
                        || hex(&sha(&read_bounded(&bundle.package_imports_path, 4 << 20)?))
                            != bundle.package_imports_sha256
                    {
                        return Err(CertificationError::StaleEvidence);
                    }
                    let sidecar = read_bounded(&bundle.package_imports_path, 4 << 20)?;
                    if sidecar != bundle.package_imports_bytes {
                        return Err(CertificationError::StaleEvidence);
                    }
                    crate::recovery_artifacts::validate_package_imports(
                        &sidecar,
                        &key.0,
                        &key.1,
                        &accepted.skinny_iface_sha256,
                        &bundle.package_imports_path,
                    )
                    .map_err(|_| CertificationError::StaleEvidence)?;
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
                        bundle.package_imports_bytes.as_slice(),
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
            product_sha256: sha(product_bytes),
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
        check_direct_package_agreement(package_bytes, &owner, &receipt.packages)?;
        if product.groups.len() != accepted.groups.len() {
            return Err(CertificationError::Mismatch("original group count"));
        }
        module_bytes.push((
            owner.clone(),
            source_sha,
            product.interface.clone(),
            product_bytes.to_vec(),
            package_bytes.to_vec(),
        ));
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
    let mut source_groups = match exact {
        Some(admission) => certified_source_map(&admission.request.groups)?,
        None => SourceGroupMap::new(),
    };
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
    let mut groups: Vec<PendingCertifiedGroup> = groups
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
        .collect::<CertResult<_>>()?;
    let mut recovery_products: Vec<_> = module_bytes
        .into_iter()
        .map(
            |(owner, source_sha, interface, product_bytes, package_bytes)| {
                let original: Vec<_> = groups
                    .iter()
                    .filter(|group| group.owner() == &owner)
                    .cloned()
                    .collect();
                let certification =
                    encode_home_certification(&owner, &original, &receipt.packages)?;
                Ok(
                    crate::recovery_artifacts::CertifiedRecoveryProduct::from_certification(
                        owner,
                        interface,
                        product_bytes,
                        package_bytes,
                        certification,
                    )
                    .with_source_sha256(source_sha),
                )
            },
        )
        .collect::<CertResult<Vec<_>>>()?;
    if let Some(admission) = exact {
        groups.extend(admission.request.groups.iter().cloned());
        recovery_products.extend(
            admission
                .request
                .context
                .recovery_products()
                .iter()
                .cloned(),
        );
    }
    Ok(CertifiedProducts {
        groups,
        recovery_products,
    })
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
    use tidepool_repr::execution_schema::DecodeLimits;

    #[test]
    fn target_package_interfaces_remain_bound_to_exact_target_and_selected_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Package.hi");
        std::fs::write(&path, b"selected interface").unwrap();
        let packages = BTreeMap::from([(
            ("fixture-unit".into(), "Package".into()),
            PackageInterfaceWitness {
                selected_path: path.clone(),
                sha256: sha(b"selected interface"),
            },
        )]);
        let target = std::sync::Arc::new(testing::prepare(testing::wire_program()).unwrap());
        let retained = certify_target_package_interfaces(&target, &packages).unwrap();
        assert!(std::sync::Arc::ptr_eq(
            retained.target.as_ref().unwrap(),
            &target
        ));
        assert!(retained.matches_target(&target));
        assert_eq!(
            retained.interface_digest("fixture-unit", "Package"),
            Some(sha(b"selected interface"))
        );
        assert_eq!(retained.interface_digest("other-unit", "Package"), None);
        let mut other = testing::wire_program();
        let tidepool_repr::execution_schema::Group::NonRecursive(binding) = &mut other.bindings[0]
        else {
            unreachable!()
        };
        binding.identity.occurrence.push_str("Changed");
        assert!(!retained.matches_target(&testing::prepare(other).unwrap()));
        assert!(!CertifiedTargetPackageInterfaces::default().matches_target(&target));
        std::fs::write(&path, b"changed interface").unwrap();
        assert!(matches!(
            certify_target_package_interfaces(&target, &packages),
            Err(CertificationError::StaleEvidence)
        ));
    }

    fn inherited_owner(module: &str) -> CachedHomeOwner {
        CachedHomeOwner {
            unit: "fixture".into(),
            module: module.into(),
            module_version: ModuleVersion([7; 32]),
            skinny_iface_sha256: sha(&[0x42]),
            product_sha256: [8; 32],
        }
    }

    fn inherited_group(
        owner: &CachedHomeOwner,
        import: PendingImportOwner,
    ) -> PendingCertifiedGroup {
        let mut wire = testing::wire_program();
        let tidepool_repr::execution_schema::Group::NonRecursive(top) = &mut wire.bindings[0]
        else {
            unreachable!()
        };
        top.identity = testing::identity(&owner.module, "entry");
        let (identity, required_generation) = match &import {
            PendingImportOwner::Source { binder, .. }
            | PendingImportOwner::Package { binder, .. } => (binder.clone(), None),
            PendingImportOwner::Retained {
                identity,
                generation,
            } => (identity.clone(), Some(*generation)),
        };
        wire.globals.push(GlobalDecl {
            identity,
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation,
        });
        PendingCertifiedGroup {
            owner: owner.clone(),
            origin: ProductOrigin::Cached,
            group: testing::projected_group(wire, 7).unwrap(),
            imports: vec![import],
        }
    }

    fn inherited_parsed(
        group: &PendingCertifiedGroup,
        packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
    ) -> (RawModuleProduct, HomeCertification) {
        let bytes = encode_home_certification(group.owner(), std::slice::from_ref(group), packages)
            .unwrap();
        (
            RawModuleProduct {
                unit: group.owner.unit.clone(),
                module: group.owner.module.clone(),
                interface: vec![0x42],
                groups: vec![group.group.clone()],
            },
            decode_home_witness(&bytes).unwrap(),
        )
    }

    #[test]
    fn inherited_original_cycle_requires_full_exact_source_closure() {
        let a = inherited_owner("A");
        let b = inherited_owner("B");
        let ga = inherited_group(
            &a,
            PendingImportOwner::Source {
                owner: b.clone(),
                original_ordinal: 7,
                binder: testing::identity("B", "entry"),
            },
        );
        let gb = inherited_group(
            &b,
            PendingImportOwner::Source {
                owner: a,
                original_ordinal: 7,
                binder: testing::identity("A", "entry"),
            },
        );
        let packages = BTreeMap::new();
        assert_eq!(
            certify_inherited_inventory(
                vec![
                    inherited_parsed(&ga, &packages),
                    inherited_parsed(&gb, &packages)
                ],
                std::slice::from_ref(&ga)
            )
            .unwrap(),
            vec![gb.clone()]
        );
        let mut conflicting_current = ga.clone();
        if let PendingImportOwner::Source { owner, .. } = &mut conflicting_current.imports[0] {
            owner.skinny_iface_sha256 = [91; 32];
        }
        assert!(matches!(
            certify_inherited_inventory(
                vec![
                    inherited_parsed(&ga, &packages),
                    inherited_parsed(&gb, &packages)
                ],
                &[conflicting_current]
            ),
            Err(CertificationError::Mismatch("shared home import ownership"))
        ));
        assert_eq!(
            certify_inherited_inventory(
                vec![
                    inherited_parsed(&ga, &packages),
                    inherited_parsed(&gb, &packages)
                ],
                &[]
            )
            .unwrap(),
            vec![ga.clone(), gb.clone()]
        );
        assert!(matches!(
            certify_inherited_inventory(vec![inherited_parsed(&ga, &packages)], &[]),
            Err(CertificationError::Mismatch(
                "inherited source owner closure"
            ))
        ));
        // A freshly certified current module can complete the inherited cycle.
        let mut current = gb.clone();
        current.origin = ProductOrigin::Fresh;
        assert_eq!(
            certify_inherited_inventory(vec![inherited_parsed(&ga, &packages)], &[current])
                .unwrap(),
            vec![ga.clone()]
        );
        let (product, mut witness) = inherited_parsed(&ga, &packages);
        witness
            .sources
            .get_mut(&(b.unit.clone(), b.module.clone()))
            .unwrap()
            .product_sha256 = [99; 32];
        assert!(matches!(
            certify_inherited_inventory(
                vec![(product, witness), inherited_parsed(&gb, &packages)],
                &[]
            ),
            Err(CertificationError::Mismatch(
                "inherited source owner closure"
            ))
        ));
        let (product, mut witness) = inherited_parsed(&ga, &packages);
        if let ReceiptImportOwner::Source {
            original_ordinal, ..
        } = &mut witness.groups[0].2[0].owner
        {
            *original_ordinal = 8;
        }
        assert!(matches!(
            certify_inherited_inventory(
                vec![(product, witness), inherited_parsed(&gb, &packages)],
                &[]
            ),
            Err(CertificationError::Mismatch("source binder/group closure"))
        ));
        let (product, mut witness) = inherited_parsed(&ga, &packages);
        witness.groups[0].1[0].occurrence = "rebound".into();
        assert!(matches!(
            certify_inherited_inventory(
                vec![(product, witness), inherited_parsed(&gb, &packages)],
                &[]
            ),
            Err(CertificationError::Mismatch(
                "inherited original group/globals"
            ))
        ));
    }

    #[test]
    fn inherited_owner_seal_preserves_retained_identity_and_refuses_package_downgrade() {
        let a = inherited_owner("A");
        let retained = inherited_group(
            &a,
            PendingImportOwner::Retained {
                identity: testing::identity("Val.G9", "old"),
                generation: 9,
            },
        );
        assert_eq!(
            certify_inherited_inventory(vec![inherited_parsed(&retained, &BTreeMap::new())], &[])
                .unwrap(),
            vec![retained]
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("B.hi");
        std::fs::write(&path, [0x42]).unwrap();
        let b = inherited_owner("B");
        let packages = BTreeMap::from([(
            ("fixture".into(), "B".into()),
            PackageInterfaceWitness {
                selected_path: path.clone(),
                sha256: sha(&[0x42]),
            },
        )]);
        let fake_package = inherited_group(
            &a,
            PendingImportOwner::Package {
                unit: "fixture".into(),
                module: "B".into(),
                binder: testing::identity("B", "entry"),
                interface_digest: sha(&[0x42]),
            },
        );
        let gb = inherited_group(
            &b,
            PendingImportOwner::Retained {
                identity: testing::identity("Val.G9", "old"),
                generation: 9,
            },
        );
        assert!(matches!(
            certify_inherited_inventory(
                vec![
                    inherited_parsed(&fake_package, &packages),
                    inherited_parsed(&gb, &BTreeMap::new())
                ],
                &[]
            ),
            Err(CertificationError::Mismatch(
                "home owner downgraded to package"
            ))
        ));
        let sealed = encode_home_certification(&a, &[fake_package.clone()], &packages).unwrap();
        let mut target = testing::wire_program();
        target.globals = fake_package.group.globals().to_vec();
        let target = testing::prepare(target).unwrap();
        let accepted = decode_home_witness(&sealed).unwrap().groups.remove(0).2;
        assert!(matches!(
            certify_target_owners(&target, &accepted, &[gb], &packages),
            Err(CertificationError::Mismatch(
                "home owner downgraded to package"
            ))
        ));
        std::fs::write(path, [0x43]).unwrap();
        assert!(matches!(
            validate_home_certification(&sealed, &a),
            Err(CertificationError::StaleEvidence)
        ));
    }

    #[test]
    fn inherited_empty_product_requires_canonical_exact_seal_and_bytes() {
        let bytes = sidecar();
        let owner = CachedHomeOwner {
            unit: "main".into(),
            module: "Fresh".into(),
            module_version: ModuleVersion([7; 32]),
            skinny_iface_sha256: sha(&[0x42]),
            product_sha256: sha(&bytes),
        };
        let seal = encode_home_certification(&owner, &[], &BTreeMap::new()).unwrap();
        let source = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();
        let interface = source.path().join("Fresh.hi");
        let product = source.path().join("Fresh.cbor");
        std::fs::write(&interface, [0x42]).unwrap();
        std::fs::write(&product, bytes).unwrap();
        std::fs::write(interface.with_extension("hi.owners"), &seal).unwrap();
        let package_seal = value_array([
            value_text("TPPKGROOTS"),
            value_text("1"),
            value_array([
                value_text("main"),
                value_text("Fresh"),
                value_text(hex(&owner.skinny_iface_sha256)),
            ]),
            value_array([]),
        ]);
        let mut packages = Vec::new();
        ciborium::ser::into_writer(&package_seal, &mut packages).unwrap();
        std::fs::write(interface.with_extension("hi.packages"), packages).unwrap();
        let refs = crate::recovery_artifacts::materialize_recovery_closure(
            run.path(),
            [1; 32],
            &[crate::recovery_artifacts::RecoveryArtifactInput {
                owner: &owner,
                interface_source: &interface,
                product_source: &product,
            }],
        )
        .unwrap();
        let mut artifact =
            crate::recovery_artifacts::verify_materialized_ref(run.path(), &refs[0]).unwrap();
        assert!(certify_inherited_products(
            &[InheritedProductInput {
                artifact: &artifact
            }],
            &[]
        )
        .unwrap()
        .is_empty());
        assert!(certify_inherited_products(
            &[
                InheritedProductInput {
                    artifact: &artifact
                },
                InheritedProductInput {
                    artifact: &artifact
                },
            ],
            &[]
        )
        .is_err());
        let mut trailing = seal.clone();
        trailing.push(0);
        assert!(validate_home_certification(&trailing, &owner).is_err());
        assert!(validate_home_certification(&vec![0; RECEIPT_LIMIT + 1], &owner).is_err());
        let mut wrong_owner = owner.clone();
        wrong_owner.module_version = ModuleVersion([9; 32]);
        let wrong_seal = encode_home_certification(&wrong_owner, &[], &BTreeMap::new()).unwrap();
        artifact.certification_bytes = wrong_seal;
        assert!(certify_inherited_products(
            &[InheritedProductInput {
                artifact: &artifact
            }],
            &[]
        )
        .is_err());
        artifact.certification_bytes = seal.clone();
        artifact.reference.certification_sha256 = [0; 32];
        assert!(certify_inherited_products(
            &[InheritedProductInput {
                artifact: &artifact
            }],
            &[]
        )
        .is_err());
        artifact.reference.certification_sha256 = sha(&seal);
        let mut witness = decode_home_witness(&seal).unwrap();
        witness.owner.skinny_iface_sha256 = [0; 32];
        artifact.certification_bytes = encode_home_witness(&witness).unwrap();
        artifact.reference.certification_sha256 = sha(&artifact.certification_bytes);
        assert!(certify_inherited_products(
            &[InheritedProductInput {
                artifact: &artifact
            }],
            &[]
        )
        .is_err());
    }

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

    fn empty_package_bundle() -> Vec<u8> {
        let roots = Value::Array(vec![
            Value::Text("TPPKGROOTS".into()),
            Value::Text("1".into()),
            Value::Array(vec![
                Value::Text("main".into()),
                Value::Text("Fresh".into()),
                Value::Text(hex(&sha(&[0x42]))),
            ]),
            Value::Array(vec![]),
        ]);
        let mut sidecar = Vec::new();
        ciborium::ser::into_writer(&roots, &mut sidecar).unwrap();
        let bundle = Value::Array(vec![
            Value::Text("TPPKGBUNDLES".into()),
            Value::Integer(1.into()),
            Value::Array(vec![Value::Array(vec![
                Value::Text("main".into()),
                Value::Text("Fresh".into()),
                Value::Bytes(sidecar),
            ])]),
        ]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&bundle, &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn direct_package_root_and_receipt_must_select_same_path() {
        let root = tempfile::tempdir().unwrap();
        let selected = root.path().join("selected.hi");
        let substituted = root.path().join("substituted.hi");
        std::fs::write(&selected, b"same interface").unwrap();
        std::fs::write(&substituted, b"same interface").unwrap();
        let owner = CachedHomeOwner {
            unit: "main".into(),
            module: "Fresh".into(),
            module_version: ModuleVersion([7; 32]),
            skinny_iface_sha256: sha(&[0x42]),
            product_sha256: [8; 32],
        };
        let digest = sha(b"same interface");
        let sidecar = value_array([
            value_text("TPPKGROOTS"),
            value_text("1"),
            value_array([
                value_text("main"),
                value_text("Fresh"),
                value_text(hex(&owner.skinny_iface_sha256)),
            ]),
            value_array([value_array([
                value_text("base-unit"),
                value_text("Data.Base"),
                value_text(selected.display().to_string()),
                value_text(hex(&digest)),
            ])]),
        ]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&sidecar, &mut bytes).unwrap();
        let packages = BTreeMap::from([(
            ("base-unit".into(), "Data.Base".into()),
            PackageInterfaceWitness {
                selected_path: substituted,
                sha256: digest,
            },
        )]);
        assert!(matches!(
            check_direct_package_agreement(&bytes, &owner, &packages),
            Err(CertificationError::Mismatch(
                "direct package/receipt selection"
            ))
        ));
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
        let package_bytes = empty_package_bundle();
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
        let certified = certify_products(
            None,
            &CertifiedReceipt {
                modules: vec![accepted.clone()],
                targets: BTreeMap::new(),
                packages: BTreeMap::new(),
            },
            &parsed,
            &bytes,
            &package_bytes,
            &raw_evidence,
            &input,
            &evidence,
            source,
            b"producer",
            &[],
            None,
        )
        .unwrap();
        assert!(certified.groups.is_empty());
        assert_eq!(certified.recovery_products.len(), 1);
        let refs = crate::recovery_artifacts::materialize_certified_products(
            directory.path(),
            [3; 32],
            &certified.recovery_products,
        )
        .unwrap();
        let verified =
            crate::recovery_artifacts::verify_materialized_ref(directory.path(), &refs[0]).unwrap();
        assert_eq!(verified.product_bytes, bytes);
        assert!(!verified.package_imports_bytes.is_empty());
        assert_eq!(
            verified.reference.certification_path.extension().unwrap(),
            "owners"
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
                &package_bytes,
                &raw_evidence,
                &input,
                &substituted,
                source,
                b"producer",
                &[],
                None,
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
                &package_bytes,
                &raw_evidence,
                &input,
                &evidence,
                source,
                b"producer",
                &[],
                None,
            ),
            Err(CertificationError::Mismatch(
                "product/iface/source/evidence digest"
            ))
        ));
        let mut changed = accepted;
        changed.dependency_witness_sha256 = [7; 32];
        assert!(certify_products(
            None,
            &CertifiedReceipt {
                modules: vec![changed],
                targets: BTreeMap::new(),
                packages: BTreeMap::new()
            },
            &parsed,
            &bytes,
            &package_bytes,
            &raw_evidence,
            &input,
            &evidence,
            source,
            b"producer",
            &[],
            None,
        )
        .is_err());
    }

    #[test]
    fn receipt_dictionary_preserves_legacy_facts_and_refuses_invalid_references() {
        let mut legacy = empty_legacy_receipt();
        let global = dictionary_test_global();
        let Value::Array(header) = &mut legacy else {
            unreachable!()
        };
        header[3] = value_array([value_array([
            value_text("target"),
            value_array([value_global(&global), value_global(&global)]),
        ])]);
        let compact = dictionary_receipt(&legacy);
        let encoded = receipt_bytes(&compact);
        assert_eq!(
            decode_receipt(&encoded).unwrap(),
            decode_receipt(&receipt_bytes(&legacy)).unwrap()
        );
        let mut trailing = encoded;
        trailing.push(0);
        assert!(matches!(
            decode_receipt(&trailing),
            Err(CertificationError::Receipt("trailing bytes"))
        ));
        for reference in [
            Value::Integer(1.into()),
            Value::Integer((-1).into()),
            Value::Null,
        ] {
            let mut altered = compact.clone();
            let Value::Array(header) = &mut altered else {
                unreachable!()
            };
            let Value::Array(targets) = &mut header[3] else {
                unreachable!()
            };
            let Value::Array(target) = &mut targets[0] else {
                unreachable!()
            };
            target[1] = value_array([reference]);
            assert!(decode_receipt(&receipt_bytes(&altered)).is_err());
        }
        let mut duplicate = compact.clone();
        let Value::Array(header) = &mut duplicate else {
            unreachable!()
        };
        let Value::Array(rows) = &mut header[5] else {
            unreachable!()
        };
        rows.push(rows[0].clone());
        assert!(matches!(
            decode_receipt(&receipt_bytes(&duplicate)),
            Err(CertificationError::Receipt(
                "duplicate global dictionary row"
            ))
        ));
        let mut unreferenced = compact;
        let Value::Array(header) = &mut unreferenced else {
            unreachable!()
        };
        header[3] = value_array([]);
        assert!(matches!(
            decode_receipt(&receipt_bytes(&unreferenced)),
            Err(CertificationError::Receipt(
                "unreferenced global dictionary row"
            ))
        ));
    }

    #[test]
    fn receipt_dictionary_bounds_expanded_witnesses() {
        let global = dictionary_test_global();
        let mut dictionary =
            GlobalDictionary::decode(&value_array([value_global(&global)])).unwrap();
        let indices = vec![Value::Integer(0.into()); GLOBAL_REFERENCE_LIMIT];
        assert_eq!(
            dictionary.resolve(&indices).unwrap().len(),
            GLOBAL_REFERENCE_LIMIT
        );
        assert!(matches!(
            dictionary.resolve(&[Value::Integer(0.into())]),
            Err(CertificationError::Receipt("expanded global count"))
        ));
        let mut large = global;
        large.identity.occurrence = "large".repeat(32 * 1024);
        let mut dictionary =
            GlobalDictionary::decode(&value_array([value_global(&large)])).unwrap();
        let row_bytes = dictionary.rows[0].1;
        let indices = vec![Value::Integer(0.into()); EXPANDED_GLOBAL_BYTES_LIMIT / row_bytes + 1];
        assert!(matches!(
            dictionary.resolve(&indices),
            Err(CertificationError::Receipt("expanded global bytes"))
        ));
    }

    #[test]
    #[ignore = "requires the retained oversized production tools receipt"]
    fn receipt_dictionary_preserves_retained_production_facts() {
        let path = std::env::var_os("TIDEPOOL_RETAINED_PRODUCT_RECEIPT")
            .expect("explicit retained production receipt path");
        let bytes = std::fs::read(path).unwrap();
        assert!(
            bytes.len() > RECEIPT_LIMIT,
            "must exercise the actual size refusal"
        );
        let legacy: Value = ciborium::de::from_reader(bytes.as_slice()).unwrap();
        let compact = dictionary_receipt(&legacy);
        let encoded = receipt_bytes(&compact);
        let expected = decode_receipt_value(&legacy).unwrap();
        let admitted = decode_receipt(&encoded).unwrap();
        assert_eq!(
            admitted, expected,
            "compression changed ordered full owner/contract facts"
        );
        assert!(encoded.len() <= RECEIPT_LIMIT);
        eprintln!(
            "retained-product-receipt legacy_bytes={} dictionary_bytes={} modules={} global_references={}",
            bytes.len(), encoded.len(), admitted.modules.len(),
            admitted.modules.iter().flat_map(|module| &module.groups).map(|group| group.globals.len()).sum::<usize>()
                + admitted.targets.values().map(Vec::len).sum::<usize>()
        );
    }

    fn empty_legacy_receipt() -> Value {
        value_array([
            value_text("TPCERT"),
            Value::Integer(2.into()),
            value_array([]),
            value_array([]),
            value_array([]),
        ])
    }

    fn dictionary_test_global() -> AcceptedGlobal {
        let identity = testing::identity("Support", "value");
        AcceptedGlobal {
            owner: ReceiptImportOwner::Retained {
                identity: identity.clone(),
                generation: 7,
            },
            identity,
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
        }
    }

    fn receipt_bytes(value: &Value) -> Vec<u8> {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(value, &mut bytes).unwrap();
        bytes
    }

    fn dictionary_receipt(legacy: &Value) -> Value {
        let mut compact = legacy.clone();
        let Value::Array(header) = &mut compact else {
            panic!("legacy receipt tuple")
        };
        assert_eq!(header[1], Value::Integer(2.into()));
        let mut dictionary = BTreeMap::new();
        let mut rows = |value: &Value| {
            for row in array(value).unwrap() {
                dictionary.insert(receipt_bytes(row), row.clone());
            }
        };
        for module in array(&header[2]).unwrap() {
            for group in array(&array(module).unwrap()[8]).unwrap() {
                rows(&array(group).unwrap()[1]);
            }
        }
        for target in array(&header[3]).unwrap() {
            rows(&array(target).unwrap()[1]);
        }
        let indexed = dictionary
            .into_iter()
            .enumerate()
            .map(|(index, (bytes, row))| (bytes, (index, row)))
            .collect::<BTreeMap<_, _>>();
        let rewrite = |value: &mut Value| {
            let Value::Array(rows) = value else {
                panic!("global rows")
            };
            for row in rows {
                *row = Value::Integer((indexed[&receipt_bytes(row)].0 as u64).into());
            }
        };
        let Value::Array(modules) = &mut header[2] else {
            panic!("modules")
        };
        for module in modules {
            let Value::Array(module) = module else {
                panic!("module")
            };
            let Value::Array(groups) = &mut module[8] else {
                panic!("groups")
            };
            for group in groups {
                let Value::Array(group) = group else {
                    panic!("group")
                };
                rewrite(&mut group[1]);
            }
        }
        let Value::Array(targets) = &mut header[3] else {
            panic!("targets")
        };
        for target in targets {
            let Value::Array(target) = target else {
                panic!("target")
            };
            rewrite(&mut target[1]);
        }
        header[1] = Value::Integer(3.into());
        header.push(value_array(indexed.into_values().map(|(_, row)| row)));
        compact
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
