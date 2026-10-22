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
pub(crate) struct CheckedOriginalPackageAssociations {
    slots: BTreeMap<(NativeGroupBinding, u32), CheckedOriginalPackageSlot>,
    use_roots: BTreeSet<NativeGroupBinding>,
}
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

impl BoundPackageSelection {
    pub(crate) fn checked_original_packages(
        &self,
        operation: &InventoryOperation,
    ) -> Result<CheckedOriginalPackageAssociations, crate::CompileError> {
        let (target, roots, occurrences, original_demand) = match &self.mode {
            BoundPackageSelectionState::Ordinary { target, links, .. } => (
                target.as_str(),
                links.original_use_roots.as_ref(),
                links.occurrences.as_ref(),
                None,
            ),
            BoundPackageSelectionState::Item {
                selection,
                links,
                original_demand,
                ..
            } => (
                selection.target.as_str(),
                &selection.original_use_roots,
                links.occurrences.as_ref(),
                original_demand.as_ref(),
            ),
            BoundPackageSelectionState::HomeOrFixture { .. } => {
                return Ok(CheckedOriginalPackageAssociations {
                    slots: BTreeMap::new(),
                    use_roots: BTreeSet::new(),
                })
            }
        };
        let failure = |message| {
            crate::CompileError::CompilerEvidence(Box::new(CertificationError::Mismatch(message)))
        };
        let mut slots = BTreeMap::new();
        let mut use_roots = BTreeSet::new();
        for (key, links) in roots.iter().filter(|(key, _)| key.target == target) {
            crate::host_work::checkpoint()?;
            let stage = original_demand.ok_or_else(|| {
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
        Ok(CheckedOriginalPackageAssociations { slots, use_roots })
    }
}

/// Refusal controls borrow a real sealed World output and its immutable input
/// owner. Mutations are observations, never replacement authority.
#[cfg(test)]
pub(crate) fn assert_actual_world_original_package_refusals(
    program: &crate::checked_cell::CellProgram,
) {
    use crate::checked_cell::{world_output_bytes, CellProgramBasis};
    let CellProgramBasis::World(checked) = program.basis() else {
        panic!("actual World fixture required")
    };
    let operation = &checked.validation().inventory;
    let encode = |value: &Value| {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(value, &mut bytes).unwrap();
        bytes
    };
    let mut package_uses = 0;
    let mut controls = 0;
    for (index, item) in program.items().iter().enumerate() {
        let Some(products) = item.native_products() else {
            continue;
        };
        let stage = checked
            .stages()
            .iter()
            .find(|stage| stage.ordinals().contains(&index))
            .unwrap();
        let outputs = stage.outputs();
        let root = checked
            .directory()
            .path()
            .join(outputs.source_receipt().relative().parent().unwrap());
        let receipt = decode_segment_originals_with_operation(
            world_output_bytes(checked, outputs.certified_products().relative()).unwrap(),
            &root,
            operation,
        )
        .unwrap();
        let relative = PathBuf::from(format!("item-{index}"));
        let item_bytes =
            world_output_bytes(checked, &relative.join("certified-products.cbor")).unwrap();
        let actual = decode_segment_item_with_operation(item_bytes, operation).unwrap();
        if actual.selection.original_use_roots.is_empty() {
            continue;
        }
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
        let bind = |observed: &SegmentItemReceipt| {
            check_item_package_selection(
                &receipt.package_links,
                &observed.selection,
                artifact.bind_package_program().sha256(),
            )
        };
        bind(&actual).expect("actual worker package selection matches its actual stage");
        let selection = products
            .completed_native_selection()
            .unwrap()
            .package_selection()
            .clone();
        let checked_packages = selection
            .checked_original_packages(operation)
            .expect("actual target Original package associations join its retained demand");
        assert!(!checked_packages.use_roots().is_empty());
        package_uses += actual.selection.original_use_roots.len();
        let mut value: Value = ciborium::de::from_reader(item_bytes.as_ref()).unwrap();
        let Value::Array(packet) = &mut value else {
            unreachable!()
        };
        let Value::Array(body) = &mut packet[2] else {
            unreachable!()
        };
        let Value::Array(selection_wire) = &mut body[3] else {
            unreachable!()
        };
        let Value::Array(rows) = &mut selection_wire[6] else {
            unreachable!()
        };
        rows.push(rows[0].clone());
        assert!(
            decode_segment_item_with_operation(&encode(&value), operation).is_err(),
            "duplicate use refuses at actual receipt ingress"
        );
        controls += 1;
        let mut missing = actual.clone();
        let first_key = missing
            .selection
            .original_use_roots
            .keys()
            .next()
            .unwrap()
            .clone();
        missing.selection.original_use_roots.remove(&first_key);
        assert!(
            bind(&missing).is_err(),
            "missing use cannot substitute for the stage's actual target projection"
        );
        controls += 1;
        let mut wrong_target = actual.clone();
        wrong_target.selection.target.push_str("_foreign");
        assert!(bind(&wrong_target).is_err());
        controls += 1;
        let foreign_operation = InventoryOperation::new(Default::default());
        assert!(selection
            .checked_original_packages(&foreign_operation)
            .is_err());
        controls += 1;
        if let Some((key, links)) = actual
            .selection
            .original_use_roots
            .iter()
            .find(|(_, links)| !links.is_empty())
        {
            let mut incomplete = selection.clone();
            let BoundPackageSelectionState::Item {
                selection: item_selection,
                ..
            } = &mut incomplete.mode
            else {
                unreachable!()
            };
            item_selection
                .original_use_roots
                .get_mut(key)
                .unwrap()
                .remove(0);
            assert!(
                incomplete.checked_original_packages(operation).is_err(),
                "missing package identity refuses against retained exact obligations"
            );
            controls += 1;
            let mut crossed = selection.clone();
            let mut original_roots = actual.selection.original_use_roots.clone();
            let mut foreign_key = key.clone();
            foreign_key.reference.original.selection.0[0] ^= 1;
            original_roots.remove(key);
            original_roots.insert(foreign_key, links.clone());
            let BoundPackageSelectionState::Item {
                selection: item_selection,
                ..
            } = &mut crossed.mode
            else {
                unreachable!()
            };
            item_selection.original_use_roots = original_roots;
            assert!(
                crossed.checked_original_packages(operation).is_err(),
                "crossed Original use cannot borrow same symbol obligations"
            );
            controls += 1;
            let mut stale = selection.clone();
            let BoundPackageSelectionState::Item {
                links: manifest, ..
            } = &mut stale.mode
            else {
                unreachable!()
            };
            let mut altered = manifest.as_ref().clone();
            let mut occurrences = altered.occurrences.to_vec();
            occurrences[links[0].1.occurrence() as usize].defining_interface_sha256[0] ^= 1;
            altered.occurrences = occurrences.into();
            *manifest = Arc::new(altered);
            assert!(
                stale.checked_original_packages(operation).is_err(),
                "stale occurrence seal refuses before graph issuance"
            );
            controls += 1;
        }
        assert!(bind(&actual).is_ok());
        assert!(
            selection.checked_original_packages(operation).is_ok(),
            "refusals preserve the actual immutable owners"
        );
    }
    assert!(
        package_uses > 0,
        "fixture must acquire an actual Original use"
    );
    assert!(
        controls >= 7,
        "fixture must exercise actual package-bearing Original refusals"
    );
    eprintln!("actual World Original package ingress: {package_uses} target uses, {controls} refusal controls");
}
