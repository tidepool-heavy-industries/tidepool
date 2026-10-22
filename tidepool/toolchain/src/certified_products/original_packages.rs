//! Receipt-local package choices joined to the retained exact Original demand.
use super::*;
use crate::artifact_inventory::NativeGroupBinding;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct OriginalPackageRootKey {
    pub(super) target: String,
    reference: ReceiptImportOwner,
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
        let target = string(&key[0])?.to_owned();
        if target.is_empty() || target.contains('\0') {
            return Err(CertificationError::Receipt("Original package target"));
        }
        let use_row = sized(&key[1], 5)?;
        let reference = ReceiptImportOwner::OriginalBinding {
            unit: string(&use_row[0])?.to_owned(),
            module: string(&use_row[1])?.to_owned(),
            interface: receipt_artifact_binding(&use_row[2])?,
            original: receipt_artifact_binding(&use_row[3])?,
            binder: identity(&use_row[4])?,
        };
        let mut identities = BTreeSet::new();
        let mut links = Vec::new();
        for entry in array(&row[1])? {
            let entry = sized(entry, 2)?;
            let identity = identity(&entry[0])?;
            if !identities.insert(identity.clone()) {
                return Err(CertificationError::Receipt(
                    "duplicate Original package identity",
                ));
            }
            let link = decode_package_links_list(&value_array([entry[1].clone()]), operation)?;
            operation.reserve::<(SymbolIdentity, PackageLink)>(1)?;
            charge_symbol(operation, &identity)?;
            links.push((identity, link.into_iter().next().unwrap()));
        }
        operation.reserve::<OriginalPackageRootKey>(1)?;
        operation.charge(target.len())?;
        if roots
            .insert(OriginalPackageRootKey { target, reference }, links)
            .is_some()
        {
            return Err(CertificationError::Receipt(
                "duplicate Original package use",
            ));
        }
    }
    Ok(roots)
}

pub(super) fn encode_roots(roots: &OriginalPackageRoots) -> Value {
    value_array(roots.iter().map(|(key, links)| {
        let ReceiptImportOwner::OriginalBinding {
            unit,
            module,
            interface,
            original,
            binder,
        } = &key.reference
        else {
            unreachable!()
        };
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
    pub(crate) fn coordinates(&self) -> impl Iterator<Item = (NativeGroupBinding, u32)> + '_ {
        self.slots.keys().copied()
    }
}

impl BoundPackageSelection {
    pub(crate) fn checked_original_packages(
        &self,
        source: &CertifiedSourceSelection,
        operation: &InventoryOperation,
    ) -> Result<CheckedOriginalPackageAssociations, crate::CompileError> {
        let (target, roots, occurrences) = match &self.mode {
            BoundPackageSelectionState::Ordinary { target, links, .. } => (
                target.as_str(),
                links.original_use_roots.as_ref(),
                links.occurrences.as_ref(),
            ),
            BoundPackageSelectionState::Item {
                selection, links, ..
            } => (
                selection.target.as_str(),
                &selection.original_use_roots,
                links.occurrences.as_ref(),
            ),
            BoundPackageSelectionState::HomeOrFixture { .. } => {
                return Ok(CheckedOriginalPackageAssociations {
                    slots: BTreeMap::new(),
                })
            }
        };
        let failure = |message| {
            crate::CompileError::CompilerEvidence(Box::new(CertificationError::Mismatch(message)))
        };
        let mut slots = BTreeMap::new();
        for (key, links) in roots.iter().filter(|(key, _)| key.target == target) {
            crate::host_work::checkpoint()?;
            let binding = source
                .original_bindings
                .as_ref()
                .ok_or_else(|| failure("Original package association lacks its demand owner"))?;
            let OriginalBindingExecution::WorldStage(stage) = &binding.execution else {
                return Err(failure(
                    "Original package association lacks its World stage",
                ));
            };
            if !std::ptr::eq(stage.operation().as_ref(), operation) {
                return Err(failure(
                    "Original package association belongs to another operation",
                ));
            }
            let demand = stage.selection().ok_or_else(|| {
                failure("Original package association exceeds empty stage demand")
            })?;
            let obligations = demand
                .selected_groups()
                .package_obligations_for_use(&key.reference)?;
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
        Ok(CheckedOriginalPackageAssociations { slots })
    }
}
