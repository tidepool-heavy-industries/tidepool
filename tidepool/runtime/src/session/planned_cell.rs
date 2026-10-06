//! Read-only reservations for a parser-certified ordered cell.
//!
//! The runtime producer reserves every original declaration and native capture
//! identity before authoritative whole-cell checking.
//! These projections have no public constructor and grant no installed-value
//! authority. Original products remain compiler-owned; runtime settlement is
//! required before a later item can use a preceding native value.

use std::sync::Arc;
use tidepool_repr::Generation;

/// Parser item order is preserved, including import-only prologues.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimePlannedCellItemKind {
    Prologue,
    Declaration,
    Bind,
    Expression,
}

/// Observational projection of the identities reserved for one item. These
/// values cannot construct a reservation or authorize a compiler offer.
#[derive(Debug)]
pub enum RuntimePlannedCellSlot {
    Prologue {
        declaration: Generation,
    },
    Declaration {
        declaration: Generation,
    },
    Bind {
        value: Generation,
    },
    Expression {
        capture: Generation,
        observation_name: String,
    },
}

/// One immutable row issued by the runtime reservation owner.
///
/// Prologues and declarations consume original Lib identities. Binds consume
/// native Val identities. Expressions reserve one capture Val identity,
/// together with its owning observation name.
#[derive(Debug)]
pub struct RuntimePlannedCellItem {
    pub(super) index: usize,
    pub(super) slot: RuntimePlannedCellSlot,
}

impl RuntimePlannedCellItem {
    pub fn index(&self) -> usize {
        self.index
    }
    pub fn kind(&self) -> RuntimePlannedCellItemKind {
        match self.slot {
            RuntimePlannedCellSlot::Prologue { .. } => RuntimePlannedCellItemKind::Prologue,
            RuntimePlannedCellSlot::Declaration { .. } => RuntimePlannedCellItemKind::Declaration,
            RuntimePlannedCellSlot::Bind { .. } => RuntimePlannedCellItemKind::Bind,
            RuntimePlannedCellSlot::Expression { .. } => RuntimePlannedCellItemKind::Expression,
        }
    }
    pub fn slot(&self) -> &RuntimePlannedCellSlot {
        &self.slot
    }
    pub fn declaration_generation(&self) -> Option<Generation> {
        match self.slot {
            RuntimePlannedCellSlot::Prologue { declaration }
            | RuntimePlannedCellSlot::Declaration { declaration } => Some(declaration),
            _ => None,
        }
    }
    pub fn value_generation(&self) -> Option<Generation> {
        match self.slot {
            RuntimePlannedCellSlot::Bind { value } => Some(value),
            RuntimePlannedCellSlot::Expression { capture, .. } => Some(capture),
            _ => None,
        }
    }
    pub fn observation_name(&self) -> Option<&str> {
        match &self.slot {
            RuntimePlannedCellSlot::Expression {
                observation_name, ..
            } => Some(observation_name),
            _ => None,
        }
    }
}

/// Frozen generation slots for one opaque compiler parser receipt.
///
/// The receipt digest and observed producer identity identify the retained
/// parser result. They do not authorize caller-supplied plans or source roots.
/// Issuance requires the actual opaque parser receipt and original private
/// admission; no public constructor accepts these digest observations.
#[derive(Debug)]
pub struct RuntimeCellPlanReservation {
    pub(super) plan: Arc<tidepool_toolchain::cell_plan::ParsedCellPlan>,
    pub(super) items: Vec<RuntimePlannedCellItem>,
    pub(super) digest: [u8; 32],
}

impl RuntimeCellPlanReservation {
    pub fn plan_digest(&self) -> [u8; 32] {
        self.plan.digest()
    }
    pub fn producer_sha256(&self) -> [u8; 32] {
        self.plan.producer_sha256()
    }
    pub fn plan(&self) -> &Arc<tidepool_toolchain::cell_plan::ParsedCellPlan> {
        &self.plan
    }
    pub fn items(&self) -> &[RuntimePlannedCellItem] {
        &self.items
    }
    pub fn compiler_specification(
        &self,
    ) -> tidepool_toolchain::checked_cell::CheckedPlannedCellSpecification {
        use tidepool_toolchain::checked_cell::CheckedPlannedCellSlot as Checked;
        let slots = self
            .items
            .iter()
            .map(|item| match &item.slot {
                RuntimePlannedCellSlot::Prologue { declaration } => Checked::Prologue {
                    declaration: declaration.0,
                },
                RuntimePlannedCellSlot::Declaration { declaration } => Checked::Declaration {
                    declaration: declaration.0,
                },
                RuntimePlannedCellSlot::Bind { value } => Checked::Bind { value: value.0 },
                RuntimePlannedCellSlot::Expression {
                    capture,
                    observation_name,
                } => Checked::Expression {
                    capture: capture.0,
                    observation_name: observation_name.clone(),
                },
            })
            .collect();
        tidepool_toolchain::checked_cell::CheckedPlannedCellSpecification {
            parsed_plan: self.plan.clone(),
            reservation_digest: self.digest,
            slots,
        }
    }
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
}
