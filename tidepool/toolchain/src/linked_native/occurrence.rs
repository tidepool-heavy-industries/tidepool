//! Closed executable occurrences derived from their actual acquisition owners.

use super::*;
use crate::certified_products::{AcquiredHomeCodeGrant, SelectedNativeImport};
use tidepool_repr::native_code::{NativeCodeComponent, NativeCodePlan, NativeCodePlanImport};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum CheckedNativeSlot {
    Code {
        occurrence: NativeOccurrenceId,
        value: ValueId,
    },
    Retained {
        binding: ArtifactBinding,
        requirement: NativeRetainedRequirement,
    },
}
impl CheckedNativeSlot {
    fn selector(&self) -> NativeGrantSelector {
        match self {
            Self::Code { occurrence, .. } => NativeGrantSelector::Code(*occurrence),
            Self::Retained { binding, .. } => NativeGrantSelector::Retained { binding: *binding },
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct HomeOccurrence {
    grant: AcquiredHomeCodeGrant,
    code: NativeCodeRef,
    slots: Arc<[CheckedNativeSlot]>,
}
#[derive(Clone, Debug)]
pub(crate) struct PackageOccurrence {
    grant: AcquiredPackageGrant,
    code: NativeCodeRef,
    slots: Arc<[CheckedNativeSlot]>,
}
#[derive(Clone, Debug)]
pub(crate) enum NativeAcquiredOccurrence {
    Home(HomeOccurrence),
    Package(PackageOccurrence),
}
impl NativeAcquiredOccurrence {
    pub(crate) fn occurrence(&self) -> NativeOccurrenceId {
        match self {
            Self::Home(node) => NativeOccurrenceId::Home(node.grant.binding()),
            Self::Package(node) => node.grant.occurrence(),
        }
    }
    pub(crate) fn code(&self) -> &NativeCodeRef {
        match self {
            Self::Home(node) => &node.code,
            Self::Package(node) => &node.code,
        }
    }
    pub(super) fn slots(&self) -> &[CheckedNativeSlot] {
        match self {
            Self::Home(node) => &node.slots,
            Self::Package(node) => &node.slots,
        }
    }
    pub(crate) fn children(&self) -> impl Iterator<Item = NativeOccurrenceId> + '_ {
        self.slots().iter().filter_map(|slot| match slot {
            CheckedNativeSlot::Code { occurrence, .. } => Some(*occurrence),
            CheckedNativeSlot::Retained { .. } => None,
        })
    }
    pub(super) fn selectors(&self) -> Arc<[NativeGrantSelector]> {
        self.slots()
            .iter()
            .map(CheckedNativeSlot::selector)
            .collect()
    }
    pub(crate) fn same_linkage(&self, other: &Self) -> bool {
        self.occurrence() == other.occurrence()
            && self.code() == other.code()
            && self.slots() == other.slots()
    }
    pub(super) fn home_grant(&self) -> Option<&AcquiredHomeCodeGrant> {
        match self {
            Self::Home(node) => Some(&node.grant),
            Self::Package(_) => None,
        }
    }
    pub(super) fn package_grant(&self) -> Option<&AcquiredPackageGrant> {
        match self {
            Self::Package(node) => Some(&node.grant),
            Self::Home(_) => None,
        }
    }
    pub(super) fn grant<'a>(
        &'a self,
        current: Option<&'a Arc<PendingCertifiedGroup>>,
    ) -> NativeCodeGrant<'a> {
        match self {
            Self::Home(node) => NativeCodeGrant::Home {
                binding: node.grant.binding(),
                product: node.grant.product(),
                current,
            },
            Self::Package(node) => NativeCodeGrant::Package {
                product: node.grant.product(),
                stage_id: node.grant.stage_id(),
                occurrence: node.grant.index(),
            },
        }
    }
}

/// Only package slots require a linkage choice. Source and retained HOME slots
/// come directly from the selected HOME grant's ordered row.
pub(crate) enum NativePackageChoice {
    Receipt(AcquiredPackageOccurrence),
    Held(AcquiredNativeCode),
}
enum PlannedGrant {
    Home(AcquiredHomeCodeGrant),
    Package(AcquiredPackageOccurrence),
}
enum PlannedSlot {
    Local {
        occurrence: NativeOccurrenceId,
        value: ValueId,
    },
    Held {
        acquired: AcquiredNativeCode,
        value: ValueId,
    },
    Retained {
        binding: ArtifactBinding,
        requirement: NativeRetainedRequirement,
    },
}
impl PlannedSlot {
    fn checked(&self) -> CheckedNativeSlot {
        match self {
            Self::Local { occurrence, value } => CheckedNativeSlot::Code {
                occurrence: *occurrence,
                value: *value,
            },
            Self::Held { acquired, value } => CheckedNativeSlot::Code {
                occurrence: acquired.occurrence(),
                value: *value,
            },
            Self::Retained {
                binding,
                requirement,
            } => CheckedNativeSlot::Retained {
                binding: *binding,
                requirement: requirement.clone(),
            },
        }
    }
    fn code_import(
        &self,
        local: &BTreeMap<NativeOccurrenceId, usize>,
    ) -> Result<NativeCodePlanImport, CompileError> {
        Ok(match self {
            Self::Local { occurrence, value } => NativeCodePlanImport::Local {
                occurrence: *local
                    .get(occurrence)
                    .ok_or_else(|| refusal("selected occurrence child is absent"))?,
                value: *value,
            },
            Self::Held { acquired, value } => NativeCodePlanImport::External {
                code: acquired.code().clone(),
                value: *value,
            },
            Self::Retained { requirement, .. } => {
                NativeCodePlanImport::Retained(requirement.clone())
            }
        })
    }
}

