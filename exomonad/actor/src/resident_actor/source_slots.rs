//! Actual source registrations, native subscriptions and admitted input share
//! one stable slot. Retirement observes and closes this same owner.
use super::*;
use crate::request::sources::{
    ActorSourceConnections, SourceBinding, SourceDelivery, SourceTarget,
};
use crate::resident_workbench::generation_admission::{
    GenerationAdmissionError, GenerationDependency, SourceDependency,
};
use std::collections::VecDeque;

#[derive(Clone, Debug, PartialEq, Eq)]
/// Identity of the actual registration issuer; native forwarding never
/// replaces its actor incarnation or reuses its ordinal.
pub(crate) struct SourceRegistrationIdentity {
    pub(crate) actor: ActorRef,
    pub(crate) slot: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceRegistrationDependency {
    Independent,
    Dependent,
    Unclassified,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SourceRegistrationState {
    Open {
        deferred: usize,
        resume_failure: Option<SourceRegistrationFailure>,
    },
    Closed {
        rejected: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SourceRegistrationFailure {
    Admission(GenerationAdmissionError),
    DestinationUnavailable,
    Dispatch(String),
}
impl std::fmt::Display for SourceRegistrationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Admission(error) => write!(f, "{error}"),
            Self::DestinationUnavailable => f.write_str("source subscription is unavailable"),
            Self::Dispatch(error) => f.write_str(error),
        }
    }
}

fn temporarily_fenced(error: &GenerationAdmissionError) -> bool {
    matches!(
        error,
        GenerationAdmissionError::Closed {
            phase: tidepool_runtime::session::SourceAdmissionPhase::Fenced { .. },
            ..
        }
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceRegistrationObservation {
    pub(crate) identity: SourceRegistrationIdentity,
    pub(crate) dependency: SourceRegistrationDependency,
    pub(crate) state: SourceRegistrationState,
}

struct OpenSourceSlot {
    binding: SourceBinding,
    connection: Option<ActorSourceConnections>,
    deferred: VecDeque<(u64, SourceDelivery)>,
    next_delivery: u64,
    redispatched: Option<u64>,
    resume_failure: Option<SourceRegistrationFailure>,
}
enum SourceSlotState {
    Open(OpenSourceSlot),
    Closed {
        target: SourceTarget,
        rejected: usize,
        settled_delivery: u64,
    },
}
struct SourceSlot {
    identity: SourceRegistrationIdentity,
    state: Mutex<SourceSlotState>,
}

/// Correlates a redispatched input with the same actual slot and issuing
/// incarnation. Only this slot owner can construct it; it grants no source
/// admission and does not create an execution.
#[derive(Clone)]
pub(crate) struct SourceSlotDelivery {
    slot: std::sync::Weak<SourceSlot>,
    ordinal: u64,
}
impl std::fmt::Debug for SourceSlotDelivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceSlotDelivery")
            .field("ordinal", &self.ordinal)
            .finish_non_exhaustive()
    }
}
impl SourceSlotDelivery {
    fn matches(&self, slot: &Arc<SourceSlot>) -> bool {
        self.slot
            .upgrade()
            .is_some_and(|owner| Arc::ptr_eq(&owner, slot))
    }
}

/// The capture retains the actual registration, not a reconstructed stop address.
#[derive(Clone)]
pub(crate) struct CapturedSourceRegistration {
    slot: Arc<SourceSlot>,
    dependency: SourceRegistrationDependency,
    cutoff: tidepool_runtime::session::RuntimeSourceAdmission,
}

impl CapturedSourceRegistration {
    pub(crate) fn observation(&self) -> SourceRegistrationObservation {
        let (source, state) = match &*self.slot.state.lock() {
            SourceSlotState::Open(open) => (
                Some(open.binding.execution.source_dependency().clone()),
                SourceRegistrationState::Open {
                    deferred: open.deferred.len(),
                    resume_failure: open.resume_failure.clone(),
                },
            ),
            SourceSlotState::Closed { rejected, .. } => (
                None,
                SourceRegistrationState::Closed {
                    rejected: *rejected,
                },
            ),
        };
        SourceRegistrationObservation {
            identity: self.slot.identity.clone(),
            dependency: source.as_ref().map_or(self.dependency, |source| {
                source_relation(source, &self.cutoff)
            }),
            state,
        }
    }

    /// Redispatch retained, unrun input only after the exact source reopened.
    pub(crate) fn resume_deferred(&self) -> SourceRegistrationObservation {
        resume_slot(&self.slot);
        self.observation()
    }

