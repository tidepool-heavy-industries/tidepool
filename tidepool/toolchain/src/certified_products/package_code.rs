//! Immutable package products admitted from one completed compiler output.
//! Structural group parsing alone never grants package execution authority.

use super::*;
use tidepool_repr::execution_schema::{PackageCodeOwner, PackageGroupCode};

fn parse_code(bytes: &[u8], operation: &InventoryOperation) -> CertResult<PackageGroupCode> {
    let value = operation.decode_value(
        bytes,
        COMPILER_RECEIPT_BYTES_LIMIT.min(operation.limits().max_module_bytes),
    )?;
    let packet = sized(&value, 3)?;
    if string(&packet[0])? != "TPPACKAGEGROUP1" {
        return Err(CertificationError::Receipt("package code envelope version"));
    }
    let fields = sized(&packet[1], 6)?;
    let raw = |value: &Value| -> CertResult<[u8; 32]> {
        let Value::Bytes(bytes) = value else {
            return Err(CertificationError::Receipt("package code owner digest"));
        };
        bytes
            .as_slice()
            .try_into()
            .map_err(|_| CertificationError::Receipt("package code owner digest"))
    };
    let owner = PackageCodeOwner {
        unit: string(&fields[0])?.to_owned(),
        module: string(&fields[1])?.to_owned(),
        compiler_sha256: raw(&fields[2])?,
        profile_sha256: raw(&fields[3])?,
        interface_sha256: raw(&fields[4])?,
        defining_version_sha256: raw(&fields[5])?,
    };
    if [
        owner.compiler_sha256,
        owner.profile_sha256,
        owner.interface_sha256,
        owner.defining_version_sha256,
    ]
    .contains(&[0; 32])
    {
        return Err(CertificationError::Mismatch(
            "package code defining provenance",
        ));
    }
    let Value::Bytes(group) = &packet[2] else {
        return Err(CertificationError::Receipt("package code group bytes"));
    };
    let requirements = crate::prepared_artifact::production_requirements()?;
    let group = operation.parse_projected_group(group, &requirements)?;
    Ok(PackageGroupCode::admit(owner, group)?)
}

/// Content-addressed package storage, separate from HOME/interface certificates.
/// The exact selected graph authenticates the artifact identity on restoration.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryPackageCodeRef {
    pub version: u8,
    pub unit: String,
    pub module: String,
    pub producer_sha256: [u8; 32],
    pub profile_sha256: [u8; 32],
    pub defining_version_sha256: [u8; 32],
    pub product_sha256: [u8; 32],
    pub envelope_path: PathBuf,
    pub envelope_sha256: [u8; 32],
    pub defining_interface: RecoveryPackageInterfaceRef,
    pub import_interfaces: Vec<RecoveryPackageInterfaceRef>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryPackageInterfaceRef {
    pub unit: String,
    pub module: String,
    pub path: PathBuf,
    pub sha256: [u8; 32],
}

fn envelope_path(sha: &[u8; 32]) -> PathBuf {
    PathBuf::from("package-code").join(format!("{}.cbor", hex(sha)))
}

fn interface_path(sha: &[u8; 32]) -> PathBuf {
    PathBuf::from("package-code").join(format!("{}.hi", hex(sha)))
}

/// The exact interface body captured by the same completed package producer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedPackageInterface {
    unit: String,
    module: String,
    sha256: [u8; 32],
    bytes: Arc<[u8]>,
}

impl CertifiedPackageInterface {
    pub fn unit(&self) -> &str {
        &self.unit
    }
    pub fn module(&self) -> &str {
        &self.module
    }
    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
    pub fn bytes(&self) -> &Arc<[u8]> {
        &self.bytes
    }

