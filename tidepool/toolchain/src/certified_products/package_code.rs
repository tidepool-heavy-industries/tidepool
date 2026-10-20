//! Immutable package products admitted from one completed compiler output.
//! Structural group parsing alone never grants package execution authority.

use super::*;
use tidepool_repr::execution_schema::{PackageCodeOwner, PackageGroupCode};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedPackageCodeProduct {
    code: PackageGroupCode,
    bytes: Arc<[u8]>,
    product_sha256: [u8; 32],
    // Positional provenance for the same code.globals(), not a namespace map.
    import_interfaces: Arc<[[u8; 32]]>,
}

impl CertifiedPackageCodeProduct {
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
        if sha(bytes) != output.sha256 {
            return Err(CertificationError::Mismatch("package code output digest"));
        }
        let operation = Arc::clone(&validation.inventory);
        let value = operation.decode_value(
            bytes,
            COMPILER_RECEIPT_BYTES_LIMIT.min(operation.limits().max_module_bytes),
        )?;
        let packet = sized(&value, 3)?;
        if string(&packet[0])? != "TPPACKAGEGROUP1" {
            return Err(CertificationError::Receipt("package code envelope version"));
        }
        let owner = sized(&packet[1], 6)?;
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
            unit: string(&owner[0])?.to_owned(),
            module: string(&owner[1])?.to_owned(),
            compiler_sha256: raw(&owner[2])?,
            profile_sha256: raw(&owner[3])?,
            interface_sha256: raw(&owner[4])?,
            defining_version_sha256: raw(&owner[5])?,
        };
        if owner.compiler_sha256 != *program.producer_sha256()
            || owner.profile_sha256 != *program.logical().profile_sha256()
            || owner.interface_sha256 == [0; 32]
            || owner.defining_version_sha256 == [0; 32]
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
        verify_package_interface(validation, interface)?;
        let Value::Bytes(group) = &packet[2] else {
            return Err(CertificationError::Receipt("package code group bytes"));
        };
        let requirements = crate::prepared_artifact::production_requirements()?;
        let group = operation.parse_projected_group(group, &requirements)?;
        let code = PackageGroupCode::admit(owner, group)?;
        operation.reserve::<[u8; 32]>(code.globals().len())?;
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
        }))
    }
}