    /// Drop only this registration and settle each retained logical input once.
    pub(crate) fn close(&self) -> SourceRegistrationObservation {
        let source = match &*self.slot.state.lock() {
            SourceSlotState::Open(open) => Some(open.binding.execution.source_dependency().clone()),
            _ => None,
        };
        let Some(source) = source else {
            return self.observation();
        };
        if source_relation(&source, &self.cutoff) != SourceRegistrationDependency::Dependent {
            return self.observation();
        }
        let discarded = {
            let mut state = self.slot.state.lock();
            match &*state {
                SourceSlotState::Open(open)
                    if open.binding.execution.source_dependency() == &source =>
                {
                    let closed = SourceSlotState::Closed {
                        target: open.binding.target,
                        rejected: open.deferred.len(),
                        settled_delivery: open.next_delivery - 1,
                    };
                    Some(std::mem::replace(&mut *state, closed))
                }
                SourceSlotState::Closed { .. } | SourceSlotState::Open(_) => None,
            }
        };
        // Subscription guards can enter RequestRegistry; never drop them under
        // a slot borrow or a retirement/source gate.
        drop(discarded);
        self.observation()
    }
}

#[derive(Clone, Default)]
pub(super) struct SourceSlots(Arc<Mutex<SourceSlotsState>>);
#[derive(Default)]
struct SourceSlotsState {
    owner: Option<ActorRef>,
    slots: Vec<Arc<SourceSlot>>,
    connections: Option<ActorSourceConnections>,
}

/// Held by source() across both map_source and run_receiver. Source retirement
/// cannot finish while that pre-cutoff executable callback is still active.
pub(super) struct AdmittedSourceDelivery {
    pub(super) binding: SourceBinding,
    pub(super) delivery: SourceDelivery,
    claim: Option<crate::resident_workbench::generation_admission::SourceClaim>,
    slot: Arc<SourceSlot>,
}
impl Drop for AdmittedSourceDelivery {
    fn drop(&mut self) {
        // Finish the admitted mapper/receiver before the next queued input.
        drop(self.claim.take());
        resume_slot(&self.slot);
    }
}

pub(super) enum SourceDeliveryAdmission {
    Admitted(AdmittedSourceDelivery),
    Deferred(SourceRegistrationIdentity),
    Refused(SourceRegistrationIdentity, GenerationAdmissionError),
    Closed(SourceRegistrationIdentity),
    Rejected(String),
}

impl SourceSlots {
    pub(super) fn bind_owner(&self, owner: ActorRef) {
        let mut state = self.0.lock();
        assert!(
            state.owner.is_none_or(|current| current == owner),
            "source slots retain their issuing actor"
        );
        state.owner = Some(owner);
    }
    pub(super) fn len(&self) -> usize {
        self.0.lock().slots.len()
    }
    pub(super) fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub(super) fn binding(&self, slot: usize) -> Option<SourceBinding> {
        let slot = self.0.lock().slots.get(slot)?.clone();
        let state = slot.state.lock();
        match &*state {
            SourceSlotState::Open(open) => Some(open.binding.clone()),
            SourceSlotState::Closed { .. } => None,
        }
    }
    pub(super) fn bindings(&self) -> Vec<Option<SourceBinding>> {
        let slots = self.0.lock().slots.clone();
        slots
            .into_iter()
            .map(|slot| match &*slot.state.lock() {
                SourceSlotState::Open(open) => Some(open.binding.clone()),
                SourceSlotState::Closed { .. } => None,
            })
            .collect()
    }
    pub(super) fn push(&self, binding: SourceBinding) {
        let mut state = self.0.lock();
        let identity = SourceRegistrationIdentity {
            actor: state
                .owner
                .expect("actor owns source slots before publication"),
            slot: state.slots.len(),
        };
        state.slots.push(Arc::new(SourceSlot {
            identity,
            state: Mutex::new(SourceSlotState::Open(OpenSourceSlot {
                binding,
                connection: None,
                deferred: VecDeque::new(),
                next_delivery: 1,
                redispatched: None,
                resume_failure: None,
            })),
        }));
    }
    pub(super) fn has_connections(&self) -> bool {
        self.0.lock().connections.is_some()
    }
    pub(super) fn install_connections(&self, mut connections: ActorSourceConnections) {
        let mut state = self.0.lock();
        assert!(
            state.connections.is_none(),
            "source destination is installed once"
        );
        for slot in &state.slots {
            if let SourceSlotState::Open(open) = &mut *slot.state.lock() {
                open.connection = Some(connections.take_slot(slot.identity.slot));
            }
        }
        state.connections = Some(connections);
    }
    pub(super) fn attach(
        &self,
        binding: SourceBinding,
        connect: impl FnOnce(&mut ActorSourceConnections, usize) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut state = self.0.lock();
        let ordinal = state.slots.len();
        let actor = state
            .owner
            .expect("actor owns source slots before attachment");
        let connections = state
            .connections
            .as_mut()
            .ok_or("source destination is unavailable")?;
        connect(connections, ordinal)?;
        let connection = connections.take_slot(ordinal);
        state.slots.push(Arc::new(SourceSlot {
            identity: SourceRegistrationIdentity {
                actor,
                slot: ordinal,
            },
            state: Mutex::new(SourceSlotState::Open(OpenSourceSlot {
                binding,
                connection: Some(connection),
                deferred: VecDeque::new(),
                next_delivery: 1,
                redispatched: None,
                resume_failure: None,
            })),
        }));
        Ok(())
    }
    pub(crate) fn capture(
        &self,
        dependency: &GenerationDependency,
    ) -> Vec<CapturedSourceRegistration> {
        self.capture_source(dependency.runtime_admission())
    }
    pub(super) fn capture_source(
        &self,
        dependency: &tidepool_runtime::session::RuntimeSourceAdmission,
    ) -> Vec<CapturedSourceRegistration> {
        let slots = self.0.lock().slots.clone();
        slots
            .into_iter()
            .filter_map(|slot| {
                let source = match &*slot.state.lock() {
                    SourceSlotState::Open(open) => {
                        Some(open.binding.execution.source_dependency().clone())
                    }
                    SourceSlotState::Closed { .. } => None,
                }?;
                let relation = source_relation(&source, dependency);
                if relation == SourceRegistrationDependency::Independent {
                    return None;
                }
                Some(CapturedSourceRegistration {
                    slot,
                    dependency: relation,
                    cutoff: dependency.clone(),
                })
            })
            .collect()
    }
    pub(super) fn claim_sources(
        &self,
    ) -> Result<
        Vec<crate::resident_workbench::generation_admission::SourceClaim>,
        crate::resident_workbench::generation_admission::GenerationAdmissionError,
    > {
        self.bindings()
            .into_iter()
            .flatten()
            .map(|source| {
                source.execution.source_dependency().source_lease()?;
                source.execution.source_dependency().claim()
            })
            .collect()
    }
    pub(super) fn admit_delivery(&self, delivery: SourceDelivery) -> SourceDeliveryAdmission {
        self.admit_delivery_observed(delivery, || ())
    }
    #[cfg(test)]
    pub(super) fn admit_delivery_after_source_observation(
        &self,
        delivery: SourceDelivery,
        observe: impl FnOnce(),
    ) -> SourceDeliveryAdmission {
        self.admit_delivery_observed(delivery, observe)
    }
    fn admit_delivery_observed(
        &self,
        mut delivery: SourceDelivery,
        observe: impl FnOnce(),
    ) -> SourceDeliveryAdmission {
        let Some(slot) = self.0.lock().slots.get(delivery.slot).cloned() else {
            return SourceDeliveryAdmission::Rejected(
                "source delivery names an unissued slot".into(),
            );
        };
        if delivery
            .slot_delivery
            .as_ref()
            .is_some_and(|marker| !marker.matches(&slot))
        {
            return SourceDeliveryAdmission::Rejected(
                "source redelivery belongs to another issuing slot".into(),
            );
        }
        let binding = {
            let mut state = slot.state.lock();
            match &mut *state {
                SourceSlotState::Closed {
                    target,
                    rejected,
                    settled_delivery,
                } if *target == delivery.target => {
                    if let Some(marker) = &delivery.slot_delivery {
                        if marker.ordinal > *settled_delivery {
                            return SourceDeliveryAdmission::Rejected(
                                "source redelivery was never issued".into(),
                            );
                        }
                    } else {
                        *rejected += 1;
                    }
                    return SourceDeliveryAdmission::Closed(slot.identity.clone());
                }
                SourceSlotState::Open(open) if open.binding.target == delivery.target => {
                    open.binding.clone()
                }
                _ => {
                    return SourceDeliveryAdmission::Rejected(
                        "source delivery differs from its issued target".into(),
                    )
                }
            }
        };
        // Source admission and the slot borrow are separate. A pre-cutoff claim
        // keeps capture behind the complete mapper and receiver execution.
        let admission = binding
            .execution
            .source_dependency()
            .source_lease()
            .and_then(|_| binding.execution.source_dependency().claim());
        observe();
        let mut state = slot.state.lock();
        let SourceSlotState::Open(open) = &mut *state else {
            if let SourceSlotState::Closed { rejected, .. } = &mut *state {
                if delivery.slot_delivery.is_none() {
                    *rejected += 1;
                }
            }
            return SourceDeliveryAdmission::Closed(slot.identity.clone());
        };
        if !Arc::ptr_eq(&open.binding.entry, &binding.entry) {
            return SourceDeliveryAdmission::Rejected(
                "source registration changed during delivery admission".into(),
            );
        }
        if let Some(marker) = &delivery.slot_delivery {
            let marker = marker.ordinal;
            if open.redispatched != Some(marker)
                || open.deferred.front().map(|(id, _)| *id) != Some(marker)
            {
                return SourceDeliveryAdmission::Rejected(
                    "source redelivery is stale or already consumed".into(),
                );
            }
            open.redispatched = None;
        }
        let (claim, failure) = match admission {
            Ok(claim) => (Some(claim), None),
            Err(error) => (None, Some(error)),
        };
        let refusal = match &open.resume_failure {
            Some(SourceRegistrationFailure::Admission(error)) if !temporarily_fenced(error) => {
                Some(error.clone())
            }
            _ => failure
                .as_ref()
                .filter(|error| !temporarily_fenced(error))
                .cloned(),
        };
        if claim.is_some()
            && refusal.is_none()
            && (delivery.slot_delivery.is_some() || open.deferred.is_empty())
        {
            if delivery.slot_delivery.take().is_some() {
                delivery = open
                    .deferred
                    .pop_front()
                    .expect("marked head checked above")
                    .1;
            }
            drop(state);
            return SourceDeliveryAdmission::Admitted(AdmittedSourceDelivery {
                binding,
                delivery,
                claim,
                slot,
            });
        }
        if delivery.slot_delivery.is_none() {
            let Some(next) = open.next_delivery.checked_add(1) else {
                drop(state);
                drop(claim);
                return SourceDeliveryAdmission::Rejected(
                    "source slot delivery ordinal exhausted".into(),
                );
            };
            let marker = open.next_delivery;
            open.next_delivery = next;
            open.deferred.push_back((marker, delivery));
        }
        open.resume_failure = refusal
            .clone()
            .or(failure)
            .map(SourceRegistrationFailure::Admission);
        drop(state);
        drop(claim);
        if let Some(error) = refusal {
            return SourceDeliveryAdmission::Refused(slot.identity.clone(), error);
        }
        // Post-store recheck closes reopen racing the first failed claim. The
        // same marked head is retained until admitted, never remapped on retry.
        resume_slot(&slot);
        SourceDeliveryAdmission::Deferred(slot.identity.clone())
    }
    /// Reopening redispatches only pre-mapper deliveries already owned here.
    /// The retained head is removed only when its marked mailbox input admits.
    pub(crate) fn resume_deferred(
        &self,
        dependency: &GenerationDependency,
    ) -> Vec<SourceRegistrationObservation> {
        let captures = self.capture(dependency);
        self.resume_captures(captures)
    }
    fn resume_captures(
        &self,
        captures: Vec<CapturedSourceRegistration>,
    ) -> Vec<SourceRegistrationObservation> {
        for capture in &captures {
            capture.resume_deferred();
        }
        captures
            .iter()
            .map(CapturedSourceRegistration::observation)
            .collect()
    }
    pub(super) async fn handoff(
        &self,
        grant: crate::local_actor::run_control::ReplacementIngressGrant,
    ) -> Result<(), crate::KernelCallFailure> {
        let destination = self
            .0
            .lock()
            .connections
            .as_ref()
            .map(ActorSourceConnections::destination);
        if let Some(destination) = destination {
            destination.retarget_sources(grant.clone()).await?;
        }
        let state = self.0.lock();
        for slot in &state.slots {
            if let SourceSlotState::Open(open) = &mut *slot.state.lock() {
                if let Some(connection) = &mut open.connection {
                    connection.handoff_observers(grant.successor());
                }
            }
        }
        Ok(())
    }