    fn from_native_output(
        program: &crate::artifacts::invocation_output::CapturedNativeOutput<'_>,
        unit: &str,
        module: &str,
        witness: &PackageInterfaceWitness,
        validation: &mut PackageInterfaceValidation,
    ) -> CertResult<Arc<Self>> {
        let relative = witness
            .selected_path
            .strip_prefix(program.directory())
            .map_err(|_| {
                CertificationError::Mismatch("package interface is outside completed output")
            })?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(CertificationError::Mismatch(
                "package interface output path",
            ));
        }
        let bytes = program
            .bytes(relative)
            .map_err(|error| CertificationError::OriginalClosure(Box::new(error)))?;
        validation
            .verify_captured(
                &witness.selected_path,
                &program.physical_directory().join(relative),
                &witness.sha256,
                bytes,
            )
            .map_err(package_validation_error)?;
        validation.inventory.reserve::<Self>(1)?;
        validation
            .inventory
            .charge(unit.len().saturating_add(module.len()))?;
        Ok(Arc::new(Self {
            unit: unit.to_owned(),
            module: module.to_owned(),
            sha256: witness.sha256,
            bytes: Arc::clone(bytes),
        }))
    }
}

/// Immutable package interface custody. Selection only narrows this owner's
/// actual captured tokens; it never grants compiler or executable authority.
#[derive(Clone, Debug)]
pub(crate) struct PackageInterfaceCustody {
    interfaces: Arc<BTreeMap<(String, String), Arc<CertifiedPackageInterface>>>,
}

impl PackageInterfaceCustody {
    /// Independent recovery/candidate admission keeps its current selected-path
    /// authentication. Completed output uses `from_native_output` instead.
    pub(super) fn capture_verified_packages(
        packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
        validation: &mut PackageInterfaceValidation,
    ) -> CertResult<Self> {
        let mut interfaces = BTreeMap::new();
        for (owner, witness) in packages {
            verify_package_interface(validation, witness)?;
            let bytes = validation.capture_owned_package_interface(&witness.selected_path, &witness.sha256)
                .map_err(package_validation_error)?;
            validation.inventory.reserve::<CertifiedPackageInterface>(1)?;
            validation.inventory.reserve::<((String,String), Arc<CertifiedPackageInterface>)>(1)?;
            validation.inventory.charge(owner.0.len().saturating_add(owner.1.len()).saturating_mul(2))?;
            interfaces.insert(owner.clone(), Arc::new(CertifiedPackageInterface {
                unit: owner.0.clone(), module: owner.1.clone(), sha256: witness.sha256, bytes,
            }));
        }
        Ok(Self { interfaces: Arc::new(interfaces) })
    }

    pub(crate) fn from_native_output(
        output: &crate::artifacts::invocation_output::CapturedNativeOutput<'_>,
        receipt: &CertifiedReceipt,
        validation: &mut PackageInterfaceValidation,
    ) -> CertResult<Self> {
        if !Arc::ptr_eq(output.operation(), &validation.inventory) {
            return Err(CertificationError::Mismatch("package interface inventory owner"));
        }
        let mut interfaces = BTreeMap::new();
        for ((unit, module), witness) in &receipt.packages {
            crate::host_work::checkpoint().map_err(CertificationError::Interrupted)?;
            let token = CertifiedPackageInterface::from_native_output(
                output, unit, module, witness, validation,
            )?;
            validation.inventory.reserve::<((String, String), Arc<CertifiedPackageInterface>)>(1)?;
            validation.inventory.charge(unit.len().saturating_add(module.len()))?;
            interfaces.insert((unit.clone(), module.clone()), token);
        }
        Ok(Self { interfaces: Arc::new(interfaces) })
    }

    pub(crate) fn get(&self, owner: &(String, String)) -> Option<&Arc<CertifiedPackageInterface>> {
        self.interfaces.get(owner)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&(String, String), &Arc<CertifiedPackageInterface>)> {
        self.interfaces.iter()
    }

    pub(crate) fn is_empty(&self) -> bool { self.interfaces.is_empty() }

    pub(crate) fn matches_references(&self, references: &[RecoveryPackageInterfaceRef]) -> bool {
        self.interfaces.len() == references.len()
            && self.interfaces.iter().zip(references).all(|((owner, token), reference)|
                (&reference.unit, &reference.module) == (&owner.0, &owner.1)
                    && reference.sha256 == *token.sha256()
                    && reference.path == interface_path(token.sha256()))
    }

