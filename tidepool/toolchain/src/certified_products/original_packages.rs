//! Receipt-local package choices joined to the retained exact Original demand.
use super::*;
use crate::artifact_inventory::NativeGroupBinding;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct OriginalPackageRootKey {
    pub(super) target: String,
    reference: OriginalPackageUse,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct OriginalPackageUse {
    unit: String,
    module: String,
    interface: crate::artifact_inventory::ArtifactBinding,
    original: crate::artifact_inventory::ArtifactBinding,
    binder: SymbolIdentity,
}

impl OriginalPackageUse {
    fn receipt_owner(&self, operation: &InventoryOperation) -> CertResult<ReceiptImportOwner> {
        operation.charge(self.unit.len().saturating_add(self.module.len()))?;
        charge_symbol(operation, &self.binder)?;
        Ok(ReceiptImportOwner::OriginalBinding {
            unit: self.unit.clone(),
            module: self.module.clone(),
            interface: self.interface,
            original: self.original,
            binder: self.binder.clone(),
        })
    }
}

pub(super) type OriginalPackageRoots =
    BTreeMap<OriginalPackageRootKey, Vec<(SymbolIdentity, PackageLink)>>;

pub(super) fn decode_roots(
    value: &Value,
    operation: &InventoryOperation,
) -> CertResult<OriginalPackageRoots> {
    let mut roots = BTreeMap::new();
    for row in array(value)? {
        let row = sized(row, 2)?;
        let key = sized(&row[0], 2)?;
        operation.reserve::<OriginalPackageRootKey>(1)?;
        let target = string(&key[0])?;
        operation.charge(target.len())?;
        let target = target.to_owned();
        if target.is_empty() || target.contains('\0') {
            return Err(CertificationError::Receipt("Original package target"));
        }
        let use_row = sized(&key[1], 5)?;
        let unit = string(&use_row[0])?;
        let module = string(&use_row[1])?;
        if unit.is_empty() || module.is_empty() || unit.contains('\0') || module.contains('\0') {
            return Err(CertificationError::Receipt("Original package use owner"));
        }
        operation.charge(unit.len().saturating_add(module.len()))?;
        operation.charge_value_copies(&use_row[4], 1)?;
        let reference = OriginalPackageUse {
            unit: unit.to_owned(),
            module: module.to_owned(),
            interface: receipt_artifact_binding(&use_row[2])?,
            original: receipt_artifact_binding(&use_row[3])?,
            binder: identity(&use_row[4])?,
        };
        let key = OriginalPackageRootKey { target, reference };
        if roots
            .last_key_value()
            .is_some_and(|(previous, _)| previous >= &key)
        {
            return Err(CertificationError::Receipt(
                "Original package uses are not unique and ordered",
            ));
        }
        let mut links: Vec<(SymbolIdentity, PackageLink)> = Vec::new();
        for entry in array(&row[1])? {
            let entry = sized(entry, 2)?;
            operation.reserve::<(SymbolIdentity, PackageLink)>(1)?;
            operation.charge_value_copies(&entry[0], 1)?;
            let identity = identity(&entry[0])?;
            if links
                .last()
                .is_some_and(|(previous, _)| previous >= &identity)
            {
                return Err(CertificationError::Receipt(
                    "Original package identities are not unique and ordered",
                ));
            }
            links.push((identity, decode_package_link(&entry[1])?));
        }
        roots.insert(key, links);
    }
    Ok(roots)
}

pub(super) fn encode_roots(roots: &OriginalPackageRoots) -> Value {
    value_array(roots.iter().map(|(key, links)| {
        let OriginalPackageUse {
            unit,
            module,
            interface,
            original,
            binder,
        } = &key.reference;
        let binding = |binding: &crate::artifact_inventory::ArtifactBinding| {
            value_array([
                Value::Bytes(binding.artifact.0.to_vec()),
                Value::Bytes(binding.selection.0.to_vec()),
            ])
        };
        value_array([
            value_array([
                value_text(&key.target),
                value_array([
                    value_text(unit),
                    value_text(module),
                    binding(interface),
                    binding(original),
                    value_identity(binder),
                ]),
            ]),
            value_array(links.iter().map(|(identity, link)| {
                value_array([
                    value_identity(identity),
                    value_array([
                        value_text("local"),
                        Value::Integer((link.occurrence() as u64).into()),
                    ]),
                ])
            })),
        ])
    }))
}

/// Only the retained stage demand can issue group/slot package associations.
/// Local receipt links cannot manufacture this owner.
#[derive(Debug)]
pub(crate) struct CheckedOriginalPackageAssociations {
    stage: Option<Arc<crate::compiler_world::native_demand::CheckedWorldStageNativeInputs>>,
    slots: BTreeMap<(NativeGroupBinding, u32), CheckedOriginalPackageSlot>,
    use_roots: BTreeSet<NativeGroupBinding>,
}
#[derive(Debug)]
struct CheckedOriginalPackageSlot {
    identity: SymbolIdentity,
    interface_digest: [u8; 32],
    link: PackageLink,
}
impl CheckedOriginalPackageAssociations {
    pub(crate) fn slot(
        &self,
        group: NativeGroupBinding,
        index: u32,
        identity: &SymbolIdentity,
        interface_digest: &[u8; 32],
    ) -> CertResult<Option<&PackageLink>> {
        let Some(slot) = self.slots.get(&(group, index)) else {
            return Ok(None);
        };
        if &slot.identity != identity || &slot.interface_digest != interface_digest {
            return Err(CertificationError::Mismatch(
                "Original package slot differs from selected native import",
            ));
        }
        Ok(Some(&slot.link))
    }
    pub(crate) fn use_roots(&self) -> &BTreeSet<NativeGroupBinding> {
        &self.use_roots
    }
    pub(crate) fn coordinates(&self) -> impl Iterator<Item = (NativeGroupBinding, u32)> + '_ {
        self.slots.keys().copied()
    }
}