    /// Replacement already holds its actual source and lifecycle admission.
    /// Transfer the same slot so captures can reobserve its current binding.
    pub(super) fn adopt_replacement(
        &self,
        previous: &Self,
        bindings: Vec<Option<SourceBinding>>,
    ) -> Result<(), String> {
        let mut old = previous.0.lock();
        if old.slots.len() != bindings.len() {
            return Err("replacement changed stable source slots".into());
        }
        if old.slots.iter().zip(&bindings).any(|(slot, binding)| {
            matches!(&*slot.state.lock(), SourceSlotState::Open(_)) && binding.is_none()
        }) {
            return Err("replacement removed a live source slot".into());
        }
        let mut state = self.0.lock();
        if !state.slots.is_empty() {
            return Err("replacement recipient already owns source slots".into());
        }
        state
            .owner
            .ok_or("replacement recipient source owner is unavailable")?;
        let mut discarded = Vec::new();
        for (slot, binding) in old.slots.drain(..).zip(bindings) {
            {
                let mut slot_state = slot.state.lock();
                match (&mut *slot_state, binding) {
                    (SourceSlotState::Open(open), Some(binding)) => {
                        discarded.push(std::mem::replace(&mut open.binding, binding));
                        open.resume_failure = None;
                    }
                    (SourceSlotState::Closed { .. }, binding) => discarded.extend(binding),
                    (SourceSlotState::Open(_), None) => {
                        unreachable!("live replacement binding validated above")
                    }
                }
            }
            state.slots.push(slot);
        }
        state.connections = old.connections.take();
        drop(state);
        drop(old);
        drop(discarded);
        Ok(())
    }
    pub(super) fn close_connections(&self) {
        let (connections, subscriptions) = {
            let mut state = self.0.lock();
            let subscriptions: Vec<_> = state
                .slots
                .iter()
                .filter_map(|slot| match &mut *slot.state.lock() {
                    SourceSlotState::Open(open) => open.connection.take(),
                    _ => None,
                })
                .collect();
            (state.connections.take(), subscriptions)
        };
        drop((connections, subscriptions));
    }
    pub(super) fn clear(&self) {
        let (slots, connections) = {
            let mut state = self.0.lock();
            (std::mem::take(&mut state.slots), state.connections.take())
        };
        drop((slots, connections));
    }
}

fn resume_slot(slot: &Arc<SourceSlot>) {
    let execution = match &*slot.state.lock() {
        SourceSlotState::Open(open)
            if !open.deferred.is_empty()
                && open.redispatched.is_none()
                && !matches!(&open.resume_failure, Some(SourceRegistrationFailure::Admission(error)) if !temporarily_fenced(error)) =>
        {
            Some(open.binding.execution.clone())
        }
        _ => None,
    };
    let Some(execution) = execution else { return };
    if let Err(error) = execution.admit_activation() {
        if let SourceSlotState::Open(open) = &mut *slot.state.lock() {
            open.resume_failure = Some(SourceRegistrationFailure::Admission(error));
        }
        return;
    }
    let mut state = slot.state.lock();
    let SourceSlotState::Open(open) = &mut *state else {
        return;
    };
    if open.redispatched.is_some() {
        return;
    }
    let Some((marker, delivery)) = open.deferred.front() else {
        return;
    };
    let marker = *marker;
    let mut delivery = delivery.clone();
    delivery.slot_delivery = Some(SourceSlotDelivery {
        slot: Arc::downgrade(slot),
        ordinal: marker,
    });
    // dispatch is solely a nonblocking LifecycleCommand::Source mailbox send.
    // It cannot invoke a mapper or reenter this slot synchronously.
    let result = open
        .connection
        .as_ref()
        .ok_or(SourceRegistrationFailure::DestinationUnavailable)
        .and_then(|connection| {
            connection
                .dispatch(delivery)
                .map_err(|error| SourceRegistrationFailure::Dispatch(error.to_string()))
        });
    match result {
        Ok(()) => {
            open.redispatched = Some(marker);
            open.resume_failure = None;
        }
        Err(error) => {
            open.resume_failure = Some(error);
        }
    }
}

fn source_relation(
    source: &SourceDependency,
    cutoff: &tidepool_runtime::session::RuntimeSourceAdmission,
) -> SourceRegistrationDependency {
    match source {
        SourceDependency::Unqualified => SourceRegistrationDependency::Unclassified,
        source => match source.source_lease() {
            Ok(source) if source.source_admission().depends_on(cutoff) => {
                SourceRegistrationDependency::Dependent
            }
            Ok(_) => SourceRegistrationDependency::Independent,
            Err(_) => SourceRegistrationDependency::Unclassified,
        },
    }
}