    pub(crate) fn materialize(
        &self,
        root: &Path,
        mode: crate::recovery_artifacts::MaterializationMode,
        validation: &mut PackageInterfaceValidation,
    ) -> CertResult<Vec<RecoveryPackageInterfaceRef>> {
        use crate::recovery_artifacts::materialize_copy_with_validation;
        std::fs::create_dir_all(root.join("package-code"))
            .map_err(|error| CertificationError::CapturedModulePayload(error.into()))?;
        let mut references = Vec::new();
        for ((unit, module), token) in self.interfaces.iter() {
            let path = interface_path(token.sha256());
            materialize_copy_with_validation(
                &root.join(&path), token.bytes(), token.sha256(), mode, validation,
            ).map_err(CertificationError::CapturedModulePayload)?;
            validation.inventory.reserve::<RecoveryPackageInterfaceRef>(1)?;
            validation.inventory.charge(unit.len().saturating_add(module.len()))?;
            references.push(RecoveryPackageInterfaceRef {
                unit: unit.clone(), module: module.clone(), path, sha256: *token.sha256(),
            });
        }
        Ok(references)
    }

    pub(crate) fn restore(
        root: &Path,
        references: &[RecoveryPackageInterfaceRef],
        expected: &BTreeMap<(String, String), [u8; 32]>,
        validation: &mut PackageInterfaceValidation,
    ) -> CertResult<Self> {
        if references.len() != expected.len()
            || !expected.iter().zip(references).all(|((owner, seal), reference)|
                (&reference.unit, &reference.module) == (&owner.0, &owner.1)
                    && &reference.sha256 == seal && reference.path == interface_path(seal)) {
            return Err(CertificationError::Mismatch("package interface recovery obligations"));
        }
        let mut interfaces = BTreeMap::new();
        for reference in references {
            let physical = crate::recovery_artifacts::resolve_owned(root, &reference.path)
                .map_err(CertificationError::CapturedModulePayload)?;
            let bytes = validation.capture_owned_package_interface(&physical, &reference.sha256)
                .map_err(CertificationError::CapturedModulePayload)?;
            validation.inventory.reserve::<CertifiedPackageInterface>(1)?;
            validation.inventory.charge(reference.unit.len().saturating_add(reference.module.len()))?;
            let token = Arc::new(CertifiedPackageInterface {
                unit: reference.unit.clone(), module: reference.module.clone(),
                sha256: reference.sha256, bytes,
            });
            validation.inventory.reserve::<((String, String), Arc<CertifiedPackageInterface>)>(1)?;
            validation.inventory.charge(reference.unit.len().saturating_add(reference.module.len()))?;
            interfaces.insert((reference.unit.clone(), reference.module.clone()), token);
        }
        Ok(Self { interfaces: Arc::new(interfaces) })
    }