impl CheckedOriginalPackageAssociations {
    pub(super) fn issue_item(
        selection: &PackageSelection,
        manifest: &PackageLinks,
        original_demand: Option<
            Arc<crate::compiler_world::native_demand::CheckedWorldStageNativeInputs>,
        >,
        operation: &InventoryOperation,
    ) -> Result<Arc<Self>, crate::CompileError> {
        let target = selection.target.as_str();
        let roots = &selection.original_use_roots;
        let occurrences = &manifest.occurrences;
        let failure = |message| {
            crate::CompileError::CompilerEvidence(Box::new(CertificationError::Mismatch(message)))
        };
        let mut slots = BTreeMap::new();
        let mut use_roots = BTreeSet::new();
        for (key, links) in roots.iter().filter(|(key, _)| key.target == target) {
            crate::host_work::checkpoint()?;
            let stage = original_demand.as_ref().ok_or_else(|| {
                failure("Original package association lacks its issued World stage")
            })?;
            if !std::ptr::eq(stage.operation().as_ref(), operation) {
                return Err(failure(
                    "Original package association belongs to another operation",
                ));
            }
            let demand = stage.selection().ok_or_else(|| {
                failure("Original package association exceeds empty stage demand")
            })?;
            let reference = key
                .reference
                .receipt_owner(operation)
                .map_err(|error| crate::CompileError::CompilerEvidence(Box::new(error)))?;
            let (root, _) = demand
                .selected_groups()
                .original_use_row(&reference)
                .ok_or_else(|| failure("Original package use exceeds exact stage demand"))?;
            operation
                .reserve::<NativeGroupBinding>(1)
                .map_err(|error| crate::CompileError::CompilerEvidence(Box::new(error.into())))?;
            use_roots.insert(root);
            let obligations = demand
                .selected_groups()
                .package_obligations_for_use(&reference)?;
            operation
                .reserve::<&SymbolIdentity>(obligations.len().saturating_add(links.len()))
                .map_err(|error| crate::CompileError::CompilerEvidence(Box::new(error.into())))?;
            let expected = obligations
                .iter()
                .map(|obligation| &obligation.identity)
                .collect::<BTreeSet<_>>();
            let actual = links
                .iter()
                .map(|(identity, _)| identity)
                .collect::<BTreeSet<_>>();
            if expected != actual {
                return Err(failure(
                    "Original package identities differ from exact demand obligations",
                ));
            }
            operation
                .reserve::<(&SymbolIdentity, &PackageLink)>(links.len())
                .map_err(|error| crate::CompileError::CompilerEvidence(Box::new(error.into())))?;
            let by_identity = links
                .iter()
                .map(|(identity, link)| (identity, link))
                .collect::<BTreeMap<_, _>>();
            for obligation in obligations {
                let link = by_identity[&obligation.identity];
                let occurrence = occurrences
                    .get(link.occurrence() as usize)
                    .ok_or_else(|| failure("Original package occurrence is out of range"))?;
                if occurrence.unit != obligation.identity.unit
                    || occurrence.module != obligation.identity.module
                    || occurrence.defining_interface_sha256 != obligation.interface_digest
                {
                    return Err(failure(
                        "Original package occurrence differs from exact obligation owner or seal",
                    ));
                }
                let coordinate = (obligation.demander, obligation.global_index);
                if let Some(previous) = slots.get(&coordinate) {
                    let previous: &CheckedOriginalPackageSlot = previous;
                    if previous.identity != obligation.identity
                        || previous.interface_digest != obligation.interface_digest
                        || previous.link != *link
                    {
                        return Err(failure(
                            "Original uses conflict on an exact native package slot",
                        ));
                    }
                } else {
                    operation
                        .reserve::<CheckedOriginalPackageSlot>(1)
                        .map_err(|error| {
                            crate::CompileError::CompilerEvidence(Box::new(error.into()))
                        })?;
                    charge_symbol(operation, &obligation.identity)
                        .map_err(|error| crate::CompileError::CompilerEvidence(Box::new(error)))?;
                    slots.insert(
                        coordinate,
                        CheckedOriginalPackageSlot {
                            identity: obligation.identity.clone(),
                            interface_digest: obligation.interface_digest,
                            link: link.clone(),
                        },
                    );
                }
            }
        }
        Ok(Arc::new(CheckedOriginalPackageAssociations {
            stage: original_demand,
            slots,
            use_roots,
        }))
    }
}