/// Pending owner-derived linkage, not an acquired executable occurrence.
pub(crate) struct NativeOccurrencePlan {
    grant: PlannedGrant,
    slots: Vec<PlannedSlot>,
}
impl NativeOccurrencePlan {
    pub(crate) fn home(
        grant: AcquiredHomeCodeGrant,
        mut packages: BTreeMap<usize, NativePackageChoice>,
        operation: &InventoryOperation,
    ) -> Result<Self, CompileError> {
        let definitions = grant.product().group().definitions();
        if definitions.globals().len() != grant.imports().len() {
            return Err(refusal(
                "HOME grant slot count differs from its definitions",
            ));
        }
        operation
            .reserve::<PlannedSlot>(grant.imports().len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let mut slots = Vec::new();
        for (slot, import) in grant.imports().iter().enumerate() {
            crate::host_work::checkpoint()?;
            operation
                .charge(1)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            slots.push(match import {
                SelectedNativeImport::Source { binding, value } => PlannedSlot::Local {
                    occurrence: NativeOccurrenceId::Home(*binding),
                    value: *value,
                },
                SelectedNativeImport::Retained {
                    binding,
                    identity,
                    generation,
                } => PlannedSlot::Retained {
                    binding: *binding,
                    requirement: NativeRetainedRequirement::new(
                        identity.clone(),
                        *generation,
                        Some(binding.artifact.0),
                    ),
                },
                SelectedNativeImport::Package {
                    identity,
                    interface_digest,
                } => {
                    let global = &definitions.globals()[slot];
                    if identity != &global.identity {
                        return Err(refusal("HOME package slot differs from its global"));
                    }
                    match packages
                        .remove(&slot)
                        .ok_or_else(|| refusal("HOME package slot lacks its acquired choice"))?
                    {
                        NativePackageChoice::Receipt(package) => PlannedSlot::Local {
                            value: resolve_package_export(
                                package.product(),
                                global,
                                definitions,
                                interface_digest,
                            )?,
                            occurrence: package.occurrence(),
                        },
                        NativePackageChoice::Held(acquired) => {
                            let NativeCodeGrant::Package { product, .. } = acquired.grant() else {
                                return Err(refusal(
                                    "HOME package slot has a nonpackage acquisition",
                                ));
                            };
                            let value = resolve_package_export(
                                product,
                                global,
                                definitions,
                                interface_digest,
                            )?;
                            PlannedSlot::Held { acquired, value }
                        }
                    }
                }
            });
        }
        if !packages.is_empty() {
            return Err(refusal("HOME has package choices outside its issued slots"));
        }
        Ok(Self {
            grant: PlannedGrant::Home(grant),
            slots,
        })
    }

    pub(crate) fn package(
        grant: AcquiredPackageOccurrence,
        operation: &InventoryOperation,
    ) -> Result<Self, CompileError> {
        operation
            .reserve::<PlannedSlot>(grant.children().len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let slots = grant
            .children()
            .map(|(child, value)| {
                operation
                    .charge(1)
                    .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
                Ok(PlannedSlot::Local {
                    occurrence: child.occurrence(),
                    value,
                })
            })
            .collect::<Result<Vec<_>, CompileError>>()?;
        Ok(Self {
            grant: PlannedGrant::Package(grant),
            slots,
        })
    }

    fn occurrence(&self) -> NativeOccurrenceId {
        match &self.grant {
            PlannedGrant::Home(grant) => NativeOccurrenceId::Home(grant.binding()),
            PlannedGrant::Package(grant) => grant.occurrence(),
        }
    }
    fn body(&self) -> Result<NativeCodeBody, CompileError> {
        Ok(match &self.grant {
            PlannedGrant::Home(grant) => NativeCodeBody::Home(CertifiedGroupCode::admit(
                grant.product().owner().clone(),
                grant.product().group().clone(),
            )?),
            PlannedGrant::Package(grant) => NativeCodeBody::Package(grant.product().code().clone()),
        })
    }
    fn close(
        self,
        code: NativeCodeRef,
        operation: &InventoryOperation,
    ) -> Result<NativeAcquiredOccurrence, CompileError> {
        operation
            .reserve::<CheckedNativeSlot>(self.slots.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let slots = self.slots.iter().map(PlannedSlot::checked).collect();
        Ok(match self.grant {
            PlannedGrant::Home(grant) => {
                NativeAcquiredOccurrence::Home(HomeOccurrence { grant, code, slots })
            }
            PlannedGrant::Package(grant) => NativeAcquiredOccurrence::Package(PackageOccurrence {
                grant: grant.into_grant(),
                code,
                slots,
            }),
        })
    }

    pub(crate) fn link(
        plans: Vec<Self>,
        operation: &InventoryOperation,
    ) -> Result<Vec<NativeAcquiredOccurrence>, CompileError> {
        operation
            .reserve::<(NativeOccurrenceId, usize)>(plans.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        operation
            .reserve::<NativeCodePlan>(plans.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        operation
            .reserve::<usize>(plans.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        operation
            .reserve::<NativeAcquiredOccurrence>(plans.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let mut local = BTreeMap::new();
        for (index, plan) in plans.iter().enumerate() {
            if local.insert(plan.occurrence(), index).is_some() {
                return Err(refusal("duplicate planned acquired occurrence"));
            }
        }
        let code_plans = plans
            .iter()
            .map(|plan| {
                operation
                    .reserve::<NativeCodePlanImport>(plan.slots.len())
                    .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
                Ok(NativeCodePlan {
                    body: plan.body()?,
                    imports: plan
                        .slots
                        .iter()
                        .map(|slot| slot.code_import(&local))
                        .collect::<Result<Vec<_>, CompileError>>()?,
                })
            })
            .collect::<Result<Vec<_>, CompileError>>()?;
        let roots = (0..plans.len()).collect::<Vec<_>>();
        let linked =
            NativeCodeComponent::link(&code_plans, &roots, operation).map_err(linked_code_error)?;
        plans
            .into_iter()
            .zip(linked)
            .map(|(plan, code)| plan.close(code, operation))
            .collect()
    }

    /// Recorded selectors are DATA. Reconstruct owner-derived slots, then check
    /// their exact code projection before issuing the same closed live variants.
    pub(super) fn restore_home(
        grant: AcquiredHomeCodeGrant,
        selectors: &[NativeGrantSelector],
        receipts: &BTreeMap<[u8; 32], Arc<NativePackageReceipt>>,
        operation: &InventoryOperation,
    ) -> Result<Self, CompileError> {
        if selectors.len() != grant.imports().len() {
            return Err(refusal("recorded HOME slot count differs"));
        }
        operation
            .reserve::<(usize, NativePackageChoice)>(grant.imports().len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let mut packages = BTreeMap::new();
        for (slot, import) in grant.imports().iter().enumerate() {
            if matches!(import, SelectedNativeImport::Package { .. }) {
                let NativeGrantSelector::Code(NativeOccurrenceId::Package {
                    stage_id,
                    occurrence,
                }) = &selectors[slot]
                else {
                    return Err(refusal("recorded HOME package slot changed kind"));
                };
                let package = receipts
                    .get(stage_id)
                    .and_then(|receipt| receipt.occurrence(*occurrence))
                    .ok_or_else(|| refusal("recorded HOME package acquisition is absent"))?;
                packages.insert(slot, NativePackageChoice::Receipt(package));
            }
        }
        Self::home(grant, packages, operation)
    }

    pub(super) fn restore(
        self,
        code: NativeCodeRef,
        selectors: &[NativeGrantSelector],
        codes: &BTreeMap<NativeOccurrenceId, NativeCodeRef>,
        operation: &InventoryOperation,
    ) -> Result<NativeAcquiredOccurrence, CompileError> {
        if code.body() != &self.body()?
            || code.imports().len() != self.slots.len()
            || selectors.len() != self.slots.len()
        {
            return Err(refusal(
                "recorded code differs from its acquired occurrence",
            ));
        }
        for ((slot, edge), recorded) in self.slots.iter().zip(code.imports()).zip(selectors) {
            crate::host_work::checkpoint()?;
            operation
                .charge(1)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            if &slot.checked().selector() != recorded {
                return Err(refusal(
                    "recorded slot differs from its exact acquired selector",
                ));
            }
            let valid = match (slot, edge) {
                (
                    PlannedSlot::Local { occurrence, value },
                    NativeCodeImport::Code {
                        code,
                        value: actual,
                    },
                ) => {
                    codes
                        .get(occurrence)
                        .is_some_and(|expected| expected == &code)
                        && value == &actual
                }
                (PlannedSlot::Retained { requirement, .. }, NativeCodeImport::Retained(actual)) => {
                    requirement == &actual
                }
                _ => false,
            };
            if !valid {
                return Err(refusal(
                    "recorded code import differs from its acquired slot",
                ));
            }
        }
        self.close(code, operation)
    }
}