    pub(crate) fn select(
        &self,
        expected: &BTreeMap<(String, String), [u8; 32]>,
        validation: &mut PackageInterfaceValidation,
    ) -> CertResult<Self> {
        let mut interfaces = BTreeMap::new();
        for (owner, seal) in expected {
            crate::host_work::checkpoint().map_err(CertificationError::Interrupted)?;
            let (unit, module) = owner;
            let token = self.interfaces.get(owner)
                .filter(|token| token.sha256() == seal)
                .ok_or(CertificationError::Mismatch("required captured package interface"))?;
            validation.inventory.reserve::<((String, String), Arc<CertifiedPackageInterface>)>(1)?;
            validation.inventory.charge(unit.len().saturating_add(module.len()))?;
            interfaces.insert((unit.clone(), module.clone()), Arc::clone(token));
        }
        Ok(Self { interfaces: Arc::new(interfaces) })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedPackageCodeProduct {
    code: PackageGroupCode,
    bytes: Arc<[u8]>,
    // Authenticated by the issuer before retaining these immutable bytes.
    envelope_sha256: [u8; 32],
    product_sha256: [u8; 32],
    descriptor: crate::artifact_inventory::ArtifactDescriptor,
    // Positional provenance for the same code.globals(), not a namespace map.
    import_interfaces: Arc<[[u8; 32]]>,
    defining_interface: Arc<CertifiedPackageInterface>,
    import_interface_witnesses: Arc<[Arc<CertifiedPackageInterface>]>,
}

impl CertifiedPackageCodeProduct {
    pub(crate) fn materialize(
        &self,
        root: &Path,
        mode: crate::recovery_artifacts::MaterializationMode,
        validation: &mut PackageInterfaceValidation,
    ) -> Result<RecoveryPackageCodeRef, crate::recovery_artifacts::RecoveryArtifactError> {
        use crate::recovery_artifacts::materialize_copy_with_validation as write;
        std::fs::create_dir_all(root.join("package-code"))?;
        let envelope_sha256 = self.envelope_sha256;
        let envelope_path = envelope_path(&envelope_sha256);
        write(
            &root.join(&envelope_path),
            &self.bytes,
            &envelope_sha256,
            mode,
            validation,
        )?;
        let mut materialize_interface = |interface: &CertifiedPackageInterface| {
            let path = interface_path(&interface.sha256);
            write(
                &root.join(&path),
                &interface.bytes,
                &interface.sha256,
                mode,
                validation,
            )?;
            Ok::<_, crate::recovery_artifacts::RecoveryArtifactError>(
                RecoveryPackageInterfaceRef {
                    unit: interface.unit.clone(),
                    module: interface.module.clone(),
                    path,
                    sha256: interface.sha256,
                },
            )
        };
        let defining_interface = materialize_interface(&self.defining_interface)?;
        let import_interfaces = self
            .import_interface_witnesses
            .iter()
            .map(|interface| materialize_interface(interface))
            .collect::<Result<_, _>>()?;
        let owner = self.code.owner();
        Ok(RecoveryPackageCodeRef {
            version: 1,
            unit: owner.unit.clone(),
            module: owner.module.clone(),
            producer_sha256: owner.compiler_sha256,
            profile_sha256: owner.profile_sha256,
            defining_version_sha256: owner.defining_version_sha256,
            product_sha256: self.product_sha256,
            envelope_path,
            envelope_sha256,
            defining_interface,
            import_interfaces,
        })
    }

    /// Restore custody from explicit, versioned references. It does not select
    /// executable roots or restore compiler roles; the exact graph does that.
    pub(crate) fn restore(
        root: &Path,
        reference: &RecoveryPackageCodeRef,
        validation: &mut PackageInterfaceValidation,
    ) -> CertResult<Arc<Self>> {
        use crate::recovery_artifacts::{read_checked, resolve_owned};
        let invalid = || CertificationError::Mismatch("package code recovery reference");
        if reference.version != 1
            || reference.envelope_path != envelope_path(&reference.envelope_sha256)
        {
            return Err(invalid());
        }
        let read =
            |relative: &Path, sha: &[u8; 32], validation: &mut PackageInterfaceValidation| {
                let path = resolve_owned(root, relative)
                    .map_err(CertificationError::CapturedModulePayload)?;
                read_checked(
                    &path,
                    sha,
                    validation.inventory.limits().max_module_bytes as u64,
                    validation,
                )
                .map(Arc::<[u8]>::from)
                .map_err(CertificationError::CapturedModulePayload)
            };
        let bytes = read(
            &reference.envelope_path,
            &reference.envelope_sha256,
            validation,
        )?;
        let code = parse_code(&bytes, &validation.inventory)?;
        let owner = code.owner();
        if owner.unit != reference.unit
            || owner.module != reference.module
            || owner.compiler_sha256 != reference.producer_sha256
            || owner.profile_sha256 != reference.profile_sha256
            || owner.defining_version_sha256 != reference.defining_version_sha256
            || reference.import_interfaces.len() != code.globals().len()
        {
            return Err(invalid());
        }
        validation
            .inventory
            .reserve::<Arc<CertifiedPackageInterface>>(code.globals().len())?;
        validation
            .inventory
            .reserve::<[u8; 32]>(code.globals().len())?;
        validation.inventory.reserve::<Self>(1)?;
        let mut read_interface = |reference: &RecoveryPackageInterfaceRef| -> CertResult<Arc<CertifiedPackageInterface>> {
            if reference.unit.is_empty() || reference.module.is_empty()
                || reference.sha256 == [0; 32] || reference.path != interface_path(&reference.sha256) {
                return Err(invalid());
            }
            let bytes = read(&reference.path, &reference.sha256, validation)?;
            validation.inventory.reserve::<CertifiedPackageInterface>(1)?;
            validation.inventory.charge(reference.unit.len() + reference.module.len())?;
            Ok(Arc::new(CertifiedPackageInterface {
                unit: reference.unit.clone(), module: reference.module.clone(),
                sha256: reference.sha256, bytes,
            }))
        };
        let defining_interface = read_interface(&reference.defining_interface)?;
        if defining_interface.unit != owner.unit
            || defining_interface.module != owner.module
            || defining_interface.sha256 != owner.interface_sha256
        {
            return Err(invalid());
        }
        let import_interface_witnesses = reference
            .import_interfaces
            .iter()
            .map(&mut read_interface)
            .collect::<CertResult<Vec<_>>>()?;
        let mut identity = Sha256::new();
        identity.update(b"tidepool.package-code-product.v1\0");
        identity.update(reference.envelope_sha256);
        let mut import_interfaces = Vec::with_capacity(code.globals().len());
        for (global, interface) in code.globals().iter().zip(&import_interface_witnesses) {
            if global.identity.unit != interface.unit || global.identity.module != interface.module
            {
                return Err(invalid());
            }
            identity.update(interface.sha256);
            import_interfaces.push(interface.sha256);
        }
        let product_sha256: [u8; 32] = identity.finalize().into();
        if product_sha256 != reference.product_sha256 {
            return Err(invalid());
        }
        validation
            .inventory
            .charge(owner.unit.len().saturating_add(owner.module.len()))?;
        let descriptor = crate::artifact_inventory::ArtifactDescriptor::package_code(
            &owner.unit,
            &owner.module,
            owner.compiler_sha256,
            owner.interface_sha256,
            product_sha256,
            import_interfaces.iter().copied(),
        );
        Ok(Arc::new(Self {
            code,
            bytes,
            envelope_sha256: reference.envelope_sha256,
            product_sha256,
            descriptor,
            import_interfaces: import_interfaces.into(),
            defining_interface,
            import_interface_witnesses: import_interface_witnesses.into(),
        }))
    }

    pub fn defining_interface(&self) -> &Arc<CertifiedPackageInterface> {
        &self.defining_interface
    }
    pub fn import_interface_witnesses(&self) -> &[Arc<CertifiedPackageInterface>] {
        &self.import_interface_witnesses
    }

    pub fn code(&self) -> &PackageGroupCode {
        &self.code
    }
    pub fn bytes(&self) -> &Arc<[u8]> {
        &self.bytes
    }
    pub fn product_sha256(&self) -> &[u8; 32] {
        &self.product_sha256
    }
    pub(crate) fn descriptor(&self) -> &crate::artifact_inventory::ArtifactDescriptor {
        &self.descriptor
    }
    pub(crate) fn artifact_id(&self) -> crate::artifact_inventory::ArtifactId {
        self.descriptor.id
    }
    pub fn import_interfaces(&self) -> &[[u8; 32]] {
        &self.import_interfaces
    }

    /// Only an actual checked output can issue fresh package code. The worker
    /// declaration names captured bytes, while the captured native output supplies compiler
    /// and profile authority and the receipt supplies checked interface seals.
    pub(crate) fn from_native_output(
        program: &crate::artifacts::invocation_output::CapturedNativeOutput<'_>,
        relative_prefix: &Path,
        output: &PackageProductOutput,
        receipt: &CertifiedReceipt,
        interfaces: &PackageInterfaceCustody,
        validation: &mut PackageInterfaceValidation,
    ) -> CertResult<Arc<Self>> {
        if !Arc::ptr_eq(program.operation(), &validation.inventory)
            || !receipt.package_products.contains(output)
            || relative_prefix.is_absolute()
            || relative_prefix
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(CertificationError::Mismatch("package code output owner"));
        }
        let relative = relative_prefix.join(&output.relative_path);
        let bytes = program
            .bytes(&relative)
            .map_err(|error| CertificationError::OriginalClosure(Box::new(error)))?;
        validation.inventory.charge(bytes.len())?;
        if sha(bytes) != output.sha256 {
            return Err(CertificationError::Mismatch("package code output digest"));
        }
        let operation = Arc::clone(&validation.inventory);
        let code = parse_code(bytes, &operation)?;
        let owner = code.owner();
        if owner.compiler_sha256 != *program.producer_sha256()
            || owner.profile_sha256 != *program.profile_sha256()
            || receipt.finalization.home_units.contains(&owner.unit)
        {
            return Err(CertificationError::Mismatch(
                "package code defining provenance",
            ));
        }
        let interface = receipt
            .packages
            .get(&(owner.unit.clone(), owner.module.clone()))
            .filter(|interface| interface.sha256 == owner.interface_sha256)
            .ok_or(CertificationError::Mismatch(
                "package code defining interface",
            ))?;
        let defining_interface = interfaces.get(&(owner.unit.clone(), owner.module.clone()))
            .filter(|token| token.sha256() == &interface.sha256)
            .cloned().ok_or(CertificationError::Mismatch("package code defining custody"))?;
        operation.reserve::<[u8; 32]>(code.globals().len())?;
        operation.reserve::<Arc<CertifiedPackageInterface>>(code.globals().len())?;
        let mut import_interface_witnesses = Vec::with_capacity(code.globals().len());
        let mut import_interfaces = Vec::with_capacity(code.globals().len());
        for global in code.globals() {
            crate::host_work::checkpoint().map_err(CertificationError::Interrupted)?;
            if receipt
                .finalization
                .home_units
                .contains(&global.identity.unit)
            {
                return Err(CertificationError::Mismatch(
                    "package code imports a HOME owner",
                ));
            }
            let witness = receipt
                .packages
                .get(&(global.identity.unit.clone(), global.identity.module.clone()))
                .ok_or(CertificationError::Mismatch(
                    "package code import interface",
                ))?;
            let witness = package_owner_witness(
                &global.identity.unit,
                &global.identity.module,
                &global.identity,
                &witness.sha256,
                &SourceGroupMap::new(),
                &receipt.packages,
            )?;
            let captured = interfaces.get(&(global.identity.unit.clone(),global.identity.module.clone()))
                .filter(|token| token.sha256() == &witness.sha256)
                .cloned().ok_or(CertificationError::Mismatch("package code import custody"))?;
            import_interface_witnesses.push(captured);
            import_interfaces.push(witness.sha256);
        }
        // Import seals are part of product identity. Exact dependency binding
        // choices belong to the separately issued selected graph.
        let mut identity = Sha256::new();
        identity.update(b"tidepool.package-code-product.v1\0");
        identity.update(output.sha256);
        for seal in &import_interfaces {
            identity.update(seal);
        }
        let product_sha256 = identity.finalize().into();
        operation.reserve::<Self>(1)?;
        operation.charge(owner.unit.len().saturating_add(owner.module.len()))?;
        let descriptor = crate::artifact_inventory::ArtifactDescriptor::package_code(
            &owner.unit,
            &owner.module,
            owner.compiler_sha256,
            owner.interface_sha256,
            product_sha256,
            import_interfaces.iter().copied(),
        );
        Ok(Arc::new(Self {
            code,
            bytes: Arc::clone(bytes),
            envelope_sha256: output.sha256,
            product_sha256,
            descriptor,
            import_interfaces: import_interfaces.into(),
            defining_interface,
            import_interface_witnesses: import_interface_witnesses.into(),
        }))
    }
}

#[cfg(test)]
mod storage_tests {
    use super::*;
    use crate::recovery_artifacts::RecoveryArtifactError;