impl BoundPackageSelection {
    pub(crate) fn checked_original_packages(
        &self,
        operation: &InventoryOperation,
    ) -> Result<Arc<CheckedOriginalPackageAssociations>, crate::CompileError> {
        let fail = |message| {
            crate::CompileError::CompilerEvidence(Box::new(CertificationError::Mismatch(message)))
        };
        match &self.mode {
            BoundPackageSelectionState::Item {
                original_packages, ..
            } => {
                if original_packages
                    .stage
                    .as_ref()
                    .is_some_and(|stage| !std::ptr::eq(stage.operation().as_ref(), operation))
                {
                    return Err(fail(
                        "Original package association belongs to another operation",
                    ));
                }
                Ok(Arc::clone(original_packages))
            }
            BoundPackageSelectionState::Ordinary {
                target,
                home_contracts,
                ..
            } => {
                if home_contracts
                    .manifest
                    .original_use_roots
                    .keys()
                    .any(|key| &key.target == target)
                {
                    return Err(fail(
                        "ordinary Original package association lacks an issued World stage",
                    ));
                }
                Ok(Arc::new(CheckedOriginalPackageAssociations {
                    stage: None,
                    slots: BTreeMap::new(),
                    use_roots: BTreeSet::new(),
                }))
            }
        }
    }
}

