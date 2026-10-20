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
    pub defining_interface: RecoveryPackageCodeInterfaceRef,
    pub import_interfaces: Vec<RecoveryPackageCodeInterfaceRef>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryPackageCodeInterfaceRef {
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
pub struct CertifiedPackageCodeInterface {
    unit: String,
    module: String,
    sha256: [u8; 32],
    bytes: Arc<[u8]>,
}

impl CertifiedPackageCodeInterface {
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

    fn from_world_output(
        program: &crate::compiler_world::CheckedWorldCellProgram,
        unit: &str,
        module: &str,
        witness: &PackageInterfaceWitness,
        operation: &InventoryOperation,
    ) -> CertResult<Arc<Self>> {
        let relative = witness
            .selected_path
            .strip_prefix(program.directory().path())
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
        let bytes = crate::checked_cell::world_output_bytes(program, relative)
            .map_err(|error| CertificationError::OriginalClosure(Box::new(error)))?;
        operation.charge(bytes.len())?;
        if sha(bytes) != witness.sha256 {
            return Err(CertificationError::Mismatch(
                "package interface captured digest",
            ));
        }
        operation.reserve::<Self>(1)?;
        operation.charge(unit.len().saturating_add(module.len()))?;
        Ok(Arc::new(Self {
            unit: unit.to_owned(),
            module: module.to_owned(),
            sha256: witness.sha256,
            bytes: Arc::clone(bytes),
        }))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedPackageCodeProduct {
    code: PackageGroupCode,
    bytes: Arc<[u8]>,
    product_sha256: [u8; 32],
    // Positional provenance for the same code.globals(), not a namespace map.
    import_interfaces: Arc<[[u8; 32]]>,
    defining_interface: Arc<CertifiedPackageCodeInterface>,
    import_interface_witnesses: Arc<[Arc<CertifiedPackageCodeInterface>]>,
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
        let envelope_sha256 = sha(&self.bytes);
        let envelope_path = envelope_path(&envelope_sha256);
        write(
            &root.join(&envelope_path),
            &self.bytes,
            &envelope_sha256,
            mode,
            validation,
        )?;
        let mut materialize_interface = |interface: &CertifiedPackageCodeInterface| {
            let path = interface_path(&interface.sha256);
            write(
                &root.join(&path),
                &interface.bytes,
                &interface.sha256,
                mode,
                validation,
            )?;
            Ok::<_, crate::recovery_artifacts::RecoveryArtifactError>(
                RecoveryPackageCodeInterfaceRef {
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
            .reserve::<Arc<CertifiedPackageCodeInterface>>(code.globals().len())?;
        validation
            .inventory
            .reserve::<[u8; 32]>(code.globals().len())?;
        validation.inventory.reserve::<Self>(1)?;
        let mut read_interface = |reference: &RecoveryPackageCodeInterfaceRef| -> CertResult<Arc<CertifiedPackageCodeInterface>> {
            if reference.unit.is_empty() || reference.module.is_empty()
                || reference.sha256 == [0; 32] || reference.path != interface_path(&reference.sha256) {
                return Err(invalid());
            }
            let bytes = read(&reference.path, &reference.sha256, validation)?;
            validation.inventory.reserve::<CertifiedPackageCodeInterface>(1)?;
            validation.inventory.charge(reference.unit.len() + reference.module.len())?;
            Ok(Arc::new(CertifiedPackageCodeInterface {
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
        Ok(Arc::new(Self {
            code,
            bytes,
            product_sha256,
            import_interfaces: import_interfaces.into(),
            defining_interface,
            import_interface_witnesses: import_interface_witnesses.into(),
        }))
    }

    pub fn defining_interface(&self) -> &Arc<CertifiedPackageCodeInterface> {
        &self.defining_interface
    }
    pub fn import_interface_witnesses(&self) -> &[Arc<CertifiedPackageCodeInterface>] {
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
    pub fn import_interfaces(&self) -> &[[u8; 32]] {
        &self.import_interfaces
    }

    /// Only an actual checked output can issue fresh package code. The worker
    /// declaration names captured bytes, while the held World supplies compiler
    /// and profile authority and the receipt supplies checked interface seals.
    pub(crate) fn from_world_output(
        program: &crate::compiler_world::CheckedWorldCellProgram,
        relative_prefix: &Path,
        output: &PackageProductOutput,
        receipt: &CertifiedReceipt,
        validation: &mut PackageInterfaceValidation,
    ) -> CertResult<Arc<Self>> {
        if !Arc::ptr_eq(&program.validation().inventory, &validation.inventory)
            || !receipt.package_products.contains(output)
            || relative_prefix.is_absolute()
            || relative_prefix
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(CertificationError::Mismatch("package code output owner"));
        }
        let relative = relative_prefix.join(&output.relative_path);
        let bytes = crate::checked_cell::world_output_bytes(program, &relative)
            .map_err(|error| CertificationError::OriginalClosure(Box::new(error)))?;
        validation.inventory.charge(bytes.len())?;
        if sha(bytes) != output.sha256 {
            return Err(CertificationError::Mismatch("package code output digest"));
        }
        let operation = Arc::clone(&validation.inventory);
        let code = parse_code(bytes, &operation)?;
        let owner = code.owner();
        if owner.compiler_sha256 != *program.producer_sha256()
            || owner.profile_sha256 != *program.logical().profile_sha256()
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
        let defining_interface = CertifiedPackageCodeInterface::from_world_output(
            program,
            &owner.unit,
            &owner.module,
            interface,
            &operation,
        )?;
        verify_package_interface(validation, interface)?;
        operation.reserve::<[u8; 32]>(code.globals().len())?;
        operation.reserve::<Arc<CertifiedPackageCodeInterface>>(code.globals().len())?;
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
            validate_package_owner(
                &global.identity.unit,
                &global.identity.module,
                &global.identity,
                &witness.sha256,
                &SourceGroupMap::new(),
                &receipt.packages,
                validation,
            )?;
            import_interface_witnesses.push(CertifiedPackageCodeInterface::from_world_output(
                program,
                &global.identity.unit,
                &global.identity.module,
                witness,
                &operation,
            )?);
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
        Ok(Arc::new(Self {
            code,
            bytes: Arc::clone(bytes),
            product_sha256,
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
            defining_interface: RecoveryPackageCodeInterfaceRef {
                unit: "package-unit".into(),
                module: "Package.Module".into(),
                path: interface_path(&[5; 32]),
                sha256: [5; 32],
            },
            import_interfaces: Vec::new(),
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