    // Reference data cannot issue a product. These controls stop before any
    // valid package envelope and exercise the actual bounded storage reader.
    fn reference(bytes: &[u8]) -> RecoveryPackageCodeRef {
        let digest = sha(bytes);
        RecoveryPackageCodeRef {
            version: 1,
            unit: "package-unit".into(),
            module: "Package.Module".into(),
            producer_sha256: [1; 32],
            profile_sha256: [2; 32],
            defining_version_sha256: [3; 32],
            product_sha256: [4; 32],
            envelope_path: envelope_path(&digest),
            envelope_sha256: digest,
            defining_interface: RecoveryPackageInterfaceRef {
                unit: "package-unit".into(),
                module: "Package.Module".into(),
                path: interface_path(&[5; 32]),
                sha256: [5; 32],
            },
            import_interfaces: Vec::new(),
        }
    }

    #[test]
    fn package_descriptor_preserves_captured_reference_identities() {
        #[derive(serde::Deserialize)]
        struct Fixture {
            label: String,
            reference: RecoveryPackageCodeRef,
            expected_id: crate::artifact_inventory::ArtifactId,
        }
        // Fixed metadata includes captured compiler outputs and unissued ordered,
        // reversed and repeated-seal references. It cannot issue product authority.
        let fixtures: Vec<Fixture> = serde_json::from_str(include_str!(
            "../../tests/fixtures/package-code-descriptor-identities.json"
        ))
        .unwrap();
        assert_eq!(fixtures.len(), 6);
        eprintln!(
            "retained package descriptor: {} inline bytes plus owned unit/module text",
            std::mem::size_of::<crate::artifact_inventory::ArtifactDescriptor>(),
        );
        for fixture in fixtures {
            assert_eq!(
                crate::artifact_inventory::ArtifactDescriptor::from_recovery_package_code(
                    &fixture.reference,
                )
                .id,
                fixture.expected_id,
                "{}",
                fixture.label,
            );
        }
    }

