//! Canonical module interfaces issued by one completed compiler finalization.
//! Core is retained as compiler input. Native promotion independently certifies
//! newly prepared products against the original canonical admission.

use super::*;
use std::path::Component;

pub(crate) const FINALIZATION_PROFILE: &str = "tidepool-ghc-finalized-module-v1";
pub(crate) const FINALIZATION_ENVELOPE_PROFILE: &str = "tidepool-ghc-finalized-module-v2";
const CANONICAL_CERTIFICATE_LIMIT: usize = 4 << 20;
const CORE_LIMIT: u64 = 32 << 20;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CanonicalSourceImport {
    pub(crate) qualifier: crate::cache::ImportQualifier,
    pub(crate) module: String,
    pub(crate) boot: bool,
    pub(crate) home_unit: Option<String>,
}

impl CanonicalSourceImport {
    pub(crate) fn key(&self) -> (String, String, bool, Option<String>) {
        (
            String::from(self.qualifier.clone()),
            self.module.clone(),
            self.boot,
            self.home_unit.clone(),
        )
    }
    fn encode(&self) -> Value {
        value_array([
            value_text(String::from(self.qualifier.clone())),
            value_text(&self.module),
            Value::Bool(self.boot),
            self.home_unit.as_ref().map_or(Value::Null, value_text),
        ])
    }
    fn decode(value: &Value, homes: &BTreeSet<String>) -> CertResult<Self> {
        let row = sized(value, 4)?;
        let qualifier = crate::cache::ImportQualifier::try_from(string(&row[0])?.to_owned())
            .map_err(|_| CertificationError::Receipt("canonical source import qualifier"))?;
        let module = string(&row[1])?.to_owned();
        let boot = match &row[2] {
            Value::Bool(boot) => *boot,
            _ => return Err(CertificationError::Receipt("canonical source import boot")),
        };
        let home_unit = match &row[3] {
            Value::Null => None,
            _ => Some(string(&row[3])?.to_owned()),
        };
        use crate::cache::ImportQualifier;
        let valid_classification = match (&qualifier, &home_unit) {
            (ImportQualifier::Unqualified | ImportQualifier::OtherUnit(_), None) => true,
            (ImportQualifier::Unqualified, Some(unit)) => homes.contains(unit),
            (ImportQualifier::ThisUnit(wanted), Some(unit)) => {
                wanted == unit && homes.contains(unit)
            }
            _ => false,
        };
        if module.is_empty() || !valid_classification {
            return Err(CertificationError::Receipt("canonical source import owner"));
        }
        Ok(Self {
            qualifier,
            module,
            boot,
            home_unit,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CanonicalOrigin {
    SourceOriginal {
        imports: Arc<[CanonicalSourceImport]>,
    },
    NativeAuthoredDeclaration {
        generation: u64,
    },
}

impl CanonicalOrigin {
    fn encode(&self) -> Value {
        match self {
            Self::SourceOriginal { imports } => value_array([
                value_text("source-original"),
                value_array(imports.iter().map(CanonicalSourceImport::encode)),
            ]),
            Self::NativeAuthoredDeclaration { generation } => value_array([
                value_text("native-authored-declaration"),
                Value::Integer((*generation).into()),
            ]),
        }
    }

    fn decode(
        value: &Value,
        owner: &FinalizedModuleReceipt,
        homes: &BTreeSet<String>,
    ) -> CertResult<Self> {
        let row = array(value)?;
        match row {
            [tag, rows] if string(tag)? == "source-original" => {
                let rows = array(rows)?;
                if rows.len() > 4096 {
                    return Err(CertificationError::Receipt("canonical source import bound"));
                }
                let imports = rows
                    .iter()
                    .map(|row| CanonicalSourceImport::decode(row, homes))
                    .collect::<CertResult<Vec<_>>>()?;
                if imports.windows(2).any(|pair| {
                    pair[0].key() >= pair[1].key()
                        || (pair[0].qualifier == pair[1].qualifier
                            && pair[0].module == pair[1].module
                            && pair[0].boot == pair[1].boot)
                }) {
                    return Err(CertificationError::Receipt("canonical source import order"));
                }
                Ok(Self::SourceOriginal {
                    imports: imports.into(),
                })
            }
            [tag, generation] if string(tag)? == "native-authored-declaration" => {
                let generation = number(generation)?;
                if generation == 0
                    || owner.unit != "main"
                    || owner.module
                        != tidepool_repr::SessionModule::lib(tidepool_repr::Generation(generation))
                            .module_name()
                {
                    return Err(CertificationError::Mismatch(
                        "authored declaration origin identity",
                    ));
                }
                Ok(Self::NativeAuthoredDeclaration { generation })
            }
            _ => Err(CertificationError::Receipt("finalized certificate origin")),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedArtifactDescriptor {
    pub relative_path: PathBuf,
    pub sha256: [u8; 32],
    pub bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizedModuleReceipt {
    pub unit: String,
    pub module: String,
    pub source_sha256: [u8; 32],
    pub interface: CapturedArtifactDescriptor,
    pub package_imports: CapturedArtifactDescriptor,
    /// Absence explicitly refuses future source-free preparation.
    pub core: Option<CapturedArtifactDescriptor>,
    pub interface_requirements: BTreeMap<(String, String), [u8; 32]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedValueInterfaceReceipt {
    pub interface: CapturedArtifactDescriptor,
    pub package_imports: CapturedArtifactDescriptor,
    pub interface_requirements: BTreeMap<(String, String), [u8; 32]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizationEnvelope {
    pub profile: String,
    /// Complete compiler home-unit inventory, not a unit inferred from spelling.
    pub home_units: BTreeSet<String>,
    pub modules: BTreeMap<(String, String), FinalizedModuleReceipt>,
    /// Selected thin interfaces issue no source or native row.
    pub value_interfaces: BTreeMap<(String, String), CapturedValueInterfaceReceipt>,
}

impl FinalizationEnvelope {
    fn validate_structure(&self) -> CertResult<()> {
        if self.profile != FINALIZATION_ENVELOPE_PROFILE
            || self.home_units.is_empty()
            || self.home_units.iter().any(String::is_empty)
        {
            return Err(CertificationError::Receipt(
                "finalization inventory bounds/profile",
            ));
        }
        let mut paths = BTreeSet::new();
        for (key, module) in &self.modules {
            if key != &(module.unit.clone(), module.module.clone())
                || !self.home_units.contains(&module.unit)
                || module.module.is_empty()
                || module.source_sha256 == [0; 32]
                || module
                    .interface_requirements
                    .iter()
                    .any(|(required, seal)| {
                        required == key
                            || !self.home_units.contains(&required.0)
                            || required.1.is_empty()
                            || *seal == [0; 32]
                    })
            {
                return Err(CertificationError::Receipt(
                    "finalized original/requirement identity",
                ));
            }
            for (artifact, limit) in [
                (&module.interface, PACKAGE_INTERFACE_LIMIT),
                (&module.package_imports, COMPILER_RECEIPT_BYTES_LIMIT as u64),
            ]
            .into_iter()
            .chain(module.core.iter().map(|core| (core, CORE_LIMIT)))
            {
                if artifact.bytes == 0
                    || artifact.bytes > limit
                    || artifact.sha256 == [0; 32]
                    || artifact.relative_path.as_os_str().is_empty()
                    || !artifact
                        .relative_path
                        .components()
                        .all(|part| matches!(part, Component::Normal(_)))
                    || !paths.insert(artifact.relative_path.clone())
                {
                    return Err(CertificationError::Receipt(
                        "captured artifact seal/path/bounds",
                    ));
                }
            }
        }
        for (key, value) in &self.value_interfaces {
            let selected = tidepool_repr::SessionModule::from_module_name(&key.1);
            if key.0 != "main"
                || !self.home_units.contains(&key.0)
                || self.modules.contains_key(key)
                || !selected.is_some_and(|owner| {
                    owner.kind == tidepool_repr::SessionModuleKind::Val
                        && owner.gen.0 != 0
                        && owner.module_name() == key.1
                })
                || value.interface_requirements.iter().any(|(owner, seal)| {
                    owner == key
                        || !self.home_units.contains(&owner.0)
                        || owner.1.is_empty()
                        || *seal == [0; 32]
                })
            {
                return Err(CertificationError::Receipt(
                    "captured value owner/requirements",
                ));
            }
            for (artifact, limit) in [
                (&value.interface, PACKAGE_INTERFACE_LIMIT),
                (&value.package_imports, COMPILER_RECEIPT_BYTES_LIMIT as u64),
            ] {
                if artifact.bytes == 0
                    || artifact.bytes > limit
                    || artifact.sha256 == [0; 32]
                    || artifact.relative_path.as_os_str().is_empty()
                    || !artifact
                        .relative_path
                        .components()
                        .all(|part| matches!(part, Component::Normal(_)))
                    || !paths.insert(artifact.relative_path.clone())
                {
                    return Err(CertificationError::Receipt(
                        "captured value payload seal/path/bounds",
                    ));
                }
            }
        }
        Ok(())
    }

    pub(super) fn validate_owners(
        &self,
        native: &[CertifiedModuleReceipt],
        packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
    ) -> CertResult<()> {
        if packages
            .keys()
            .any(|(unit, _)| self.home_units.contains(unit))
            || native
                .iter()
                .any(|module| !self.home_units.contains(&module.unit))
        {
            return Err(CertificationError::Receipt(
                "home/package unit classification",
            ));
        }
        for module in native
            .iter()
            .filter(|module| module.origin == ProductOrigin::Fresh)
        {
            let finalized = self
                .modules
                .get(&(module.unit.clone(), module.module.clone()))
                .ok_or(CertificationError::Receipt(
                    "native owner lacks finalization",
                ))?;
            if finalized.interface.sha256 != module.skinny_iface_sha256
                || finalized.source_sha256 != module.source_sha256
                || finalized.interface_requirements != module.interface_requirements
                || finalized.core.is_none()
            {
                return Err(CertificationError::Receipt("native finalization differs"));
            }
        }
        if native.iter().any(|module| {
            module.origin == ProductOrigin::RetainedCore
                && self
                    .modules
                    .contains_key(&(module.unit.clone(), module.module.clone()))
        }) {
            return Err(CertificationError::Receipt(
                "retained core owner has fresh finalization",
            ));
        }
        Ok(())
    }
}

fn path(value: &Value) -> CertResult<PathBuf> {
    let path = PathBuf::from(string(value)?);
    if path.as_os_str().is_empty()
        || !path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(CertificationError::Receipt("captured artifact path"));
    }
    Ok(path)
}

fn descriptor(
    path_value: &Value,
    sha_value: &Value,
    size_value: &Value,
    limit: u64,
) -> CertResult<CapturedArtifactDescriptor> {
    let sha256 = digest(sha_value)?;
    let bytes = number(size_value)?;
    if sha256 == [0; 32] || string(sha_value)? != hex(&sha256) || bytes == 0 || bytes > limit {
        return Err(CertificationError::Receipt("captured artifact seal/size"));
    }
    Ok(CapturedArtifactDescriptor {
        relative_path: path(path_value)?,
        sha256,
        bytes,
    })
}

#[cfg(test)]
pub(super) fn decode_envelope(value: &Value) -> CertResult<FinalizationEnvelope> {
    decode_envelope_with_operation(value, &InventoryOperation::new(Default::default()))
}

pub(super) fn decode_envelope_with_operation(
    value: &Value,
    operation: &InventoryOperation,
) -> CertResult<FinalizationEnvelope> {
    operation.charge_value_copies(value, 3)?;
    let row = sized(value, 4)?;
    let profile = string(&row[0])?.to_owned();
    if profile != FINALIZATION_ENVELOPE_PROFILE {
        return Err(CertificationError::Receipt("finalization profile"));
    }
    let units = array(&row[1])?;
    if units.is_empty() {
        return Err(CertificationError::Receipt("complete home unit count"));
    }
    let mut home_units = BTreeSet::new();
    let mut previous = None;
    for unit in units {
        let unit = string(unit)?.to_owned();
        if unit.is_empty() || previous.as_ref().is_some_and(|old| old >= &unit) {
            return Err(CertificationError::Receipt("canonical home unit inventory"));
        }
        previous = Some(unit.clone());
        home_units.insert(unit);
    }
    let rows = array(&row[2])?;
    let mut modules = BTreeMap::new();
    let mut previous = None;
    let mut paths = BTreeSet::new();
    for value in rows {
        let row = sized(value, 11)?;
        let unit = string(&row[0])?.to_owned();
        let module = string(&row[1])?.to_owned();
        let key = (unit.clone(), module.clone());
        let source_sha256 = digest(&row[2])?;
        if !home_units.contains(&unit)
            || module.is_empty()
            || source_sha256 == [0; 32]
            || string(&row[2])? != hex(&source_sha256)
            || previous.as_ref().is_some_and(|old| old >= &key)
        {
            return Err(CertificationError::Receipt("finalized original identity"));
        }
        previous = Some(key.clone());
        let interface = descriptor(&row[3], &row[4], &row[5], PACKAGE_INTERFACE_LIMIT)?;
        let package_imports = descriptor(
            &row[6],
            &row[7],
            &row[8],
            COMPILER_RECEIPT_BYTES_LIMIT as u64,
        )?;
        let core = match &row[9] {
            Value::Null => None,
            value => {
                let core = sized(value, 3)?;
                Some(descriptor(&core[0], &core[1], &core[2], CORE_LIMIT)?)
            }
        };
        for item in [&interface, &package_imports]
            .into_iter()
            .chain(core.iter())
        {
            if !paths.insert(item.relative_path.clone()) {
                return Err(CertificationError::Receipt("aliased finalization payload"));
            }
            operation.charge(
                usize::try_from(item.bytes)
                    .map_err(|_| CertificationError::Receipt("finalization payload budget"))?,
            )?;
        }
        let interface_requirements = decode_interface_requirements(&row[10])?;
        if interface_requirements.contains_key(&key)
            || interface_requirements
                .keys()
                .any(|(unit, _)| !home_units.contains(unit))
        {
            return Err(CertificationError::Receipt(
                "finalized requirement identity",
            ));
        }
        modules.insert(
            key,
            FinalizedModuleReceipt {
                unit,
                module,
                source_sha256,
                interface,
                package_imports,
                core,
                interface_requirements,
            },
        );
    }
    let rows = array(&row[3])?;
    let mut value_interfaces = BTreeMap::new();
    let mut previous = None;
    for value in rows {
        let row = sized(value, 9)?;
        let key = (string(&row[0])?.to_owned(), string(&row[1])?.to_owned());
        if previous.as_ref().is_some_and(|old| old >= &key) {
            return Err(CertificationError::Receipt("captured value order"));
        }
        previous = Some(key.clone());
        let interface = descriptor(&row[2], &row[3], &row[4], PACKAGE_INTERFACE_LIMIT)?;
        let package_imports = descriptor(
            &row[5],
            &row[6],
            &row[7],
            COMPILER_RECEIPT_BYTES_LIMIT as u64,
        )?;
        operation.charge(
            usize::try_from(interface.bytes)
                .map_err(|_| CertificationError::Receipt("value payload budget"))?,
        )?;
        operation.charge(
            usize::try_from(package_imports.bytes)
                .map_err(|_| CertificationError::Receipt("value payload budget"))?,
        )?;
        value_interfaces.insert(
            key,
            CapturedValueInterfaceReceipt {
                interface,
                package_imports,
                interface_requirements: decode_interface_requirements(&row[8])?,
            },
        );
    }
    let envelope = FinalizationEnvelope {
        profile,
        home_units,
        modules,
        value_interfaces,
    };
    envelope.validate_structure()?;
    Ok(envelope)
}

/// Private construction binds immutable payloads and all canonical certificate
/// facts together. Clones retain these same allocations; no mutation API exists.
#[derive(Clone, Debug)]
pub(crate) struct CertifiedModuleInterface {
    producer_sha256: [u8; 32],
    receipt: FinalizedModuleReceipt,
    home_units: Arc<BTreeSet<String>>,
    interface: Arc<[u8]>,
    package_imports: Arc<[u8]>,
    certificate: Arc<[u8]>,
    core: Option<Arc<[u8]>>,
    origin: CanonicalOrigin,
}

impl PartialEq for CertifiedModuleInterface {
    fn eq(&self, other: &Self) -> bool {
        // Scratch capture paths and declared lengths do not create another
        // durable owner for identical authenticated payloads.
        self.producer_sha256 == other.producer_sha256
            && self.certificate == other.certificate
            && self.interface == other.interface
            && self.package_imports == other.package_imports
            && self.core == other.core
    }
}
impl Eq for CertifiedModuleInterface {}

impl CertifiedModuleInterface {
    pub(crate) fn origin(&self) -> CanonicalOrigin {
        self.origin.clone()
    }
    pub(crate) fn source_imports(&self) -> Option<&[CanonicalSourceImport]> {
        match &self.origin {
            CanonicalOrigin::SourceOriginal { imports } => Some(imports),
            _ => None,
        }
    }
    pub(crate) fn unit(&self) -> &str {
        &self.receipt.unit
    }
    pub(crate) fn module(&self) -> &str {
        &self.receipt.module
    }
    pub(crate) fn home_units(&self) -> &BTreeSet<String> {
        &self.home_units
    }
    pub(crate) fn source_sha256(&self) -> [u8; 32] {
        self.receipt.source_sha256
    }
    pub(crate) fn producer_sha256(&self) -> [u8; 32] {
        self.producer_sha256
    }
    pub(crate) fn interface_anchor(&self) -> Arc<[u8]> {
        Arc::clone(&self.interface)
    }
    pub(crate) fn package_imports_anchor(&self) -> Arc<[u8]> {
        Arc::clone(&self.package_imports)
    }
    pub(crate) fn interface_bytes(&self) -> &[u8] {
        &self.interface
    }
    pub(crate) fn package_imports_bytes(&self) -> &[u8] {
        &self.package_imports
    }
    pub(crate) fn certificate_bytes(&self) -> &[u8] {
        &self.certificate
    }
    pub(crate) fn interface_sha256(&self) -> [u8; 32] {
        self.receipt.interface.sha256
    }
    pub(crate) fn package_imports_sha256(&self) -> [u8; 32] {
        self.receipt.package_imports.sha256
    }
    pub(crate) fn requirements(&self) -> &BTreeMap<(String, String), [u8; 32]> {
        &self.receipt.interface_requirements
    }
    /// A persisted descriptor must name the same authenticated contents.
    /// Capture paths do not determine canonical identity.
    pub(crate) fn matches_recovery_reference(
        &self,
        reference: &crate::recovery_artifacts::RecoveryModuleInterfaceRef,
    ) -> bool {
        let interface = &reference.interface;
        let core_matches = match (&reference.core, &self.receipt.core) {
            (Some(reference), Some(core)) => {
                reference.bytes == core.bytes && reference.sha256 == core.sha256
            }
            (None, None) => true,
            _ => false,
        };
        interface.toolchain_identity_sha256 == self.producer_sha256
            && interface.unit == self.receipt.unit
            && interface.module == self.receipt.module
            && interface.skinny_iface_sha256 == self.receipt.interface.sha256
            && interface.package_imports_sha256 == self.receipt.package_imports.sha256
            && reference.certificate_sha256 == sha(&self.certificate)
            && core_matches
    }

    /// Materialization can retain Core without making it available through an
    /// interface capability. Preparation requires a separate admission owner.
    pub(crate) fn core_bytes(&self) -> Option<&[u8]> {
        self.core.as_deref()
    }

    /// Only an independently admitted defining original can authorize a new
    /// native child. The worker cannot replace its canonical payload or seals.
    pub(super) fn validate_native_promotion(
        &self,
        accepted: &CertifiedModuleReceipt,
        producer: [u8; 32],
        inherited_seals: &BTreeMap<(String, String), [u8; 32]>,
        interface: &[u8],
        package_imports: &[u8],
    ) -> CertResult<()> {
        let (Some(core), Some(descriptor)) = (&self.core, &self.receipt.core) else {
            return Err(CertificationError::Mismatch(
                "retained original core absent",
            ));
        };
        if !matches!(self.origin, CanonicalOrigin::SourceOriginal { .. })
            || accepted.origin != ProductOrigin::RetainedCore
            || accepted.module_version.is_some()
            || self.producer_sha256 != producer
            || self.receipt.unit != accepted.unit
            || self.receipt.module != accepted.module
            || self.receipt.source_sha256 != accepted.source_sha256
            || self.receipt.interface.sha256 != accepted.skinny_iface_sha256
            || self.receipt.interface_requirements != accepted.interface_requirements
            || sha(&self.certificate) != accepted.dependency_witness_sha256
            || self.interface.as_ref() != interface
            || self.package_imports.as_ref() != package_imports
            || core.len() as u64 != descriptor.bytes
            || sha(core) != descriptor.sha256
        {
            return Err(CertificationError::Mismatch(
                "retained original promotion identity",
            ));
        }
        for (required, seal) in &self.receipt.interface_requirements {
            if inherited_seals.get(required) != Some(seal) {
                return Err(CertificationError::FinalizedInterfaceRequirement {
                    unit: self.receipt.unit.clone(),
                    module: self.receipt.module.clone(),
                    required_unit: required.0.clone(),
                    required_module: required.1.clone(),
                    expected_sha256: hex(seal),
                    selected_sha256: inherited_seals.get(required).map(|seal| hex(seal)),
                });
            }
        }
        Ok(())
    }
}

fn capture(
    root: &Path,
    descriptor: &CapturedArtifactDescriptor,
    limit: u64,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<Vec<u8>> {
    crate::recovery_artifacts::capture_module_payload(
        root,
        &descriptor.relative_path,
        &descriptor.sha256,
        Some(descriptor.bytes),
        limit,
        validation,
    )
    .map_err(CertificationError::CapturedModulePayload)
}

#[cfg(test)]
pub(super) fn canonical_certificate(
    producer: [u8; 32],
    envelope: &FinalizationEnvelope,
    module: &FinalizedModuleReceipt,
    origin: &CanonicalOrigin,
) -> CertResult<Vec<u8>> {
    canonical_certificate_with_operation(
        producer,
        envelope,
        module,
        origin,
        &InventoryOperation::new(Default::default()),
    )
}
fn canonical_certificate_with_operation(
    producer: [u8; 32],
    envelope: &FinalizationEnvelope,
    module: &FinalizedModuleReceipt,
    origin: &CanonicalOrigin,
    operation: &InventoryOperation,
) -> CertResult<Vec<u8>> {
    operation.reserve::<Value>(48)?;
    operation.charge(
        module
            .unit
            .len()
            .checked_add(module.module.len())
            .and_then(|size| size.checked_add(512))
            .ok_or(CertificationError::Receipt("canonical owner size"))?,
    )?;
    for unit in &envelope.home_units {
        operation.reserve::<Value>(2)?;
        operation.charge(unit.len())?;
    }
    for (unit, name) in module.interface_requirements.keys() {
        operation.reserve::<Value>(8)?;
        operation.charge(
            unit.len()
                .checked_add(name.len())
                .and_then(|size| size.checked_add(64))
                .ok_or(CertificationError::Receipt("canonical requirement size"))?,
        )?;
    }
    if let CanonicalOrigin::SourceOriginal { imports } = origin {
        for import in imports.iter() {
            operation.reserve::<Value>(10)?;
            operation.charge(import.module.len())?;
            if let Some(unit) = &import.home_unit {
                operation.charge(unit.len())?;
            }
            match &import.qualifier {
                crate::cache::ImportQualifier::ThisUnit(unit)
                | crate::cache::ImportQualifier::OtherUnit(unit) => operation.charge(unit.len())?,
                _ => operation.charge(16)?,
            }
        }
    }
    // Scratch paths and descriptor sizes are not durable semantic identity.
    let value = value_array([
        value_text("TPFINALMODULE"),
        Value::Integer(3.into()),
        value_text(FINALIZATION_PROFILE),
        value_text(hex(&producer)),
        value_array(envelope.home_units.iter().map(value_text)),
        value_text(&module.unit),
        value_text(&module.module),
        value_text(hex(&module.source_sha256)),
        value_text(hex(&module.interface.sha256)),
        value_text(hex(&module.package_imports.sha256)),
        module
            .core
            .as_ref()
            .map_or(Value::Null, |core| value_text(hex(&core.sha256))),
        encode_interface_requirements(&module.interface_requirements),
        origin.encode(),
    ]);
    encode_value_with_operation(
        &value,
        CertificationFormat::CanonicalModuleCertificate,
        CANONICAL_CERTIFICATE_LIMIT,
        operation,
    )
}

pub(super) fn issue_value_interfaces(
    envelope: &FinalizationEnvelope,
    root: &Path,
    producer: [u8; 32],
    selected: &[tidepool_repr::SessionModule],
    produced: Option<&crate::checked_cell::ProducedValueTypeInterfaces>,
    inherited: &BTreeMap<(String, String), [u8; 32]>,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<Vec<crate::recovery_artifacts::CertifiedValueInterface>> {
    envelope.validate_structure()?;
    let mut available = inherited.clone();
    for (key, seal) in envelope
        .modules
        .iter()
        .map(|(key, row)| (key, row.interface.sha256))
        .chain(
            envelope
                .value_interfaces
                .iter()
                .map(|(key, row)| (key, row.interface.sha256)),
        )
    {
        if available
            .insert(key.clone(), seal)
            .is_some_and(|old| old != seal)
        {
            return Err(CertificationError::Mismatch(
                "captured value selected owner conflict",
            ));
        }
    }
    let mut issued = Vec::new();
    for output in produced.into_iter().flat_map(|output| output.required()) {
        if !envelope.value_interfaces.keys().any(|(unit, name)| {
            unit == output.interface().unit() && name == output.interface().module()
        }) {
            return Err(CertificationError::Mismatch(
                "produced value type output is missing",
            ));
        }
    }
    for (key, value) in &envelope.value_interfaces {
        let owner = tidepool_repr::SessionModule::from_module_name(&key.1)
            .ok_or(CertificationError::Receipt("captured value session owner"))?;
        let output = produced
            .into_iter()
            .flat_map(|output| output.interfaces())
            .find(|output| {
                output.interface().unit() == key.0 && output.interface().module() == key.1
            });
        if !selected.contains(&owner) && output.is_none() {
            return Err(CertificationError::Mismatch(
                "captured value was not selected by the request",
            ));
        }
        if value
            .interface_requirements
            .iter()
            .any(|(owner, seal)| available.get(owner) != Some(seal))
        {
            return Err(CertificationError::Mismatch(
                "captured value type dependency closure",
            ));
        }
        let bytes = capture(root, &value.interface, PACKAGE_INTERFACE_LIMIT, validation)?;
        let packages = capture(
            root,
            &value.package_imports,
            COMPILER_RECEIPT_BYTES_LIMIT as u64,
            validation,
        )?;
        if let Some(output) = output {
            let expected = output.interface();
            let requirements = output
                .requirements()
                .iter()
                .map(|owner| (owner.unit.clone(), owner.module.clone()))
                .collect::<BTreeSet<_>>();
            if selected.contains(&owner)
                || expected.toolchain_identity_sha256() != producer
                || bytes.as_slice() != expected.interface_bytes()
                || packages.as_slice() != expected.package_imports_bytes()
                || value
                    .interface_requirements
                    .keys()
                    .cloned()
                    .collect::<BTreeSet<_>>()
                    != requirements
            {
                return Err(CertificationError::Mismatch(
                    "produced value type output differs from its reserved capture",
                ));
            }
        }
        let interface = crate::recovery_artifacts::CertifiedJoinedInterface::from_certification_with_validation(
            producer,key.0.clone(),key.1.clone(),bytes,packages,validation)
            .map_err(CertificationError::CapturedModulePayload)?;
        issued.push(
            crate::recovery_artifacts::CertifiedValueInterface::from_admitted_interface(
                interface,
                value
                    .interface_requirements
                    .keys()
                    .map(
                        |(unit, module)| crate::declaration_join::ExactModuleIdentity {
                            unit: unit.clone(),
                            module: module.clone(),
                        },
                    )
                    .collect(),
            ),
        );
    }
    Ok(issued)
}

pub(super) fn issue_interfaces(
    envelope: &FinalizationEnvelope,
    root: &Path,
    producer: [u8; 32],
    evidence: &DependencyEvidence,
    inherited: &BTreeMap<(String, String), [u8; 32]>,
    authored: Option<&crate::declaration_join::NativeAuthoredDeclarationAdmission>,
    exact_source_imports: &BTreeMap<
        crate::declaration_join::ExactModuleIdentity,
        Vec<CanonicalSourceImport>,
    >,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<Vec<CertifiedModuleInterface>> {
    envelope.validate_structure()?;
    if producer == [0; 32] || envelope.profile != FINALIZATION_ENVELOPE_PROFILE {
        return Err(CertificationError::Mismatch(
            "finalization producer/profile",
        ));
    }
    let mut available = inherited.clone();
    for (key, module) in &envelope.modules {
        if available
            .insert(key.clone(), module.interface.sha256)
            .is_some_and(|old| old != module.interface.sha256)
        {
            return Err(CertificationError::Mismatch(
                "canonical interface owner conflict",
            ));
        }
    }
    let home_units = Arc::new(envelope.home_units.clone());
    let mut issued = Vec::new();
    for (key, module) in &envelope.modules {
        let mut matching = evidence
            .modules
            .iter()
            .filter(|node| !node.boot && node.unit == module.unit && node.module == module.module);
        let node = matching
            .next()
            .ok_or(CertificationError::Mismatch("finalized source owner"))?;
        if matching.next().is_some()
            || !evidence.sources.iter().any(|source| {
                source.path == node.source && source.sha256 == hex(&module.source_sha256)
            })
            || !node.product.has_canonical_source_interface()
        {
            return Err(CertificationError::Mismatch(
                "finalized source/interface closure",
            ));
        }
        for (required, seal) in &module.interface_requirements {
            if required == key || available.get(required) != Some(seal) {
                return Err(CertificationError::FinalizedInterfaceRequirement {
                    unit: module.unit.clone(),
                    module: module.module.clone(),
                    required_unit: required.0.clone(),
                    required_module: required.1.clone(),
                    expected_sha256: hex(seal),
                    selected_sha256: available.get(required).map(|seal| hex(seal)),
                });
            }
        }
        let interface = capture(root, &module.interface, PACKAGE_INTERFACE_LIMIT, validation)?;
        let package_imports = capture(
            root,
            &module.package_imports,
            COMPILER_RECEIPT_BYTES_LIMIT as u64,
            validation,
        )?;
        crate::recovery_artifacts::validate_package_imports_with_validation(
            &package_imports,
            &module.unit,
            &module.module,
            &module.interface.sha256,
            Path::new("finalized-module.hi.packages"),
            validation,
        )
        .map_err(|_| CertificationError::Mismatch("finalized package imports"))?;
        let core = module
            .core
            .as_ref()
            .map(|descriptor| capture(root, descriptor, CORE_LIMIT, validation))
            .transpose()?;
        let origin = match authored {
            Some(admission)
                if admission.owner().unit == module.unit
                    && admission.owner().module == module.module =>
            {
                CanonicalOrigin::NativeAuthoredDeclaration {
                    generation: admission.generation(),
                }
            }
            _ => {
                // This is ORIGINAL compiler evidence already admitted by the
                // finalization owner, before any later source-selection receipt.
                let mut imports = BTreeMap::new();
                for edge in &node.imports {
                    let home_unit = match &edge.selected {
                        None => None,
                        Some(path) => {
                            let owners = evidence
                                .modules
                                .iter()
                                .filter(|child| {
                                    child.module == edge.module
                                        && child.boot == edge.boot
                                        && &child.source == path
                                })
                                .collect::<Vec<_>>();
                            let [child] = owners.as_slice() else {
                                return Err(CertificationError::Mismatch(
                                    "canonical original source import owner",
                                ));
                            };
                            Some(child.unit.clone())
                        }
                    };
                    let row = CanonicalSourceImport {
                        qualifier: edge.qualifier.clone(),
                        module: edge.module.clone(),
                        boot: edge.boot,
                        home_unit,
                    };
                    imports.insert(row.key(), row);
                }
                let owner = crate::declaration_join::ExactModuleIdentity {
                    unit: module.unit.clone(),
                    module: module.module.clone(),
                };
                for row in exact_source_imports.get(&owner).into_iter().flatten() {
                    imports.insert(row.key(), row.clone());
                }
                if imports.len() > 4096 {
                    return Err(CertificationError::Receipt("canonical source import bound"));
                }
                let origin = CanonicalOrigin::SourceOriginal {
                    imports: imports.into_values().collect::<Vec<_>>().into(),
                };
                // Issuance enforces the same classification and uniqueness
                // invariants as cold recovery, including exact parser rows.
                CanonicalOrigin::decode(&origin.encode(), module, &envelope.home_units)?
            }
        };
        let certificate = canonical_certificate_with_operation(
            producer,
            envelope,
            module,
            &origin,
            &validation.inventory,
        )?;
        issued.push(CertifiedModuleInterface {
            producer_sha256: producer,
            receipt: module.clone(),
            home_units: Arc::clone(&home_units),
            interface: interface.into(),
            package_imports: package_imports.into(),
            certificate: certificate.into(),
            core: core.map(Into::into),
            origin,
        });
    }
    Ok(issued)
}

/// Cold and externally captured certificates pass the full decoder and payload
/// checks. A certificate never derives identity from source spelling or ABI.
pub(super) fn recover_interface(
    producer: [u8; 32],
    certificate: Vec<u8>,
    interface: Vec<u8>,
    package_imports: Vec<u8>,
    core: Option<Vec<u8>>,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<CertifiedModuleInterface> {
    if certificate.len() > CANONICAL_CERTIFICATE_LIMIT {
        return Err(CertificationError::Receipt("finalized certificate size"));
    }
    let value = validation
        .inventory
        .decode_value(&certificate, CANONICAL_CERTIFICATE_LIMIT)?;
    validation.inventory.charge_value_copies(&value, 3)?;
    let row = sized(&value, 13)?;
    if string(&row[0])? != "TPFINALMODULE"
        || number(&row[1])? != 3
        || string(&row[2])? != FINALIZATION_PROFILE
        || digest(&row[3])? != producer
        || producer == [0; 32]
    {
        return Err(CertificationError::Mismatch(
            "finalized certificate producer/profile",
        ));
    }
    let core_row = match &row[10] {
        Value::Null if core.is_none() => Value::Null,
        Value::Text(_) if core.is_some() => value_array([
            value_text("module.core"),
            row[10].clone(),
            Value::Integer((core.as_ref().expect("checked core presence").len() as u64).into()),
        ]),
        _ => {
            return Err(CertificationError::Mismatch(
                "finalized Core recovery presence",
            ))
        }
    };
    let descriptor_row = value_array([
        row[5].clone(),
        row[6].clone(),
        row[7].clone(),
        value_text("module.hi"),
        row[8].clone(),
        Value::Integer((interface.len() as u64).into()),
        value_text("module.hi.packages"),
        row[9].clone(),
        Value::Integer((package_imports.len() as u64).into()),
        core_row,
        row[11].clone(),
    ]);
    let envelope = decode_envelope_with_operation(
        &value_array([
            value_text(FINALIZATION_ENVELOPE_PROFILE),
            row[4].clone(),
            value_array([descriptor_row]),
            value_array([]),
        ]),
        &validation.inventory,
    )?;
    let receipt = envelope
        .modules
        .into_values()
        .next()
        .ok_or(CertificationError::Receipt("finalized certificate owner"))?;
    let origin = CanonicalOrigin::decode(&row[12], &receipt, &envelope.home_units)?;
    if sha(&interface) != receipt.interface.sha256
        || sha(&package_imports) != receipt.package_imports.sha256
        || core.as_ref().map(|bytes| sha(bytes)) != receipt.core.as_ref().map(|item| item.sha256)
    {
        return Err(CertificationError::Mismatch(
            "finalized certificate payload",
        ));
    }
    crate::recovery_artifacts::validate_package_imports_with_validation(
        &package_imports,
        &receipt.unit,
        &receipt.module,
        &receipt.interface.sha256,
        Path::new("finalized-module.hi.packages"),
        validation,
    )
    .map_err(|_| CertificationError::Mismatch("finalized package imports"))?;
    let canonical_envelope = FinalizationEnvelope {
        profile: FINALIZATION_ENVELOPE_PROFILE.into(),
        value_interfaces: BTreeMap::new(),
        home_units: envelope.home_units,
        modules: BTreeMap::new(),
    };
    if canonical_certificate_with_operation(
        producer,
        &canonical_envelope,
        &receipt,
        &origin,
        &validation.inventory,
    )? != certificate
    {
        return Err(CertificationError::Receipt(
            "noncanonical finalized certificate",
        ));
    }
    Ok(CertifiedModuleInterface {
        producer_sha256: producer,
        receipt,
        home_units: Arc::new(canonical_envelope.home_units),
        interface: interface.into(),
        package_imports: package_imports.into(),
        certificate: certificate.into(),
        core: core.map(Into::into),
        origin,
    })
}

#[cfg(test)]
pub(super) fn fixture_interface(
    producer: [u8; 32],
    unit: &str,
    module: &str,
    source_sha256: [u8; 32],
    interface: Vec<u8>,
    package_imports: Vec<u8>,
    interface_requirements: BTreeMap<(String, String), [u8; 32]>,
    core: Option<Vec<u8>>,
) -> CertifiedModuleInterface {
    let home_units = std::iter::once(unit.to_owned())
        .chain(interface_requirements.keys().map(|(unit, _)| unit.clone()))
        .collect();
    let receipt = FinalizedModuleReceipt {
        unit: unit.into(),
        module: module.into(),
        source_sha256,
        interface: CapturedArtifactDescriptor {
            relative_path: "module.hi".into(),
            sha256: sha(&interface),
            bytes: interface.len() as u64,
        },
        package_imports: CapturedArtifactDescriptor {
            relative_path: "module.hi.packages".into(),
            sha256: sha(&package_imports),
            bytes: package_imports.len() as u64,
        },
        core: core.as_ref().map(|bytes| CapturedArtifactDescriptor {
            relative_path: "module.core".into(),
            sha256: sha(bytes),
            bytes: bytes.len() as u64,
        }),
        interface_requirements,
    };
    let envelope = FinalizationEnvelope {
        profile: FINALIZATION_ENVELOPE_PROFILE.into(),
        value_interfaces: BTreeMap::new(),
        home_units,
        modules: BTreeMap::new(),
    };
    let certificate = canonical_certificate(
        producer,
        &envelope,
        &receipt,
        &CanonicalOrigin::SourceOriginal {
            imports: Arc::from([]),
        },
    )
    .unwrap();
    recover_interface(
        producer,
        certificate,
        interface,
        package_imports,
        core,
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap()
}

#[cfg(test)]
pub(super) fn fixture_source_imports(
    interface: CertifiedModuleInterface,
    imports: Vec<CanonicalSourceImport>,
) -> CertifiedModuleInterface {
    let mut rows = imports
        .into_iter()
        .map(|row| (row.key(), row))
        .collect::<BTreeMap<_, _>>();
    let origin = CanonicalOrigin::SourceOriginal {
        imports: std::mem::take(&mut rows)
            .into_values()
            .collect::<Vec<_>>()
            .into(),
    };
    let envelope = FinalizationEnvelope {
        profile: FINALIZATION_ENVELOPE_PROFILE.into(),
        value_interfaces: BTreeMap::new(),
        home_units: (*interface.home_units).clone(),
        modules: BTreeMap::new(),
    };
    let certificate = canonical_certificate(
        interface.producer_sha256,
        &envelope,
        &interface.receipt,
        &origin,
    )
    .unwrap();
    recover_interface(
        interface.producer_sha256,
        certificate,
        interface.interface.to_vec(),
        interface.package_imports.to_vec(),
        interface.core.as_ref().map(|bytes| bytes.to_vec()),
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap()
}

#[cfg(test)]
pub(super) fn encode_fixture_envelope(envelope: &FinalizationEnvelope) -> Value {
    value_array([
        value_text(&envelope.profile),
        value_array(envelope.home_units.iter().map(value_text)),
        value_array(envelope.modules.values().map(|module| {
            value_array([
                value_text(&module.unit),
                value_text(&module.module),
                value_text(hex(&module.source_sha256)),
                value_text(module.interface.relative_path.to_string_lossy()),
                value_text(hex(&module.interface.sha256)),
                Value::Integer(module.interface.bytes.into()),
                value_text(module.package_imports.relative_path.to_string_lossy()),
                value_text(hex(&module.package_imports.sha256)),
                Value::Integer(module.package_imports.bytes.into()),
                module.core.as_ref().map_or(Value::Null, |core| {
                    value_array([
                        value_text(core.relative_path.to_string_lossy()),
                        value_text(hex(&core.sha256)),
                        Value::Integer(core.bytes.into()),
                    ])
                }),
                encode_interface_requirements(&module.interface_requirements),
            ])
        })),
        value_array(
            envelope
                .value_interfaces
                .iter()
                .map(|((unit, module), value)| {
                    value_array([
                        value_text(unit),
                        value_text(module),
                        value_text(value.interface.relative_path.to_string_lossy()),
                        value_text(hex(&value.interface.sha256)),
                        Value::Integer(value.interface.bytes.into()),
                        value_text(value.package_imports.relative_path.to_string_lossy()),
                        value_text(hex(&value.package_imports.sha256)),
                        Value::Integer(value.package_imports.bytes.into()),
                        encode_interface_requirements(&value.interface_requirements),
                    ])
                }),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interface_for_owner(
        unit: &str,
        name: &str,
        core: Option<Vec<u8>>,
    ) -> CertifiedModuleInterface {
        let bytes = b"interface".to_vec();
        let packages = value_array([
            value_text("TPPKGROOTS"),
            value_text("2"),
            value_array([
                value_text(unit),
                value_text(name),
                value_text(hex(&sha(&bytes))),
            ]),
            value_array([]),
            value_array([]),
        ]);
        let mut package_bytes = Vec::new();
        ciborium::ser::into_writer(&packages, &mut package_bytes).unwrap();
        fixture_interface(
            [7; 32],
            unit,
            name,
            [8; 32],
            bytes,
            package_bytes,
            BTreeMap::new(),
            core,
        )
    }

    fn interface(core: Option<Vec<u8>>) -> CertifiedModuleInterface {
        interface_for_owner("home-a", "Owner", core)
    }

    #[test]
    fn retained_native_promotion_requires_original_payloads_and_exact_seals() {
        let original = interface(Some(b"core".to_vec()));
        let required = ("home-a".into(), "Dependency".into());
        let requirements = BTreeMap::from([(required.clone(), [9; 32])]);
        let original = fixture_interface(
            original.producer_sha256(),
            original.unit(),
            original.module(),
            original.source_sha256(),
            original.interface_bytes().to_vec(),
            original.package_imports_bytes().to_vec(),
            requirements.clone(),
            original.core_bytes().map(ToOwned::to_owned),
        );
        let accepted = CertifiedModuleReceipt {
            origin: ProductOrigin::RetainedCore,
            unit: original.unit().into(),
            module: original.module().into(),
            module_version: None,
            source_sha256: original.source_sha256(),
            skinny_iface_sha256: original.interface_sha256(),
            product_sha256: [10; 32],
            dependency_witness_sha256: sha(original.certificate_bytes()),
            groups: vec![],
            interface_requirements: requirements.clone(),
        };
        let validate = |original: &CertifiedModuleInterface,
                        accepted: &CertifiedModuleReceipt,
                        seals: &BTreeMap<_, _>,
                        interface: &[u8],
                        packages: &[u8]| {
            original.validate_native_promotion(accepted, [7; 32], seals, interface, packages)
        };
        assert!(validate(
            &original,
            &accepted,
            &requirements,
            original.interface_bytes(),
            original.package_imports_bytes(),
        )
        .is_ok());
        let receipt_mutations: [fn(&mut CertifiedModuleReceipt); 8] = [
            |row: &mut CertifiedModuleReceipt| row.origin = ProductOrigin::Fresh,
            |row: &mut CertifiedModuleReceipt| row.unit = "other-home".into(),
            |row: &mut CertifiedModuleReceipt| row.module = "Other".into(),
            |row: &mut CertifiedModuleReceipt| row.source_sha256 = [0; 32],
            |row: &mut CertifiedModuleReceipt| row.skinny_iface_sha256 = [0; 32],
            |row: &mut CertifiedModuleReceipt| row.dependency_witness_sha256 = [0; 32],
            |row: &mut CertifiedModuleReceipt| row.interface_requirements.clear(),
            |row: &mut CertifiedModuleReceipt| row.module_version = Some(ModuleVersion([11; 32])),
        ];
        for mutate in receipt_mutations {
            let mut changed = accepted.clone();
            mutate(&mut changed);
            assert!(validate(
                &original,
                &changed,
                &requirements,
                original.interface_bytes(),
                original.package_imports_bytes(),
            )
            .is_err());
        }
        for seals in [BTreeMap::new(), BTreeMap::from([(required, [0; 32])])] {
            assert!(matches!(
                validate(
                    &original,
                    &accepted,
                    &seals,
                    original.interface_bytes(),
                    original.package_imports_bytes(),
                ),
                Err(CertificationError::FinalizedInterfaceRequirement { .. })
            ));
        }
        assert!(validate(
            &original,
            &accepted,
            &requirements,
            b"changed interface",
            original.package_imports_bytes(),
        )
        .is_err());
        assert!(validate(
            &original,
            &accepted,
            &requirements,
            original.interface_bytes(),
            b"changed packages",
        )
        .is_err());
        let canonical_mutations: [fn(&mut CertifiedModuleInterface); 4] = [
            |module: &mut CertifiedModuleInterface| module.core = None,
            |module: &mut CertifiedModuleInterface| {
                module.core = Some(b"changed Core".to_vec().into())
            },
            |module: &mut CertifiedModuleInterface| module.producer_sha256 = [0; 32],
            |module: &mut CertifiedModuleInterface| {
                module.origin = CanonicalOrigin::NativeAuthoredDeclaration { generation: 1 }
            },
        ];
        for mutate in canonical_mutations {
            let mut changed = original.clone();
            mutate(&mut changed);
            assert!(validate(
                &changed,
                &accepted,
                &requirements,
                original.interface_bytes(),
                original.package_imports_bytes(),
            )
            .is_err());
        }
        let envelope = FinalizationEnvelope {
            profile: FINALIZATION_ENVELOPE_PROFILE.into(),
            value_interfaces: BTreeMap::new(),
            home_units: original.home_units().clone(),
            modules: BTreeMap::from([(
                (original.unit().into(), original.module().into()),
                original.receipt.clone(),
            )]),
        };
        assert!(matches!(
            envelope.validate_owners(&[accepted], &BTreeMap::new()),
            Err(CertificationError::Receipt(
                "retained core owner has fresh finalization"
            )),
        ));
    }

    #[test]
    fn canonical_source_imports_reject_contradictory_home_classification() {
        use crate::cache::ImportQualifier;
        let original = interface(None);
        let edge = CanonicalSourceImport {
            qualifier: ImportQualifier::Unqualified,
            module: "Dependency".into(),
            boot: false,
            home_unit: Some("home-a".into()),
        };
        let decode = |imports: Vec<CanonicalSourceImport>| {
            let origin = CanonicalOrigin::SourceOriginal {
                imports: imports.into(),
            };
            CanonicalOrigin::decode(&origin.encode(), &original.receipt, original.home_units())
        };
        assert!(decode(vec![edge.clone()]).is_ok());
        assert!(decode(vec![CanonicalSourceImport {
            qualifier: ImportQualifier::ThisUnit("home-a".into()),
            home_unit: None,
            ..edge.clone()
        }])
        .is_err());
        assert!(decode(vec![CanonicalSourceImport {
            qualifier: ImportQualifier::ThisUnit("other-home".into()),
            ..edge.clone()
        }])
        .is_err());
        assert!(decode(vec![CanonicalSourceImport {
            qualifier: ImportQualifier::OtherUnit("home-a".into()),
            ..edge.clone()
        }])
        .is_err());
        assert!(decode(vec![
            CanonicalSourceImport {
                home_unit: None,
                ..edge.clone()
            },
            edge
        ])
        .is_err());
    }

    #[test]
    fn captured_payload_errors_preserve_missing_and_modified_artifact_causes() {
        use crate::recovery_artifacts::RecoveryArtifactError;

        let root = tempfile::tempdir().unwrap();
        let fixture = interface(None);
        let descriptor = &fixture.receipt.interface;
        let path = root.path().join(&descriptor.relative_path);
        let mut validation = PackageInterfaceValidation::default();
        assert!(matches!(
            capture(root.path(), descriptor, 1024, &mut validation),
            Err(CertificationError::CapturedModulePayload(
                RecoveryArtifactError::Unavailable(missing)
            )) if missing == path
        ));
        let mut modified = fixture.interface_bytes().to_vec();
        modified[0] ^= 1;
        std::fs::write(&path, modified).unwrap();
        assert!(matches!(
            capture(root.path(), descriptor, 1024, &mut validation),
            Err(CertificationError::CapturedModulePayload(
                RecoveryArtifactError::DigestMismatch(changed)
            )) if changed == path
        ));
        std::fs::write(&path, fixture.interface_bytes()).unwrap();
        assert_eq!(
            capture(root.path(), descriptor, 1024, &mut validation).unwrap(),
            fixture.interface_bytes()
        );
    }

    #[test]
    fn canonical_source_issuer_seals_original_exact_import_shapes_for_interface_only_owner() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("Owner.hs");
        let bytes =
            b"module Owner where\nimport {-# SOURCE #-} \"home-a\" Dependency\ntype Answer = Int\n";
        std::fs::write(&source, bytes).unwrap();
        let fixture = interface(None);
        std::fs::write(root.path().join("module.hi"), fixture.interface_bytes()).unwrap();
        std::fs::write(
            root.path().join("module.hi.packages"),
            fixture.package_imports_bytes(),
        )
        .unwrap();
        let mut receipt = fixture.receipt.clone();
        receipt.source_sha256 = sha(bytes);
        let envelope = FinalizationEnvelope {
            profile: FINALIZATION_ENVELOPE_PROFILE.into(),
            value_interfaces: BTreeMap::new(),
            home_units: fixture.home_units().clone(),
            modules: BTreeMap::from([(("home-a".into(), "Owner".into()), receipt)]),
        };
        let evidence = DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: vec![crate::cache::SourceEvidence {
                path: source.clone(),
                sha256: hex(&sha(bytes)),
            }],
            modules: vec![crate::cache::ModuleEvidence {
                unit: "home-a".into(),
                module: "Owner".into(),
                boot: false,
                source,
                imports: vec![],
                product: ProductAvailability::ProjectionRejected,
            }],
            resolutions: vec![],
            packages: vec![],
        };
        // The normalized original graph omitted this exact edge; its original
        // receipt still retains the GHC parser's package qualifier and owner.
        let imported = CanonicalSourceImport {
            qualifier: crate::cache::ImportQualifier::ThisUnit("home-a".into()),
            module: "Dependency".into(),
            boot: true,
            home_unit: Some("home-a".into()),
        };
        let exact = BTreeMap::from([(
            crate::declaration_join::ExactModuleIdentity {
                unit: "home-a".into(),
                module: "Owner".into(),
            },
            vec![imported.clone()],
        )]);
        let issued = issue_interfaces(
            &envelope,
            root.path(),
            [7; 32],
            &evidence,
            &BTreeMap::new(),
            None,
            &exact,
            &mut PackageInterfaceValidation::default(),
        )
        .unwrap();
        assert!(issued[0].requirements().is_empty());
        assert_eq!(issued[0].source_imports(), Some([imported].as_slice()));
        let recovered = recover_interface(
            [7; 32],
            issued[0].certificate_bytes().to_vec(),
            issued[0].interface_bytes().to_vec(),
            issued[0].package_imports_bytes().to_vec(),
            None,
            &mut PackageInterfaceValidation::default(),
        )
        .unwrap();
        assert_eq!(recovered, issued[0]);
        let mut missing_interface = evidence.clone();
        missing_interface.modules[0].product = ProductAvailability::MissingInterface;
        assert!(matches!(
            issue_interfaces(
                &envelope,
                root.path(),
                [7; 32],
                &missing_interface,
                &BTreeMap::new(),
                None,
                &exact,
                &mut PackageInterfaceValidation::default(),
            ),
            Err(CertificationError::Mismatch(
                "finalized source/interface closure"
            ))
        ));
        let mut changed_source = evidence.clone();
        changed_source.sources[0].sha256 = hex(&sha(b"different source bytes"));
        assert!(matches!(
            issue_interfaces(
                &envelope,
                root.path(),
                [7; 32],
                &changed_source,
                &BTreeMap::new(),
                None,
                &exact,
                &mut PackageInterfaceValidation::default(),
            ),
            Err(CertificationError::Mismatch(
                "finalized source/interface closure"
            ))
        ));
        assert!(matches!(
            issue_interfaces(
                &envelope,
                root.path(),
                [0; 32],
                &evidence,
                &BTreeMap::new(),
                None,
                &exact,
                &mut PackageInterfaceValidation::default(),
            ),
            Err(CertificationError::Mismatch(
                "finalization producer/profile"
            ))
        ));
        let mut changed_profile = envelope.clone();
        changed_profile.profile = "unmatched profile".into();
        assert!(matches!(
            issue_interfaces(
                &changed_profile,
                root.path(),
                [7; 32],
                &evidence,
                &BTreeMap::new(),
                None,
                &exact,
                &mut PackageInterfaceValidation::default(),
            ),
            Err(CertificationError::Receipt(
                "finalization inventory bounds/profile"
            ))
        ));
        let mut old: Value = ciborium::de::from_reader(issued[0].certificate_bytes()).unwrap();
        old.as_array_mut().unwrap()[1] = Value::Integer(2.into());
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&old, &mut bytes).unwrap();
        assert!(recover_interface(
            [7; 32],
            bytes,
            issued[0].interface_bytes().to_vec(),
            issued[0].package_imports_bytes().to_vec(),
            None,
            &mut PackageInterfaceValidation::default()
        )
        .is_err());
    }

    #[test]
    fn canonical_origin_is_explicit_and_survives_recovery() {
        let source = interface_for_owner("main", "Tidepool.Session.Lib.G7", None);
        assert!(matches!(
            source.origin(),
            CanonicalOrigin::SourceOriginal { .. }
        ));
        let envelope = FinalizationEnvelope {
            profile: FINALIZATION_ENVELOPE_PROFILE.into(),
            value_interfaces: BTreeMap::new(),
            home_units: source.home_units().clone(),
            modules: BTreeMap::new(),
        };
        let native = CanonicalOrigin::NativeAuthoredDeclaration { generation: 7 };
        let certificate =
            canonical_certificate([7; 32], &envelope, &source.receipt, &native).unwrap();
        let recovered = recover_interface(
            [7; 32],
            certificate,
            source.interface_bytes().to_vec(),
            source.package_imports_bytes().to_vec(),
            None,
            &mut PackageInterfaceValidation::default(),
        )
        .unwrap();
        assert_eq!(recovered.origin(), native);
        for origin in [
            CanonicalOrigin::NativeAuthoredDeclaration { generation: 0 },
            CanonicalOrigin::NativeAuthoredDeclaration { generation: 8 },
        ] {
            let certificate =
                canonical_certificate([7; 32], &envelope, &source.receipt, &origin).unwrap();
            assert!(recover_interface(
                [7; 32],
                certificate,
                source.interface_bytes().to_vec(),
                source.package_imports_bytes().to_vec(),
                None,
                &mut PackageInterfaceValidation::default()
            )
            .is_err());
        }
        let mut legacy: Value = ciborium::de::from_reader(source.certificate_bytes()).unwrap();
        let Value::Array(ref mut row) = legacy else {
            unreachable!()
        };
        row[1] = Value::Integer(1.into());
        row.pop();
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&legacy, &mut bytes).unwrap();
        assert!(recover_interface(
            [7; 32],
            bytes,
            source.interface_bytes().to_vec(),
            source.package_imports_bytes().to_vec(),
            None,
            &mut PackageInterfaceValidation::default()
        )
        .is_err());
    }

    #[test]
    fn canonical_certificate_retains_exact_core_and_rejects_substitution() {
        let module = interface(Some(b"tidy-core".to_vec()));
        assert!(recover_interface(
            [7; 32],
            module.certificate_bytes().to_vec(),
            module.interface_bytes().to_vec(),
            module.package_imports_bytes().to_vec(),
            Some(b"other-core".to_vec()),
            &mut PackageInterfaceValidation::default()
        )
        .is_err());
        assert!(recover_interface(
            [9; 32],
            module.certificate_bytes().to_vec(),
            module.interface_bytes().to_vec(),
            module.package_imports_bytes().to_vec(),
            Some(b"tidy-core".to_vec()),
            &mut PackageInterfaceValidation::default()
        )
        .is_err());
        let cloned = module.clone();
        assert!(Arc::ptr_eq(&module.interface, &cloned.interface));
        assert!(Arc::ptr_eq(
            module.core.as_ref().unwrap(),
            cloned.core.as_ref().unwrap()
        ));
    }

    #[test]
    fn explicit_absent_core_cannot_be_added_after_capture() {
        let module = interface(None);
        assert!(module.core_bytes().is_none());
        assert!(recover_interface(
            [7; 32],
            module.certificate_bytes().to_vec(),
            module.interface_bytes().to_vec(),
            module.package_imports_bytes().to_vec(),
            Some(b"core".to_vec()),
            &mut PackageInterfaceValidation::default()
        )
        .is_err());
    }

    #[test]
    fn complete_home_units_refuse_a_home_owner_as_package() {
        let module = interface(None);
        let envelope = FinalizationEnvelope {
            profile: FINALIZATION_ENVELOPE_PROFILE.into(),
            value_interfaces: BTreeMap::new(),
            home_units: module.home_units().clone(),
            modules: BTreeMap::new(),
        };
        let packages = BTreeMap::from([(
            ("home-a".into(), "PackageClaim".into()),
            PackageInterfaceWitness {
                selected_path: "/tmp/claimed.hi".into(),
                sha256: [1; 32],
            },
        )]);
        assert!(envelope.validate_owners(&[], &packages).is_err());
    }
    #[test]
    fn typed_finalization_refuses_durable_bound_and_home_inventory_drift() {
        let module = interface(Some(b"tidy-core".to_vec()));
        let key = (module.unit().to_owned(), module.module().to_owned());
        let envelope = FinalizationEnvelope {
            profile: FINALIZATION_ENVELOPE_PROFILE.into(),
            value_interfaces: BTreeMap::new(),
            home_units: module.home_units().clone(),
            modules: BTreeMap::from([(key.clone(), module.receipt.clone())]),
        };
        envelope.validate_structure().unwrap();
        let mut changed = envelope.clone();
        changed
            .modules
            .get_mut(&key)
            .unwrap()
            .core
            .as_mut()
            .unwrap()
            .bytes = 0;
        assert!(changed.validate_structure().is_err());
        let mut changed = envelope.clone();
        changed
            .modules
            .get_mut(&key)
            .unwrap()
            .core
            .as_mut()
            .unwrap()
            .relative_path = module.receipt.interface.relative_path.clone();
        assert!(changed.validate_structure().is_err());
        let mut changed = envelope;
        changed.home_units.remove(module.unit());
        assert!(changed.validate_structure().is_err());
    }
    #[test]
    fn selected_value_capture_closes_types_without_source_or_native_authority() {
        let root = tempfile::tempdir().unwrap();
        let selected = tidepool_repr::SessionModule::val(tidepool_repr::Generation(1));
        let key = ("main".to_owned(), selected.module_name());
        let bytes = b"selected thin interface".to_vec();
        let packages = value_array([
            value_text("TPPKGROOTS"),
            value_text("2"),
            value_array([
                value_text(&key.0),
                value_text(&key.1),
                value_text(hex(&sha(&bytes))),
            ]),
            value_array([]),
            value_array([]),
        ]);
        let mut value_package_bytes = Vec::new();
        ciborium::ser::into_writer(&packages, &mut value_package_bytes).unwrap();
        std::fs::write(root.path().join("value.hi"), &bytes).unwrap();
        std::fs::write(root.path().join("value.packages"), &value_package_bytes).unwrap();
        let nominal = interface(None);
        let inherited = BTreeMap::from([(
            (nominal.unit().to_owned(), nominal.module().to_owned()),
            nominal.interface_sha256(),
        )]);
        let envelope = FinalizationEnvelope {
            profile: FINALIZATION_ENVELOPE_PROFILE.into(),
            home_units: BTreeSet::from(["main".into(), nominal.unit().to_owned()]),
            modules: BTreeMap::new(),
            value_interfaces: BTreeMap::from([(
                key.clone(),
                CapturedValueInterfaceReceipt {
                    interface: CapturedArtifactDescriptor {
                        relative_path: "value.hi".into(),
                        sha256: sha(&bytes),
                        bytes: bytes.len() as u64,
                    },
                    package_imports: CapturedArtifactDescriptor {
                        relative_path: "value.packages".into(),
                        sha256: sha(&value_package_bytes),
                        bytes: value_package_bytes.len() as u64,
                    },
                    interface_requirements: inherited.clone(),
                },
            )]),
        };
        let encoded = encode_fixture_envelope(&envelope);
        let decoded = decode_envelope(&encoded).unwrap();
        assert_eq!(decoded, envelope);
        let issue = |value: &FinalizationEnvelope,
                     selected: &[tidepool_repr::SessionModule],
                     inherited: &BTreeMap<_, _>| {
            issue_value_interfaces(
                value,
                root.path(),
                nominal.producer_sha256(),
                selected,
                None,
                inherited,
                &mut PackageInterfaceValidation::default(),
            )
        };
        let values = issue(&decoded, &[selected], &inherited).unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].interface().interface_bytes(), bytes);
        assert!(decoded.modules.is_empty());
        let consumer_bytes = b"consumer interface".to_vec();
        let package = value_array([
            value_text("TPPKGROOTS"),
            value_text("2"),
            value_array([
                value_text("main"),
                value_text("Consumer"),
                value_text(hex(&sha(&consumer_bytes))),
            ]),
            value_array([]),
            value_array([]),
        ]);
        let mut package_bytes = Vec::new();
        ciborium::ser::into_writer(&package, &mut package_bytes).unwrap();
        let consumer = fixture_interface(
            nominal.producer_sha256(),
            "main",
            "Consumer",
            [8; 32],
            consumer_bytes,
            package_bytes,
            BTreeMap::from([(key.clone(), sha(&bytes))]),
            None,
        );
        let view = crate::declaration_context::certified_product_artifact_view_with_validation(
            nominal.producer_sha256(),
            &[],
            &[nominal.clone(), consumer],
            &values,
            None,
            crate::artifact_inventory::NativeArtifactDemand::AllGroups,
            &mut PackageInterfaceValidation::default(),
        )
        .unwrap();
        use crate::artifact_inventory::ArtifactKind;
        let descriptors = view.descriptors();
        assert!(descriptors
            .iter()
            .any(|row| row.kind == ArtifactKind::ValueInterface
                && row.owner.unit == key.0
                && row.owner.module == key.1));
        assert!(descriptors
            .iter()
            .all(|row| row.kind != ArtifactKind::OriginalModule));
        assert!(issue(&decoded, &[], &inherited).is_err());
        assert!(issue(
            &decoded,
            &[tidepool_repr::SessionModule::val(
                tidepool_repr::Generation(2)
            )],
            &inherited
        )
        .is_err());
        assert!(issue(&decoded, &[selected], &BTreeMap::new()).is_err());
        let mut changed = decoded.clone();
        changed
            .value_interfaces
            .get_mut(&key)
            .unwrap()
            .interface_requirements
            .insert(
                (nominal.unit().to_owned(), nominal.module().to_owned()),
                [9; 32],
            );
        assert!(issue(&changed, &[selected], &inherited).is_err());
        let mut old = decoded.clone();
        old.profile = FINALIZATION_PROFILE.into();
        assert!(decode_envelope(&encode_fixture_envelope(&old)).is_err());
        let mut duplicate = encoded;
        let rows = duplicate.as_array_mut().unwrap()[3].as_array_mut().unwrap();
        rows.push(rows[0].clone());
        assert!(decode_envelope(&duplicate).is_err());
        std::fs::write(root.path().join("value.packages"), b"altered sidecar").unwrap();
        assert!(issue(&decoded, &[selected], &inherited).is_err());
        std::fs::write(root.path().join("value.packages"), &value_package_bytes).unwrap();
        assert!(issue(&decoded, &[selected], &inherited).is_ok());
        std::fs::write(root.path().join("value.hi"), b"altered interface").unwrap();
        assert!(issue(&decoded, &[selected], &inherited).is_err());
        std::fs::write(root.path().join("value.hi"), &bytes).unwrap();
        assert!(issue(&decoded, &[selected], &inherited).is_ok());
        // A fully hash-sealed sidecar naming another owner still cannot issue
        // the selected interface's authority.
        let mut wrong_owner = packages;
        wrong_owner.as_array_mut().unwrap()[2]
            .as_array_mut()
            .unwrap()[1] = value_text(
            tidepool_repr::SessionModule::val(tidepool_repr::Generation(2)).module_name(),
        );
        let mut wrong_package_bytes = Vec::new();
        ciborium::ser::into_writer(&wrong_owner, &mut wrong_package_bytes).unwrap();
        std::fs::write(root.path().join("value.packages"), &wrong_package_bytes).unwrap();
        let mut wrong_package = decoded.clone();
        let descriptor = &mut wrong_package
            .value_interfaces
            .get_mut(&key)
            .unwrap()
            .package_imports;
        descriptor.sha256 = sha(&wrong_package_bytes);
        descriptor.bytes = wrong_package_bytes.len() as u64;
        assert!(issue(&wrong_package, &[selected], &inherited).is_err());
        std::fs::write(root.path().join("value.packages"), &value_package_bytes).unwrap();
        assert!(issue(&decoded, &[selected], &inherited).is_ok());
    }
}
