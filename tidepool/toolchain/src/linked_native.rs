//! Executable references paired with exact acquisition and occurrence grants.
//! The pure component describes code; this owner records which issued occurrence
//! permits each ordered use. Neither identity nor byte custody substitutes a grant.

use crate::{
    artifact_inventory::{ArtifactBinding, ArtifactId, ArtifactView, NativeGroupBinding},
    certified_products::{CertifiedPackageCodeProduct, PackageLinks, PendingCertifiedGroup},
    CompileError,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tidepool_repr::{
    execution_schema::{CertifiedGroupCode, InventoryOperation, PreparedProgram, ValueId},
    native_code::{
        NativeCodeBody, NativeCodeImport, NativeCodeLocation, NativeCodeRef,
        NativeRetainedRequirement,
    },
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum NativeOccurrenceId {
    Home(NativeGroupBinding),
    Package { stage_id: [u8; 32], occurrence: u32 },
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum NativeGrantSelector {
    Code(NativeOccurrenceId),
    Retained { binding: ArtifactBinding },
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativePackageOccurrenceFacts {
    pub occurrence: u32,
    pub product: ArtifactId,
    pub children: Vec<u32>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativePackageReceiptFacts {
    pub stage_id: [u8; 32],
    pub occurrences: Vec<NativePackageOccurrenceFacts>,
}
/// Existing immutable compiler manifest plus only its actually selected proof
/// projection. Restored proof has no operation or fresh capture authority.
#[derive(Debug)]
pub(crate) struct NativePackageReceipt {
    pub(crate) manifest: Option<Arc<PackageLinks>>,
    pub(crate) facts: Arc<NativePackageReceiptFacts>,
}
impl NativePackageReceipt {
    pub(crate) fn issue(
        manifest: Arc<PackageLinks>,
        products: &BTreeMap<u32, Arc<CertifiedPackageCodeProduct>>,
        operation: &InventoryOperation,
    ) -> Result<Arc<Self>, CompileError> {
        let mut occurrences = Vec::new();
        for (index, product) in products {
            crate::host_work::checkpoint()?;
            operation
                .reserve::<NativePackageOccurrenceFacts>(1)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            let row = manifest
                .occurrences()
                .get(*index as usize)
                .filter(|row| row.id() == *index)
                .ok_or_else(|| refusal("selected package occurrence is absent"))?;
            if row.envelope_sha256() != product.envelope_sha256()
                || row.unit() != product.code().owner().unit
                || row.module() != product.code().owner().module
                || row.defining_interface_sha256() != product.defining_interface().sha256()
                || row.defining_version_sha256() != &product.code().owner().defining_version_sha256
            {
                return Err(refusal(
                    "selected occurrence changed its authenticated package product",
                ));
            }
            if row
                .imports()
                .iter()
                .any(|link| !products.contains_key(&link.occurrence()))
            {
                return Err(refusal("selected receipt omits an exact package child"));
            }
            occurrences.push(NativePackageOccurrenceFacts {
                occurrence: *index,
                product: product.artifact_id(),
                children: row.imports().iter().map(|link| link.occurrence()).collect(),
            });
        }
        Ok(Arc::new(Self {
            facts: Arc::new(NativePackageReceiptFacts {
                stage_id: *manifest.stage_id(),
                occurrences,
            }),
            manifest: Some(manifest),
        }))
    }
}
#[derive(Clone, Debug)]
pub(crate) enum NativeAcquisition {
    Home {
        product: Arc<PendingCertifiedGroup>,
    },
    Package {
        product: Arc<CertifiedPackageCodeProduct>,
        receipt: Arc<NativePackageReceipt>,
        occurrence: u32,
    },
}
#[derive(Clone, Debug)]
pub(crate) struct NativeAcquiredOccurrence {
    pub(crate) occurrence: NativeOccurrenceId,
    pub(crate) code: NativeCodeRef,
    pub(crate) acquisition: NativeAcquisition,
    pub(crate) imports: Arc<[NativeGrantSelector]>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeOccurrenceFacts {
    pub occurrence: NativeOccurrenceId,
    pub code: NativeCodeLocation,
    pub imports: Arc<[NativeGrantSelector]>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAcquiredClosureFacts {
    pub version: u32,
    pub occurrences: Vec<NativeOccurrenceFacts>,
    pub package_receipts: Vec<Arc<NativePackageReceiptFacts>>,
}
/// Current is receiving-target correlation, never part of persisted acquisition.
pub(crate) type NativeTargetSources = BTreeMap<NativeGroupBinding, Arc<PendingCertifiedGroup>>;

#[derive(Debug)]
pub struct AcquiredNativeClosure {
    occurrences: BTreeMap<NativeOccurrenceId, Arc<NativeAcquiredOccurrence>>,
    artifacts: ArtifactView,
    recorded: Arc<NativeAcquiredClosureFacts>,
    record_bytes: Arc<[u8]>,
    identity: [u8; 32],
}
impl AcquiredNativeClosure {
    pub(crate) fn issue(
        occurrences: Vec<NativeAcquiredOccurrence>,
        artifacts: ArtifactView,
        operation: &InventoryOperation,
    ) -> Result<Arc<Self>, CompileError> {
        operation
            .reserve::<NativeAcquiredOccurrence>(occurrences.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let mut nodes = BTreeMap::new();
        let mut receipts = BTreeMap::<[u8; 32], Arc<NativePackageReceiptFacts>>::new();
        for node in occurrences {
            crate::host_work::checkpoint()?;
            match (&node.occurrence, &node.acquisition, node.code.body()) {
                (
                    NativeOccurrenceId::Home(binding),
                    NativeAcquisition::Home { product },
                    NativeCodeBody::Home(code),
                ) => {
                    let expected = CertifiedGroupCode::admit(
                        product.owner().clone(),
                        product.group().clone(),
                    )?;
                    if binding.original_ordinal != product.group().original_ordinal()
                        || &expected != code
                    {
                        return Err(refusal("HOME code differs from its exact acquired product"));
                    }
                }
                (
                    NativeOccurrenceId::Package {
                        stage_id,
                        occurrence,
                    },
                    NativeAcquisition::Package {
                        product,
                        receipt,
                        occurrence: selected,
                    },
                    NativeCodeBody::Package(code),
                ) => {
                    if selected != occurrence
                        || stage_id != &receipt.facts.stage_id
                        || product.code() != code
                    {
                        return Err(refusal("package code differs from its acquired occurrence"));
                    }
                    let row = receipt
                        .facts
                        .occurrences
                        .iter()
                        .find(|row| row.occurrence == *occurrence)
                        .ok_or_else(|| refusal("selected package receipt row is absent"))?;
                    if row.product != product.artifact_id() {
                        return Err(refusal("package receipt changed its exact product"));
                    }
                    if let Some(prior) = receipts.insert(*stage_id, Arc::clone(&receipt.facts)) {
                        if prior != receipt.facts {
                            return Err(refusal("competing selected package receipt projections"));
                        }
                    }
                }
                _ => return Err(refusal("code body and acquired occurrence kinds differ")),
            }
            if nodes.insert(node.occurrence, Arc::new(node)).is_some() {
                return Err(refusal("duplicate exact acquired occurrence"));
            }
        }
        for node in nodes.values() {
            let imports = node.code.imports().collect::<Vec<_>>();
            if imports.len() != node.imports.len() {
                return Err(refusal("acquired occurrence slot count differs"));
            }
            for (index, (edge, grant)) in imports.iter().zip(node.imports.iter()).enumerate() {
                match (edge, grant) {
                    (
                        NativeCodeImport::Code { code, .. },
                        NativeGrantSelector::Code(occurrence),
                    ) => {
                        let child = nodes
                            .get(occurrence)
                            .ok_or_else(|| refusal("acquired child occurrence is absent"))?;
                        if child.code != *code {
                            return Err(refusal("acquired child changed its linked code"));
                        }
                        if let NativeAcquisition::Package {
                            receipt,
                            occurrence,
                            ..
                        } = &node.acquisition
                        {
                            let row = receipt
                                .facts
                                .occurrences
                                .iter()
                                .find(|row| row.occurrence == *occurrence)
                                .ok_or_else(|| refusal("selected package receipt row is absent"))?;
                            let expected = row
                                .children
                                .get(index)
                                .copied()
                                .ok_or_else(|| refusal("package receipt child is absent"))?;
                            if child.occurrence
                                != (NativeOccurrenceId::Package {
                                    stage_id: receipt.facts.stage_id,
                                    occurrence: expected,
                                })
                            {
                                return Err(refusal(
                                    "package selected edge changed its exact occurrence",
                                ));
                            }
                        }
                    }
                    (
                        NativeCodeImport::Retained(requirement),
                        NativeGrantSelector::Retained { binding },
                    ) => {
                        if requirement.type_artifact() != Some(&binding.artifact.0) {
                            return Err(refusal(
                                "retained requirement changed its acquired type owner",
                            ));
                        }
                    }
                    _ => return Err(refusal("acquired occurrence changed an import kind")),
                }
            }
        }
        let recorded = Arc::new(NativeAcquiredClosureFacts {
            version: 1,
            occurrences: nodes
                .values()
                .map(|node| NativeOccurrenceFacts {
                    occurrence: node.occurrence,
                    code: node.code.location(),
                    imports: Arc::clone(&node.imports),
                })
                .collect(),
            package_receipts: receipts.into_values().collect(),
        });
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(recorded.as_ref(), &mut bytes)
            .map_err(|error| refusal(&format!("acquired closure encoding: {error}")))?;
        operation
            .charge(bytes.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let mut hash = Sha256::new();
        hash.update(b"Tidepool acquired native closure v1\0");
        hash.update(&bytes);
        Ok(Arc::new(Self {
            occurrences: nodes,
            artifacts,
            recorded,
            record_bytes: bytes.into(),
            identity: hash.finalize().into(),
        }))
    }
    pub fn identity(&self) -> &[u8; 32] {
        &self.identity
    }
    pub fn recorded_facts(&self) -> &Arc<NativeAcquiredClosureFacts> {
        &self.recorded
    }
    pub fn record_bytes(&self) -> &Arc<[u8]> {
        &self.record_bytes
    }
    pub fn artifacts(&self) -> &ArtifactView {
        &self.artifacts
    }
    pub fn acquired(self: &Arc<Self>) -> impl ExactSizeIterator<Item = AcquiredNativeCode> + '_ {
        self.occurrences
            .keys()
            .map(|occurrence| AcquiredNativeCode {
                owner: Arc::clone(self),
                occurrence: *occurrence,
                sources: None,
            })
    }
    pub fn home(self: &Arc<Self>, binding: NativeGroupBinding) -> Option<AcquiredNativeCode> {
        self.get(NativeOccurrenceId::Home(binding))
    }
    pub(crate) fn get(
        self: &Arc<Self>,
        occurrence: NativeOccurrenceId,
    ) -> Option<AcquiredNativeCode> {
        self.occurrences
            .contains_key(&occurrence)
            .then(|| AcquiredNativeCode {
                owner: Arc::clone(self),
                occurrence,
                sources: None,
            })
    }
}
#[derive(Clone, Debug)]
pub struct AcquiredNativeCode {
    owner: Arc<AcquiredNativeClosure>,
    occurrence: NativeOccurrenceId,
    sources: Option<Arc<NativeTargetSources>>,
}
#[derive(Clone, Copy, Debug)]
pub enum NativeCodeGrant<'a> {
    Home {
        binding: NativeGroupBinding,
        product: &'a Arc<PendingCertifiedGroup>,
        current: Option<&'a Arc<PendingCertifiedGroup>>,
    },
    Package {
        product: &'a Arc<CertifiedPackageCodeProduct>,
        stage_id: &'a [u8; 32],
        occurrence: u32,
    },
}
#[derive(Clone, Debug)]
pub enum AcquiredNativeImport {
    Code {
        acquired: AcquiredNativeCode,
        value: ValueId,
    },
    Retained {
        requirement: NativeRetainedRequirement,
        binding: ArtifactBinding,
    },
}
impl AcquiredNativeCode {
    fn node(&self) -> &NativeAcquiredOccurrence {
        &self.owner.occurrences[&self.occurrence]
    }
    pub fn code(&self) -> &NativeCodeRef {
        &self.node().code
    }
    pub fn occurrence(&self) -> NativeOccurrenceId {
        self.occurrence
    }
    pub fn closure(&self) -> &Arc<AcquiredNativeClosure> {
        &self.owner
    }
    pub fn grant(&self) -> NativeCodeGrant<'_> {
        match (&self.occurrence, &self.node().acquisition) {
            (NativeOccurrenceId::Home(binding), NativeAcquisition::Home { product }) => {
                NativeCodeGrant::Home {
                    binding: *binding,
                    product,
                    current: self
                        .sources
                        .as_ref()
                        .and_then(|sources| sources.get(binding)),
                }
            }
            (
                _,
                NativeAcquisition::Package {
                    product,
                    receipt,
                    occurrence,
                },
            ) => NativeCodeGrant::Package {
                product,
                stage_id: &receipt.facts.stage_id,
                occurrence: *occurrence,
            },
            _ => unreachable!("issued occurrence kind"),
        }
    }
    pub fn imports(&self) -> impl ExactSizeIterator<Item = AcquiredNativeImport> + '_ {
        self.code()
            .imports()
            .zip(self.node().imports.iter())
            .map(|(edge, selector)| match (edge, selector) {
                (NativeCodeImport::Code { value, .. }, NativeGrantSelector::Code(occurrence)) => {
                    AcquiredNativeImport::Code {
                        acquired: Self {
                            owner: Arc::clone(&self.owner),
                            occurrence: *occurrence,
                            sources: self.sources.clone(),
                        },
                        value,
                    }
                }
                (
                    NativeCodeImport::Retained(requirement),
                    NativeGrantSelector::Retained { binding },
                ) => AcquiredNativeImport::Retained {
                    requirement,
                    binding: *binding,
                },
                _ => unreachable!("issued paired acquisition slots"),
            })
    }
    pub(crate) fn reborrow(&self, sources: &Arc<NativeTargetSources>) -> Self {
        Self {
            owner: Arc::clone(&self.owner),
            occurrence: self.occurrence,
            sources: Some(Arc::clone(sources)),
        }
    }
    pub(crate) fn selected_occurrences(&self) -> Vec<NativeAcquiredOccurrence> {
        let mut reached = BTreeSet::new();
        let mut pending = vec![self.occurrence];
        let mut result = Vec::new();
        while let Some(occurrence) = pending.pop() {
            if !reached.insert(occurrence) {
                continue;
            }
            let node = &self.owner.occurrences[&occurrence];
            pending.extend(node.imports.iter().filter_map(|selector| match selector {
                NativeGrantSelector::Code(occurrence) => Some(*occurrence),
                _ => None,
            }));
            result.push(node.as_ref().clone());
        }
        result
    }
}
#[derive(Clone, Debug)]
pub enum LinkedNativeTargetImport {
    Code {
        acquired: AcquiredNativeCode,
        value: ValueId,
    },
    RetainedCapture(NativeRetainedRequirement),
}
#[derive(Debug)]
pub struct LinkedNativeTarget {
    pub(crate) prepared: Arc<PreparedProgram>,
    pub(crate) imports: Arc<[LinkedNativeTargetImport]>,
    pub(crate) closure: Arc<AcquiredNativeClosure>,
    pub(crate) sources: Arc<NativeTargetSources>,
}
impl LinkedNativeTarget {
    pub fn prepared(&self) -> &Arc<PreparedProgram> {
        &self.prepared
    }
    pub fn entry(&self) -> ValueId {
        self.prepared.entry()
    }
    pub fn imports(&self) -> &[LinkedNativeTargetImport] {
        &self.imports
    }
    pub fn acquired(&self) -> impl ExactSizeIterator<Item = AcquiredNativeCode> + '_ {
        self.closure
            .acquired()
            .map(|code| code.reborrow(&self.sources))
    }
    pub fn closure(&self) -> &Arc<AcquiredNativeClosure> {
        &self.closure
    }
    pub fn artifacts(&self) -> &ArtifactView {
        self.closure.artifacts()
    }
}
fn refusal(detail: &str) -> CompileError {
    CompileError::ExtractFailed(format!("native acquisition: {detail}"))
}
