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
mod restore;
pub(crate) use restore::restore_acquired_native_closures;
mod occurrence;
use occurrence::CheckedNativeSlot;
pub(crate) use occurrence::{NativeAcquiredOccurrence, NativeOccurrencePlan, NativePackageChoice};

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
#[serde(deny_unknown_fields)]
pub enum NativeOccurrenceId {
    Home(NativeGroupBinding),
    Package { stage_id: [u8; 32], occurrence: u32 },
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
/// Authenticated selected receipt rows retain their exact acquired products.
/// This owner grants no operation or fresh capture authority.
#[derive(Debug)]
pub(crate) struct NativePackageReceipt {
    facts: Arc<NativePackageReceiptFacts>,
    rows: BTreeMap<u32, Arc<AcquiredPackageRow>>,
}
#[derive(Debug)]
struct AcquiredPackageRow {
    index: usize,
    product: Arc<CertifiedPackageCodeProduct>,
    values: Arc<[ValueId]>,
}
#[derive(Clone, Debug)]
pub(crate) struct AcquiredPackageOccurrence {
    receipt: Arc<NativePackageReceipt>,
    occurrence: u32,
}
/// A selected row retains its actual acquisition and immutable receipt facts;
/// unrelated products in the receipt are not retained by executable code.
#[derive(Clone, Debug)]
struct AcquiredPackageGrant {
    receipt: Arc<NativePackageReceiptFacts>,
    row: Arc<AcquiredPackageRow>,
}
impl AcquiredPackageGrant {
    fn row(&self) -> &NativePackageOccurrenceFacts {
        &self.receipt.occurrences[self.row.index]
    }
    fn stage_id(&self) -> &[u8; 32] {
        &self.receipt.stage_id
    }
    fn index(&self) -> u32 {
        self.row().occurrence
    }
    fn product(&self) -> &Arc<CertifiedPackageCodeProduct> {
        &self.row.product
    }
    fn occurrence(&self) -> NativeOccurrenceId {
        NativeOccurrenceId::Package {
            stage_id: *self.stage_id(),
            occurrence: self.index(),
        }
    }
}
impl AcquiredPackageOccurrence {
    fn row(&self) -> &NativePackageOccurrenceFacts {
        self.receipt
            .row(self.occurrence)
            .expect("acquired package row")
    }
    pub(crate) fn occurrence(&self) -> NativeOccurrenceId {
        NativeOccurrenceId::Package {
            stage_id: self.receipt.facts.stage_id,
            occurrence: self.occurrence,
        }
    }
    fn product(&self) -> &Arc<CertifiedPackageCodeProduct> {
        &self.receipt.rows[&self.occurrence].product
    }
    fn into_grant(self) -> AcquiredPackageGrant {
        AcquiredPackageGrant {
            receipt: Arc::clone(&self.receipt.facts),
            row: Arc::clone(&self.receipt.rows[&self.occurrence]),
        }
    }
    fn children(&self) -> impl ExactSizeIterator<Item = (Self, ValueId)> + '_ {
        self.row()
            .children
            .iter()
            .zip(self.receipt.rows[&self.occurrence].values.iter())
            .map(|(occurrence, value)| {
                (
                    Self {
                        receipt: Arc::clone(&self.receipt),
                        occurrence: *occurrence,
                    },
                    *value,
                )
            })
    }
}
impl NativePackageReceipt {
    fn acquire_rows(
        facts: Arc<NativePackageReceiptFacts>,
        products: &BTreeMap<u32, Arc<CertifiedPackageCodeProduct>>,
        operation: &InventoryOperation,
    ) -> Result<Arc<Self>, CompileError> {
        operation
            .reserve::<(u32, Arc<AcquiredPackageRow>)>(facts.occurrences.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        operation
            .reserve::<AcquiredPackageRow>(facts.occurrences.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        if facts
            .occurrences
            .windows(2)
            .any(|pair| pair[0].occurrence >= pair[1].occurrence)
        {
            return Err(refusal("noncanonical package receipt occurrence order"));
        }
        let mut rows = facts
            .occurrences
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let product = products
                    .get(&row.occurrence)
                    .filter(|product| product.artifact_id() == row.product)
                    .ok_or_else(|| {
                        refusal("selected package product has no exact acquired reference")
                    })?;
                if row.children.len() != product.code().globals().len() {
                    return Err(refusal(
                        "package receipt changed its exact ordered slot count",
                    ));
                }
                Ok((
                    row.occurrence,
                    Arc::new(AcquiredPackageRow {
                        index,
                        product: Arc::clone(product),
                        values: Arc::from([]),
                    }),
                ))
            })
            .collect::<Result<BTreeMap<_, _>, CompileError>>()?;
        if facts
            .occurrences
            .iter()
            .any(|row| row.children.iter().any(|child| !rows.contains_key(child)))
        {
            return Err(refusal("package receipt omits a selected child occurrence"));
        }
        for row in &facts.occurrences {
            crate::host_work::checkpoint()?;
            operation
                .reserve::<ValueId>(row.children.len())
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            let mut values = Vec::new();
            for (slot, child) in row.children.iter().enumerate() {
                operation
                    .charge(1)
                    .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
                values.push(resolve_package_import(
                    &rows[&row.occurrence].product,
                    slot,
                    &rows[child].product,
                )?);
            }
            Arc::get_mut(rows.get_mut(&row.occurrence).expect("acquired package row"))
                .expect("package receipt has not lent rows")
                .values = values.into();
        }
        Ok(Arc::new(Self { facts, rows }))
    }
    fn row(&self, occurrence: u32) -> Option<&NativePackageOccurrenceFacts> {
        self.rows
            .get(&occurrence)
            .map(|row| &self.facts.occurrences[row.index])
    }
    pub(crate) fn occurrence(
        self: &Arc<Self>,
        occurrence: u32,
    ) -> Option<AcquiredPackageOccurrence> {
        self.rows
            .contains_key(&occurrence)
            .then(|| AcquiredPackageOccurrence {
                receipt: Arc::clone(self),
                occurrence,
            })
    }
    fn acquire_recorded(
        facts: Arc<NativePackageReceiptFacts>,
        products: &BTreeMap<ArtifactId, Arc<CertifiedPackageCodeProduct>>,
        operation: &InventoryOperation,
    ) -> Result<Arc<Self>, CompileError> {
        operation
            .reserve::<(u32, Arc<CertifiedPackageCodeProduct>)>(facts.occurrences.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let selected = facts
            .occurrences
            .iter()
            .map(|row| {
                let product = products
                    .get(&row.product)
                    .ok_or_else(|| refusal("recorded package acquisition is absent"))?;
                Ok((row.occurrence, Arc::clone(product)))
            })
            .collect::<Result<BTreeMap<_, _>, CompileError>>()?;
        Self::acquire_rows(facts, &selected, operation)
    }
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
        Self::acquire_rows(
            Arc::new(NativePackageReceiptFacts {
                stage_id: *manifest.stage_id(),
                occurrences,
            }),
            products,
            operation,
        )
    }
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
/// Receiving-target provenance is not persisted acquisition authority.
#[derive(Clone, Debug)]
pub(crate) enum TargetHomeSource {
    CurrentStage(Arc<PendingCertifiedGroup>),
    PriorProgramStage(Arc<PendingCertifiedGroup>),
    HistoricalInput,
}
impl TargetHomeSource {
    pub(crate) fn cell_stage_group(&self) -> Option<&Arc<PendingCertifiedGroup>> {
        match self {
            Self::CurrentStage(group) | Self::PriorProgramStage(group) => Some(group),
            Self::HistoricalInput => None,
        }
    }
}
pub(crate) type NativeTargetSources = BTreeMap<NativeGroupBinding, TargetHomeSource>;

/// A retained slot on code compiled in this cell, verified by its linked target.
#[derive(Clone, Copy, Debug)]
pub struct CellStageSlotWitness<'a> {
    owner: &'a AcquiredNativeCode,
    demander: NativeGroupBinding,
    group: &'a Arc<PendingCertifiedGroup>,
    global_index: u32,
    binding: ArtifactBinding,
    leaf: &'a NativeRetainedRequirement,
}
impl<'a> CellStageSlotWitness<'a> {
    pub fn owner(&self) -> &'a AcquiredNativeCode { self.owner }
    pub fn demander(&self) -> NativeGroupBinding { self.demander }
    pub fn group(&self) -> &'a Arc<PendingCertifiedGroup> { self.group }
    pub fn global_index(&self) -> u32 { self.global_index }
    pub fn binding(&self) -> ArtifactBinding { self.binding }
    pub fn leaf(&self) -> &'a NativeRetainedRequirement { self.leaf }
}
/// A retained slot inherited from a settled cell, verified by its linked target.
#[derive(Clone, Copy, Debug)]
pub struct HistoricalSlotWitness<'a> {
    owner: &'a AcquiredNativeCode,
    demander: NativeGroupBinding,
    global_index: u32,
    binding: ArtifactBinding,
    leaf: &'a NativeRetainedRequirement,
}
impl<'a> HistoricalSlotWitness<'a> {
    pub fn owner(&self) -> &'a AcquiredNativeCode { self.owner }
    pub fn demander(&self) -> NativeGroupBinding { self.demander }
    pub fn global_index(&self) -> u32 { self.global_index }
    pub fn binding(&self) -> ArtifactBinding { self.binding }
    pub fn leaf(&self) -> &'a NativeRetainedRequirement { self.leaf }
}
#[derive(Clone, Copy, Debug)]
pub enum VerifiedRetainedSlotDemander<'a> {
    CurrentStage(CellStageSlotWitness<'a>),
    PriorProgramStage(CellStageSlotWitness<'a>),
    HistoricalInput(HistoricalSlotWitness<'a>),
}

#[derive(Debug)]
pub struct AcquiredNativeClosure {
    occurrences: BTreeMap<NativeOccurrenceId, Arc<NativeAcquiredOccurrence>>,
    artifacts: ArtifactView,
    recorded: Arc<NativeAcquiredClosureFacts>,
    record_bytes: Arc<[u8]>,
    identity: [u8; 32],
}
impl AcquiredNativeClosure {
    pub(crate) fn from_occurrences(
        occurrences: Vec<NativeAcquiredOccurrence>,
        mut artifacts: ArtifactView,
        operation: &InventoryOperation,
    ) -> Result<Arc<Self>, CompileError> {
        operation
            .reserve::<NativeAcquiredOccurrence>(occurrences.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let mut nodes = BTreeMap::new();
        let mut receipts = BTreeMap::<[u8; 32], BTreeMap<u32, NativePackageOccurrenceFacts>>::new();
        for node in occurrences {
            crate::host_work::checkpoint()?;
            if let Some(grant) = node.home_grant() {
                artifacts = artifacts.retain_custody(grant.artifacts())?;
            }
            if let Some(grant) = node.package_grant() {
                let row = grant.row();
                if let Some(previous) = receipts
                    .entry(*grant.stage_id())
                    .or_default()
                    .insert(grant.index(), row.clone())
                {
                    if previous != *row {
                        return Err(refusal("package occurrence receipt collision"));
                    }
                }
            }
            if nodes.insert(node.occurrence(), Arc::new(node)).is_some() {
                return Err(refusal("duplicate exact acquired occurrence"));
            }
        }
        // Local occurrence constructors close their grant/body/slot correlation.
        // Joining intact occurrences still checks that every exact selected
        // child is present with the same immutable linked code.
        for node in nodes.values() {
            for (edge, slot) in node.code().imports().zip(node.slots()) {
                crate::host_work::checkpoint()?;
                operation
                    .charge(1)
                    .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
                if let (
                    NativeCodeImport::Code { code, .. },
                    CheckedNativeSlot::Code { occurrence, .. },
                ) = (edge, slot)
                {
                    let child = nodes
                        .get(occurrence)
                        .ok_or_else(|| refusal("acquired child occurrence is absent"))?;
                    if child.code() != &code {
                        return Err(refusal("acquired child changed its linked code"));
                    }
                }
            }
        }
        operation
            .reserve::<NativeOccurrenceFacts>(nodes.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let mut occurrence_facts = Vec::new();
        for node in nodes.values() {
            operation
                .reserve::<NativeGrantSelector>(node.slots().len())
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            occurrence_facts.push(NativeOccurrenceFacts {
                occurrence: node.occurrence(),
                code: node.code().location(),
                imports: node.selectors(),
            });
        }
        let recorded = Arc::new(NativeAcquiredClosureFacts {
            version: 1,
            occurrences: occurrence_facts,
            package_receipts: receipts
                .into_iter()
                .map(|(stage_id, rows)| {
                    Arc::new(NativePackageReceiptFacts {
                        stage_id,
                        occurrences: rows.into_values().collect(),
                    })
                })
                .collect(),
        });
        let bytes = encode_record(recorded.as_ref(), operation)?;
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
        self.node().code()
    }
    pub fn occurrence(&self) -> NativeOccurrenceId {
        self.occurrence
    }
    pub fn closure(&self) -> &Arc<AcquiredNativeClosure> {
        &self.owner
    }
    pub fn grant(&self) -> NativeCodeGrant<'_> {
        let current = match self.occurrence {
            NativeOccurrenceId::Home(binding) => self
                .sources
                .as_ref()
                .and_then(|sources| sources.get(&binding))
                .and_then(TargetHomeSource::cell_stage_group),
            NativeOccurrenceId::Package { .. } => None,
        };
        self.node().grant(current)
    }
    pub fn imports(&self) -> impl ExactSizeIterator<Item = AcquiredNativeImport> + '_ {
        self.node().slots().iter().map(|slot| match slot {
            CheckedNativeSlot::Code { occurrence, value } => AcquiredNativeImport::Code {
                acquired: Self {
                    owner: Arc::clone(&self.owner),
                    occurrence: *occurrence,
                    sources: self.sources.clone(),
                },
                value: *value,
            },
            CheckedNativeSlot::Retained {
                requirement,
                binding,
            } => AcquiredNativeImport::Retained {
                requirement: requirement.clone(),
                binding: *binding,
            },
        })
    }
    /// Compare immutable value facts selected by this exact acquired import.
    /// Runtime scope, produced-value receipt and lease checks remain separate.
    pub fn matches_retained_checked_value(
        &self,
        global_index: u32,
        binding: ArtifactBinding,
        row: &crate::checked_cell::CheckedValueArtifact,
    ) -> Result<bool, CompileError> {
        let index = usize::try_from(global_index)
            .map_err(|_| refusal("retained import index is out of range"))?;
        match self.node().slots().get(index) {
            Some(CheckedNativeSlot::Retained {
                binding: selected, ..
            }) if *selected == binding => {}
            _ => return Ok(false),
        }
        let selected = self
            .owner
            .artifacts
            .observed_payload(&binding.artifact)
            .ok_or_else(|| refusal("acquired retained import lost its immutable payload"))?;
        let (actual, _) = row.world_type_parts()?;
        Ok(selected.as_ref() == actual.as_ref())
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
            pending.extend(node.children());
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
    RetainedCapture(LinkedEntryRetainedCapture),
}
/// Direct target captures retain their actual typed-entry slot association.
#[derive(Clone, Debug)]
pub struct LinkedEntryRetainedOwner {
    pub(crate) owner: AcquiredNativeCode,
    pub(crate) global_index: u32,
    pub(crate) binding: ArtifactBinding,
}
impl LinkedEntryRetainedOwner {
    pub fn owner(&self) -> &AcquiredNativeCode { &self.owner }
    pub fn global_index(&self) -> u32 { self.global_index }
    pub fn binding(&self) -> ArtifactBinding { self.binding }
}
#[derive(Clone, Debug)]
pub struct LinkedEntryRetainedCapture {
    pub(crate) requirement: NativeRetainedRequirement,
    pub(crate) entry_owner: Option<LinkedEntryRetainedOwner>,
}
impl LinkedEntryRetainedCapture {
    pub fn requirement(&self) -> &NativeRetainedRequirement { &self.requirement }
    pub fn entry_owner(&self) -> Option<&LinkedEntryRetainedOwner> { self.entry_owner.as_ref() }
}
#[derive(Debug)]
pub struct LinkedNativeTarget {
    pub(crate) prepared: Arc<PreparedProgram>,
    pub(crate) imports: Arc<[LinkedNativeTargetImport]>,
    pub(crate) closure: Arc<AcquiredNativeClosure>,
    pub(crate) sources: Arc<NativeTargetSources>,
    pub(crate) entry_occurrence: Option<NativeOccurrenceId>,
}
impl LinkedNativeTarget {
    pub fn entry_occurrence(&self) -> Option<NativeOccurrenceId> { self.entry_occurrence }
    pub fn get(&self, occurrence: NativeOccurrenceId) -> Option<AcquiredNativeCode> {
        self.closure.get(occurrence).map(|code| code.reborrow(&self.sources))
    }
    pub fn verify_retained_slot<'a>(
        &'a self,
        owner: &'a AcquiredNativeCode,
        global_index: u32,
        binding: ArtifactBinding,
        leaf: &'a NativeRetainedRequirement,
    ) -> Result<VerifiedRetainedSlotDemander<'a>, CompileError> {
        if !self.owns_acquired(owner) {
            return Err(refusal("native source receipt belongs to another receiving target"));
        }
        let NativeOccurrenceId::Home(demander) = owner.occurrence() else {
            return Err(refusal("retained native requirement owner is not HOME code"));
        };
        let NativeCodeGrant::Home { binding: grant_binding, product, .. } = owner.grant() else {
            return Err(refusal("native source receipt is not HOME code"));
        };
        if grant_binding != demander {
            return Err(refusal("native source receipt binding mismatch"));
        }
        let index = usize::try_from(global_index)
            .map_err(|_| refusal("retained import index is out of range"))?;
        match owner.imports().nth(index) {
            Some(AcquiredNativeImport::Retained { requirement, binding: expected })
                if expected == binding && &requirement == leaf => {}
            _ => return Err(refusal("retained native requirement does not match owner slot")),
        }
        match self.sources.get(&demander)
            .ok_or_else(|| refusal("selected HOME occurrence lacks target source classification"))? {
            TargetHomeSource::CurrentStage(group) | TargetHomeSource::PriorProgramStage(group) => {
                if !group.same_issuance(product) {
                    return Err(refusal("native source receipt names another physical row"));
                }
                let witness = CellStageSlotWitness { owner, demander, group, global_index, binding, leaf };
                Ok(match self.sources[&demander] {
                    TargetHomeSource::CurrentStage(_) => VerifiedRetainedSlotDemander::CurrentStage(witness),
                    TargetHomeSource::PriorProgramStage(_) => VerifiedRetainedSlotDemander::PriorProgramStage(witness),
                    TargetHomeSource::HistoricalInput => unreachable!(),
                })
            }
            TargetHomeSource::HistoricalInput => Ok(VerifiedRetainedSlotDemander::HistoricalInput(
                HistoricalSlotWitness { owner, demander, global_index, binding, leaf }
            )),
        }
    }
    pub(crate) fn owns_acquired(&self, selected: &AcquiredNativeCode) -> bool {
        Arc::ptr_eq(&self.closure, &selected.owner)
            && selected
                .sources
                .as_ref()
                .is_some_and(|sources| Arc::ptr_eq(sources, &self.sources))
    }
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

pub(crate) fn resolve_package_export(
    product: &crate::certified_products::CertifiedPackageCodeProduct,
    global: &tidepool_repr::execution_schema::GlobalDecl,
    importer: tidepool_repr::execution_schema::DefinitionsView<'_>,
    interface: &[u8; 32],
) -> Result<tidepool_repr::execution_schema::ValueId, CompileError> {
    let code = product.code();
    if global.identity.unit != code.owner().unit
        || global.identity.module != code.owner().module
        || interface != product.defining_interface().sha256()
    {
        return Err(refusal(
            "selected package defining interface differs from its exact obligation",
        ));
    }
    let value = code
        .exported_values()
        .find_map(|(identity, value)| (identity == &global.identity).then_some(value))
        .ok_or_else(|| refusal("selected package does not export the demanded identity"))?;
    let contract = code
        .definitions()
        .export_contract(value)?
        .ok_or_else(|| refusal("selected package export has no contract"))?;
    let signature = global
        .entry_signature
        .map(|id| {
            importer
                .signatures()
                .get(id.0 as usize)
                .ok_or_else(|| refusal("package slot signature is out of range"))
        })
        .transpose()?;
    if contract.rep != global.rep
        || signature.is_some_and(|required| contract.entry_signature != Some(required))
        || global.required_evaluated && !contract.evaluated
        || global.required_generation.is_some()
    {
        return Err(refusal(
            "selected package export contract differs from the importer",
        ));
    }
    Ok(value)
}
pub(crate) fn resolve_package_import(
    product: &crate::certified_products::CertifiedPackageCodeProduct,
    slot: usize,
    child: &crate::certified_products::CertifiedPackageCodeProduct,
) -> Result<tidepool_repr::execution_schema::ValueId, CompileError> {
    let code = product.code();
    let owner = code.owner();
    let child_owner = child.code().owner();
    let global = code
        .globals()
        .get(slot)
        .ok_or_else(|| refusal("package ordered slot is absent"))?;
    let witness = product
        .import_interface_witnesses()
        .get(slot)
        .ok_or_else(|| refusal("package slot lacks its certified interface"))?;
    if owner.compiler_sha256 != child_owner.compiler_sha256
        || owner.profile_sha256 != child_owner.profile_sha256
        || witness.unit() != global.identity.unit
        || witness.module() != global.identity.module
    {
        return Err(refusal(
            "selected package child differs from its certified compiler/interface contract",
        ));
    }
    resolve_package_export(child, global, code.definitions(), witness.sha256())
}

/// Preserve operational budget refusals through compiler admission classification.
pub(crate) fn linked_code_error(
    error: tidepool_repr::native_code::NativeCodeError,
) -> CompileError {
    match error {
        tidepool_repr::native_code::NativeCodeError::Budget(error) => {
            CompileError::CompilerEvidence(Box::new(error.into()))
        }
        error => refusal(&format!("native linked code rejected: {error}")),
    }
}

fn encode_record(
    recorded: &NativeAcquiredClosureFacts,
    operation: &InventoryOperation,
) -> Result<Vec<u8>, CompileError> {
    struct Writer<'a> {
        operation: &'a InventoryOperation,
        bytes: Vec<u8>,
        budget_error: Option<tidepool_repr::execution_schema::ParseError>,
    }
    impl std::io::Write for Writer<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if let Err(error) = self.operation.charge(bytes.len()) {
                self.budget_error = Some(error);
                return Err(std::io::Error::other(
                    "native acquisition record budget exhausted",
                ));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Writer {
        operation,
        bytes: Vec::new(),
        budget_error: None,
    };
    let result = ciborium::ser::into_writer(recorded, &mut writer);
    if let Some(error) = writer.budget_error {
        return Err(CompileError::CompilerEvidence(Box::new(error.into())));
    }
    result.map_err(|error| refusal(&format!("acquired closure encoding: {error}")))?;
    Ok(writer.bytes)
}

#[cfg(test)]
pub(crate) mod tests;
