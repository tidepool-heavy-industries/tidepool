//! Canonical module interfaces issued by one completed compiler finalization.
//! Core is retained as compiler input, never projected as execution authority.

use super::*;
use std::path::Component;

pub(crate) const FINALIZATION_PROFILE: &str = "tidepool-ghc-finalized-module-v1";
const CORE_LIMIT: u64 = 32 << 20;
const FINALIZATION_PAYLOAD_LIMIT: usize = 128 << 20;

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
pub struct FinalizationEnvelope {
    pub profile: String,
    /// Complete compiler home-unit inventory, not a unit inferred from spelling.
    pub home_units: BTreeSet<String>,
    pub modules: BTreeMap<(String, String), FinalizedModuleReceipt>,
}

impl FinalizationEnvelope {
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

pub(super) fn decode_envelope(value: &Value) -> CertResult<FinalizationEnvelope> {
    let row = sized(value, 3)?;
    let profile = string(&row[0])?.to_owned();
    if profile != FINALIZATION_PROFILE {
        return Err(CertificationError::Receipt("finalization profile"));
    }
    let units = array(&row[1])?;
    if units.is_empty() || units.len() > MODULE_LIMIT {
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
    if rows.len() > MODULE_LIMIT {
        return Err(CertificationError::Receipt("finalized module count"));
    }
    let mut modules = BTreeMap::new();
    let mut previous = None;
    let mut paths = BTreeSet::new();
    let mut payload_bytes = 0usize;
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
        let package_imports = descriptor(&row[6], &row[7], &row[8], RECEIPT_LIMIT as u64)?;
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
            payload_bytes = payload_bytes
                .checked_add(item.bytes as usize)
                .ok_or(CertificationError::Receipt("finalization payload budget"))?;
            if payload_bytes > FINALIZATION_PAYLOAD_LIMIT {
                return Err(CertificationError::Receipt("finalization payload budget"));
            }
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
    Ok(FinalizationEnvelope {
        profile,
        home_units,
        modules,
    })
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
    /// Materialization can retain Core without making it available through an
    /// interface capability. Preparation requires a separate admission owner.
    pub(crate) fn core_bytes(&self) -> Option<&[u8]> {
        self.core.as_deref()
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
    .map_err(|_| CertificationError::Mismatch("captured module payload"))
}

fn canonical_certificate(
    producer: [u8; 32],
    envelope: &FinalizationEnvelope,
    module: &FinalizedModuleReceipt,
) -> CertResult<Vec<u8>> {
    // Scratch paths and descriptor sizes are not durable semantic identity.
    let value = value_array([
        value_text("TPFINALMODULE"),
        Value::Integer(1.into()),
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
    ]);
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&value, &mut bytes)
        .map_err(|_| CertificationError::Receipt("finalized certificate encoding"))?;
    if bytes.len() > RECEIPT_LIMIT {
        return Err(CertificationError::Receipt("finalized certificate size"));
    }
    Ok(bytes)
}

pub(super) fn issue_interfaces(
    envelope: &FinalizationEnvelope,
    root: &Path,
    producer: [u8; 32],
    evidence: &DependencyEvidence,
    inherited: &BTreeMap<(String, String), [u8; 32]>,
    validation: &mut PackageInterfaceValidation,
) -> CertResult<Vec<CertifiedModuleInterface>> {
    if producer == [0; 32] || envelope.profile != FINALIZATION_PROFILE {
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
            || !matches!(
                node.product,
                ProductAvailability::Ready | ProductAvailability::InterfaceOnly
            )
            || module
                .interface_requirements
                .iter()
                .any(|(required, seal)| required == key || available.get(required) != Some(seal))
        {
            return Err(CertificationError::Mismatch(
                "finalized source/interface closure",
            ));
        }
        let interface = capture(root, &module.interface, PACKAGE_INTERFACE_LIMIT, validation)?;
        let package_imports = capture(
            root,
            &module.package_imports,
            RECEIPT_LIMIT as u64,
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
        let certificate = canonical_certificate(producer, envelope, module)?;
        issued.push(CertifiedModuleInterface {
            producer_sha256: producer,
            receipt: module.clone(),
            home_units: Arc::clone(&home_units),
            interface: interface.into(),
            package_imports: package_imports.into(),
            certificate: certificate.into(),
            core: core.map(Into::into),
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
    if certificate.len() > RECEIPT_LIMIT {
        return Err(CertificationError::Receipt("finalized certificate size"));
    }
    let mut cursor = std::io::Cursor::new(&certificate);
    let value: Value = ciborium::de::from_reader_with_recursion_limit(&mut cursor, 32)
        .map_err(|_| CertificationError::Receipt("finalized certificate encoding"))?;
    if cursor.position() != certificate.len() as u64 {
        return Err(CertificationError::Receipt(
            "finalized certificate trailing bytes",
        ));
    }
    let row = sized(&value, 12)?;
    if string(&row[0])? != "TPFINALMODULE"
        || number(&row[1])? != 1
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
    let envelope = decode_envelope(&value_array([
        row[2].clone(),
        row[4].clone(),
        value_array([descriptor_row]),
    ]))?;
    let receipt = envelope
        .modules
        .into_values()
        .next()
        .ok_or(CertificationError::Receipt("finalized certificate owner"))?;
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
        profile: FINALIZATION_PROFILE.into(),
        home_units: envelope.home_units,
        modules: BTreeMap::new(),
    };
    if canonical_certificate(producer, &canonical_envelope, &receipt)? != certificate {
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
        profile: FINALIZATION_PROFILE.into(),
        home_units,
        modules: BTreeMap::new(),
    };
    let certificate = canonical_certificate(producer, &envelope, &receipt).unwrap();
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
mod tests {
    use super::*;

    fn interface(core: Option<Vec<u8>>) -> CertifiedModuleInterface {
        let bytes = b"interface".to_vec();
        let packages = value_array([
            value_text("TPPKGROOTS"),
            value_text("2"),
            value_array([
                value_text("home-a"),
                value_text("Owner"),
                value_text(hex(&sha(&bytes))),
            ]),
            value_array([]),
            value_array([]),
        ]);
        let mut package_bytes = Vec::new();
        ciborium::ser::into_writer(&packages, &mut package_bytes).unwrap();
        fixture_interface(
            [7; 32],
            "home-a",
            "Owner",
            [8; 32],
            bytes,
            package_bytes,
            BTreeMap::new(),
            core,
        )
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
            profile: FINALIZATION_PROFILE.into(),
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
}