    #[test]
    fn package_storage_preserves_missing_and_corrupt_refusals() {
        let root = tempfile::tempdir().unwrap();
        let reference = reference(b"expected envelope");
        let restore = || {
            CertifiedPackageCodeProduct::restore(
                root.path(),
                &reference,
                &mut PackageInterfaceValidation::default(),
            )
        };
        assert!(matches!(
            restore(),
            Err(CertificationError::CapturedModulePayload(
                RecoveryArtifactError::Unavailable(_)
            ))
        ));
        let mut acquisition =
            crate::declaration_context::RecoveredArtifactAcquisition::new(root.path());
        let payloads = [
            crate::declaration_context::RecoveryPayloadReference::PackageCode(reference.clone()),
        ];
        let descriptors = [
            crate::artifact_inventory::ArtifactDescriptor::from_recovery_package_code(&reference),
        ];
        let mut capture = || acquisition.capture_payloads_bound(&payloads, &descriptors, &[], &[]);
        assert!(
            matches!(capture(), Err(crate::declaration_context::RecoveryInventoryError::Certification(
            crate::CompileError::CompilerEvidence(error))) if matches!(error.as_ref(),
                CertificationError::CapturedModulePayload(RecoveryArtifactError::Unavailable(_))))
        );
        std::fs::create_dir_all(root.path().join("package-code")).unwrap();
        std::fs::write(
            root.path().join(&reference.envelope_path),
            b"corrupt envelope",
        )
        .unwrap();
        assert!(matches!(
            restore(),
            Err(CertificationError::CapturedModulePayload(
                RecoveryArtifactError::DigestMismatch(_)
            ))
        ));
        assert!(
            matches!(capture(), Err(crate::declaration_context::RecoveryInventoryError::Certification(
            crate::CompileError::CompilerEvidence(error))) if matches!(error.as_ref(),
                CertificationError::CapturedModulePayload(RecoveryArtifactError::DigestMismatch(_))))
        );
    }

    #[test]
    fn package_storage_requires_versioned_content_addressed_references() {
        let root = tempfile::tempdir().unwrap();
        let original = reference(b"unissued envelope");
        for changed in [
            RecoveryPackageCodeRef {
                version: 0,
                ..original.clone()
            },
            RecoveryPackageCodeRef {
                envelope_path: PathBuf::from("../outside"),
                ..original.clone()
            },
        ] {
            assert!(matches!(
                CertifiedPackageCodeProduct::restore(
                    root.path(),
                    &changed,
                    &mut PackageInterfaceValidation::default()
                ),
                Err(CertificationError::Mismatch(
                    "package code recovery reference"
                ))
            ));
        }
        let mut missing = serde_json::to_value(&original).unwrap();
        missing
            .as_object_mut()
            .unwrap()
            .remove("defining_interface");
        assert!(serde_json::from_value::<RecoveryPackageCodeRef>(missing).is_err());
    }
}