/// Controls enter the real item issuer using mutations of its compiler packet.
#[cfg(test)]
pub(crate) fn assert_actual_world_original_package_refusals(
    program: &crate::checked_cell::CellProgram,
) {
    use crate::checked_cell::{world_output_bytes, CellProgramBasis};
    let CellProgramBasis::World(checked) = program.basis() else {
        panic!("actual World fixture required")
    };
    assert_eq!(
        checked.stages().len(),
        1,
        "this fixture exercises its one actual stage issuer"
    );
    let operation = &checked.validation().inventory;
    let mut validation = PackageInterfaceValidation::with_inventory(Arc::clone(operation));
    let produced = crate::checked_cell::CheckedValueInputs::capture_world_produced_types(
        checked,
        &mut validation,
    )
    .unwrap();
    let stage = &checked.stages()[0];
    let first = *stage.ordinals().first().unwrap();
    let end = stage.ordinals().last().unwrap() + 1;
    let segment_outputs = produced.for_segment(first..end).unwrap();
    let segment = crate::declaration_context::decode_world_source_receipt(checked, stage.index())
        .unwrap()
        .issue_segment_originals(
            None,
            &segment_outputs,
            &mut validation,
            crate::declaration_context::WorldProgramContinuation::begin(checked),
        )
        .unwrap();
    let mut cursor = crate::declaration_context::bind_certified_segment_artifacts_with_validation(
        &segment.originals,
        &mut validation,
    )
    .unwrap();
    let mut controls = 0;
    let mut package_uses = 0;
    for (index, item) in program.items().iter().enumerate() {
        let Some(products) = item.native_products() else {
            continue;
        };
        let relative = PathBuf::from(format!("item-{index}"));
        let item_bytes =
            world_output_bytes(checked, &relative.join("certified-products.cbor")).unwrap();
        let actual = decode_segment_item_with_operation(item_bytes, operation).unwrap();
        if actual.selection.original_use_roots.is_empty() {
            continue;
        }
        package_uses += actual.selection.original_use_roots.len();
        let prepared_path = checked
            .item(index)
            .unwrap()
            .executable()
            .unwrap()
            .manifest()
            .keys()
            .find(|path| {
                path.to_str()
                    .is_some_and(|path| path.ends_with(".prepared.cbor"))
            })
            .unwrap();
        let artifact = crate::prepared_artifact::PreparedArtifact::parse(
            world_output_bytes(checked, &relative.join(prepared_path))
                .unwrap()
                .to_vec(),
            tidepool_repr::execution_schema::DecodeLimits::default(),
        )
        .unwrap();
        let item_outputs = produced.for_item(index).unwrap();
        let selection = products
            .completed_native_selection()
            .unwrap()
            .package_selection();
        let actual_packages = selection.checked_original_packages(operation).unwrap();
        assert!(!actual_packages.use_roots().is_empty());
        let packet: Value = ciborium::de::from_reader(item_bytes.as_ref()).unwrap();
        let mutate = |roots: Value| {
            let mut observed = packet.clone();
            let Value::Array(header) = &mut observed else {
                unreachable!()
            };
            let Value::Array(body) = &mut header[2] else {
                unreachable!()
            };
            let Value::Array(selection) = &mut body[3] else {
                unreachable!()
            };
            selection[6] = roots;
            let mut bytes = Vec::new();
            ciborium::ser::into_writer(&observed, &mut bytes).unwrap();
            decode_segment_item_with_operation(&bytes, operation)
        };
        let mut duplicates = array(&encode_roots(&actual.selection.original_use_roots))
            .unwrap()
            .to_vec();
        duplicates.push(duplicates[0].clone());
        assert!(matches!(
            mutate(value_array(duplicates)),
            Err(CertificationError::Receipt(
                "Original package uses are not unique and ordered"
            ))
        ));
        controls += 1;
        let mut issue = |observed| {
            segment.originals.select_item(
                &mut cursor,
                observed,
                &artifact.bind_package_program(),
                Some(&item_outputs),
                &mut validation,
            )
        };
        let mut missing = actual.selection.original_use_roots.clone();
        let first_key = missing.keys().next().unwrap().clone();
        missing.remove(&first_key);
        let observed = mutate(encode_roots(&missing)).unwrap();
        assert!(
            matches!(
                issue(observed),
                Err(CertificationError::Receipt(
                    "item Original package uses differ from its stage manifest"
                ))
            ),
            "omitted use refuses before advancing the real item cursor"
        );
        controls += 1;
        let mut crossed = actual.selection.original_use_roots.clone();
        let links = crossed.remove(&first_key).unwrap();
        let mut foreign_key = first_key;
        foreign_key.reference.original.selection.0[0] ^= 1;
        crossed.insert(foreign_key, links);
        let observed = mutate(encode_roots(&crossed)).unwrap();
        assert!(
            matches!(
                issue(observed),
                Err(CertificationError::Receipt(
                    "item Original package uses differ from its stage manifest"
                ))
            ),
            "crossed exact Original use refuses through the real issuer"
        );
        controls += 1;
        if let Some((key, _)) = actual
            .selection
            .original_use_roots
            .iter()
            .find(|(_, links)| !links.is_empty())
        {
            let mut incomplete = actual.selection.original_use_roots.clone();
            incomplete.get_mut(key).unwrap().remove(0);
            let observed = mutate(encode_roots(&incomplete)).unwrap();
            assert!(matches!(
                issue(observed),
                Err(CertificationError::Receipt(
                    "item Original package uses differ from its stage manifest"
                ))
            ));
            controls += 1;
        }
        let foreign = InventoryOperation::new(Default::default());
        assert!(
            matches!(selection.checked_original_packages(&foreign),Err(crate::CompileError::CompilerEvidence(error))
            if matches!(error.as_ref(),CertificationError::Mismatch("Original package association belongs to another operation")))
        );
        controls += 1;
        let (_, _, _, retry) =
            issue(actual).expect("refusals leave the actual item issuer and cursor unchanged");
        let retried = retry.checked_original_packages(operation).unwrap();
        assert_eq!(retried.use_roots(), actual_packages.use_roots());
        assert_eq!(
            retried.coordinates().collect::<Vec<_>>(),
            actual_packages.coordinates().collect::<Vec<_>>()
        );
    }
    assert!(
        package_uses > 0,
        "actual fixture must acquire an Original use"
    );
    assert!(
        controls >= 5,
        "actual fixture must exercise a package-bearing use"
    );
    eprintln!(
        "actual World Original package issuer: {package_uses} uses, {controls} refusal controls"
    );
}
