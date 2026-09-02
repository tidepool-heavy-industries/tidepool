//! Interpreter-owned worker idempotency, result custody, and retention facts.

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WorkerHandle(String);

impl WorkerHandle {
    #[must_use]
    pub fn fresh() -> Self {
        Self(format!("worker-{}", uuid::Uuid::new_v4()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn from_raw(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerSpec {
    pub key: String,
    pub assignment: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkerSpecError {
    #[error("worker key must not be empty")]
    EmptyKey,
    #[error("worker assignment must not be empty")]
    EmptyAssignment,
}

impl WorkerSpec {
    pub fn new(
        key: impl Into<String>,
        assignment: impl Into<String>,
    ) -> Result<Self, WorkerSpecError> {
        let key = key.into();
        let assignment = normalize_assignment(assignment.into());
        if key.trim().is_empty() {
            return Err(WorkerSpecError::EmptyKey);
        }
        if assignment.trim().is_empty() {
            return Err(WorkerSpecError::EmptyAssignment);
        }
        let mut fingerprint = blake3::Hasher::new();
        fingerprint.update(key.as_bytes());
        fingerprint.update(&[0]);
        fingerprint.update(assignment.as_bytes());
        Ok(Self {
            key,
            assignment,
            fingerprint: fingerprint.finalize().to_hex().to_string(),
        })
    }
}

fn normalize_assignment(assignment: String) -> String {
    assignment.replace("\r\n", "\n")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedWorker {
    pub key: String,
    pub handle: WorkerHandle,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerStartResult {
    Accepted(AcceptedWorker),
    Existing(AcceptedWorker),
    Conflict {
        key: String,
        existing_fingerprint: String,
        requested_fingerprint: String,
    },
    Failed {
        accepted: AcceptedWorker,
        detail: String,
    },
    Acknowledged(AcceptedWorker),
}

#[derive(Debug)]
pub struct WorkerReservation {
    accepted: AcceptedWorker,
}

impl WorkerReservation {
    #[must_use]
    pub fn accepted(&self) -> &AcceptedWorker {
        &self.accepted
    }
}

#[derive(Debug)]
pub struct ReservedWorker {
    pub result: WorkerStartResult,
    pub reservation: Option<WorkerReservation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerInspection {
    Pending(WorkerHandle),
    Ready(WorkerHandle),
    StartFailed {
        handle: WorkerHandle,
        detail: String,
    },
    Acknowledged(WorkerHandle),
    NotFound(WorkerHandle),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcknowledgementDisposition {
    IntegratedAs(String),
    Reviewed,
    Rejected { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerAcknowledgement {
    Acknowledged {
        handle: WorkerHandle,
        disposition: AcknowledgementDisposition,
    },
    AlreadyAcknowledged {
        handle: WorkerHandle,
        disposition: AcknowledgementDisposition,
    },
    NotCollected(WorkerHandle),
    NotFound(WorkerHandle),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerPhase {
    Provisioning,
    Running,
    Terminal,
    Collected,
    Acknowledged,
    StartFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerSummary {
    pub accepted: AcceptedWorker,
    pub phase: WorkerPhase,
}

enum EntryState<A> {
    Reserved,
    Running {
        attachment: A,
    },
    Terminal {
        attachment: A,
        collected: bool,
    },
    Acknowledged {
        disposition: AcknowledgementDisposition,
    },
    StartFailed {
        detail: String,
        collected: bool,
    },
}

struct WorkerEntry<A> {
    spec: WorkerSpec,
    accepted: AcceptedWorker,
    state: EntryState<A>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkerLedgerError {
    #[error("worker reservation is unknown or no longer pending: {0}")]
    InvalidReservation(WorkerHandle),
    #[error("worker is unknown: {0}")]
    Unknown(WorkerHandle),
    #[error("worker is already terminal: {0}")]
    AlreadyTerminal(WorkerHandle),
    #[error("worker exit is not ready for collection: {0}")]
    ExitNotReady(WorkerHandle),
}

impl std::fmt::Display for WorkerHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

pub struct WorkerLedger<A> {
    by_key: HashMap<String, WorkerHandle>,
    entries: HashMap<WorkerHandle, WorkerEntry<A>>,
}

impl<A> Default for WorkerLedger<A> {
    fn default() -> Self {
        Self {
            by_key: HashMap::new(),
            entries: HashMap::new(),
        }
    }
}

impl<A: Clone> WorkerLedger<A> {
    #[must_use]
    pub fn accepted(&self, handle: &WorkerHandle) -> Option<&AcceptedWorker> {
        self.entries.get(handle).map(|entry| &entry.accepted)
    }

    #[must_use]
    pub fn reserve_batch(&mut self, specs: Vec<WorkerSpec>) -> Vec<ReservedWorker> {
        specs.into_iter().map(|spec| self.reserve(spec)).collect()
    }

    fn reserve(&mut self, spec: WorkerSpec) -> ReservedWorker {
        if let Some(handle) = self.by_key.get(&spec.key) {
            let Some(entry) = self.entries.get(handle) else {
                unreachable!("key index always points to a worker entry");
            };
            if entry.spec.fingerprint != spec.fingerprint {
                return ReservedWorker {
                    result: WorkerStartResult::Conflict {
                        key: spec.key,
                        existing_fingerprint: entry.spec.fingerprint.clone(),
                        requested_fingerprint: spec.fingerprint,
                    },
                    reservation: None,
                };
            }
            let result = match &entry.state {
                EntryState::Acknowledged { .. } => {
                    WorkerStartResult::Acknowledged(entry.accepted.clone())
                }
                EntryState::StartFailed { detail, .. } => WorkerStartResult::Failed {
                    accepted: entry.accepted.clone(),
                    detail: detail.clone(),
                },
                EntryState::Reserved | EntryState::Running { .. } | EntryState::Terminal { .. } => {
                    WorkerStartResult::Existing(entry.accepted.clone())
                }
            };
            return ReservedWorker {
                result,
                reservation: None,
            };
        }

        let handle = WorkerHandle::fresh();
        let accepted = AcceptedWorker {
            key: spec.key.clone(),
            handle: handle.clone(),
            fingerprint: spec.fingerprint.clone(),
        };
        self.by_key.insert(spec.key.clone(), handle.clone());
        self.entries.insert(
            handle,
            WorkerEntry {
                spec,
                accepted: accepted.clone(),
                state: EntryState::Reserved,
            },
        );
        ReservedWorker {
            result: WorkerStartResult::Accepted(accepted.clone()),
            reservation: Some(WorkerReservation { accepted }),
        }
    }

    pub fn commit_started(
        &mut self,
        reservation: WorkerReservation,
        attachment: A,
    ) -> Result<(), WorkerLedgerError> {
        let handle = reservation.accepted.handle;
        let Some(entry) = self.entries.get_mut(&handle) else {
            return Err(WorkerLedgerError::InvalidReservation(handle));
        };
        if !matches!(entry.state, EntryState::Reserved) {
            return Err(WorkerLedgerError::InvalidReservation(handle));
        }
        entry.state = EntryState::Running { attachment };
        Ok(())
    }

    pub fn commit_started_handle(
        &mut self,
        handle: &WorkerHandle,
        attachment: A,
    ) -> Result<(), WorkerLedgerError> {
        let Some(entry) = self.entries.get_mut(handle) else {
            return Err(WorkerLedgerError::InvalidReservation(handle.clone()));
        };
        if !matches!(entry.state, EntryState::Reserved) {
            return Err(WorkerLedgerError::InvalidReservation(handle.clone()));
        }
        entry.state = EntryState::Running { attachment };
        Ok(())
    }

    pub fn fail_start(
        &mut self,
        reservation: WorkerReservation,
        detail: impl Into<String>,
    ) -> Result<(), WorkerLedgerError> {
        let handle = reservation.accepted.handle;
        let Some(entry) = self.entries.get_mut(&handle) else {
            return Err(WorkerLedgerError::InvalidReservation(handle));
        };
        if !matches!(entry.state, EntryState::Reserved) {
            return Err(WorkerLedgerError::InvalidReservation(handle));
        }
        entry.state = EntryState::StartFailed {
            detail: detail.into(),
            collected: false,
        };
        Ok(())
    }

    pub fn fail_start_handle(
        &mut self,
        handle: &WorkerHandle,
        detail: impl Into<String>,
    ) -> Result<(), WorkerLedgerError> {
        let Some(entry) = self.entries.get_mut(handle) else {
            return Err(WorkerLedgerError::InvalidReservation(handle.clone()));
        };
        if !matches!(entry.state, EntryState::Reserved) {
            return Err(WorkerLedgerError::InvalidReservation(handle.clone()));
        }
        entry.state = EntryState::StartFailed {
            detail: detail.into(),
            collected: false,
        };
        Ok(())
    }

    pub fn settle(&mut self, handle: &WorkerHandle) -> Result<(), WorkerLedgerError> {
        let Some(entry) = self.entries.get_mut(handle) else {
            return Err(WorkerLedgerError::Unknown(handle.clone()));
        };
        if !matches!(entry.state, EntryState::Running { .. }) {
            return Err(WorkerLedgerError::AlreadyTerminal(handle.clone()));
        }
        let EntryState::Running { attachment } = &entry.state else {
            unreachable!("running state was checked above")
        };
        let attachment = attachment.clone();
        entry.state = EntryState::Terminal {
            attachment,
            collected: false,
        };
        Ok(())
    }

    #[must_use]
    pub fn list(&self) -> Vec<WorkerSummary> {
        let mut summaries: Vec<_> = self
            .entries
            .values()
            .map(|entry| WorkerSummary {
                accepted: entry.accepted.clone(),
                phase: phase(&entry.state),
            })
            .collect();
        summaries.sort_by(|left, right| left.accepted.key.cmp(&right.accepted.key));
        summaries
    }
    #[must_use]
    pub fn inspect(&mut self, handle: &WorkerHandle) -> WorkerInspection {
        let Some(entry) = self.entries.get_mut(handle) else {
            return WorkerInspection::NotFound(handle.clone());
        };
        match &mut entry.state {
            EntryState::Reserved | EntryState::Running { .. } => {
                WorkerInspection::Pending(handle.clone())
            }
            EntryState::Terminal { .. } => WorkerInspection::Ready(handle.clone()),
            EntryState::Acknowledged { .. } => WorkerInspection::Acknowledged(handle.clone()),
            EntryState::StartFailed { detail, collected } => {
                *collected = true;
                WorkerInspection::StartFailed {
                    handle: handle.clone(),
                    detail: detail.clone(),
                }
            }
        }
    }

    #[must_use]
    pub fn find_attached(&self, mut predicate: impl FnMut(&A) -> bool) -> Option<WorkerHandle> {
        self.entries.iter().find_map(|(handle, entry)| {
            let attachment = match &entry.state {
                EntryState::Running { attachment } | EntryState::Terminal { attachment, .. } => {
                    attachment
                }
                EntryState::Reserved
                | EntryState::Acknowledged { .. }
                | EntryState::StartFailed { .. } => return None,
            };
            predicate(attachment).then(|| handle.clone())
        })
    }

    /// Borrow the attached exact exit reference and mark logical collection.
    /// The attachment itself remains in the terminal entry until acknowledgement.
    pub fn collect_attachment(&mut self, handle: &WorkerHandle) -> Result<A, WorkerLedgerError> {
        let Some(entry) = self.entries.get_mut(handle) else {
            return Err(WorkerLedgerError::Unknown(handle.clone()));
        };
        match &mut entry.state {
            EntryState::Terminal {
                attachment,
                collected,
            } => {
                *collected = true;
                Ok(attachment.clone())
            }
            _ => Err(WorkerLedgerError::ExitNotReady(handle.clone())),
        }
    }

    #[must_use]
    pub fn acknowledge(
        &mut self,
        handle: &WorkerHandle,
        disposition: AcknowledgementDisposition,
    ) -> (WorkerAcknowledgement, Option<A>) {
        let Some(entry) = self.entries.get_mut(handle) else {
            return (WorkerAcknowledgement::NotFound(handle.clone()), None);
        };
        match &entry.state {
            EntryState::Terminal {
                collected: true, ..
            } => {
                let EntryState::Terminal { attachment, .. } = &entry.state else {
                    unreachable!("terminal state was checked above")
                };
                let attachment = attachment.clone();
                entry.state = EntryState::Acknowledged {
                    disposition: disposition.clone(),
                };
                (
                    WorkerAcknowledgement::Acknowledged {
                        handle: handle.clone(),
                        disposition,
                    },
                    Some(attachment),
                )
            }
            EntryState::StartFailed {
                collected: true, ..
            } => {
                entry.state = EntryState::Acknowledged {
                    disposition: disposition.clone(),
                };
                (
                    WorkerAcknowledgement::Acknowledged {
                        handle: handle.clone(),
                        disposition,
                    },
                    None,
                )
            }
            EntryState::Acknowledged { disposition } => (
                WorkerAcknowledgement::AlreadyAcknowledged {
                    handle: handle.clone(),
                    disposition: disposition.clone(),
                },
                None,
            ),
            EntryState::Reserved
            | EntryState::Running { .. }
            | EntryState::Terminal {
                collected: false, ..
            }
            | EntryState::StartFailed {
                collected: false, ..
            } => (WorkerAcknowledgement::NotCollected(handle.clone()), None),
        }
    }
}

fn phase<A>(state: &EntryState<A>) -> WorkerPhase {
    match state {
        EntryState::Reserved => WorkerPhase::Provisioning,
        EntryState::Running { .. } => WorkerPhase::Running,
        EntryState::Terminal {
            collected: false, ..
        } => WorkerPhase::Terminal,
        EntryState::Terminal {
            collected: true, ..
        } => WorkerPhase::Collected,
        EntryState::Acknowledged { .. } => WorkerPhase::Acknowledged,
        EntryState::StartFailed { .. } => WorkerPhase::StartFailed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(key: &str, assignment: &str) -> WorkerSpec {
        WorkerSpec::new(key, assignment).expect("valid worker spec")
    }

    fn accepted(reserved: &ReservedWorker) -> AcceptedWorker {
        match &reserved.result {
            WorkerStartResult::Accepted(accepted) => accepted.clone(),
            other => panic!("expected accepted worker, got {other:?}"),
        }
    }

    #[test]
    fn batch_reservation_is_ordered_independent_and_idempotent() {
        let mut ledger = WorkerLedger::<()>::default();
        let reserved = ledger.reserve_batch(vec![spec("a", "one\r\ntwo"), spec("b", "other")]);
        assert_eq!(reserved.len(), 2);
        let a = accepted(&reserved[0]);
        let b = accepted(&reserved[1]);
        assert_ne!(a.handle, b.handle);

        let retried = ledger.reserve_batch(vec![spec("a", "one\ntwo"), spec("a", "changed")]);
        assert_eq!(retried[0].result, WorkerStartResult::Existing(a.clone()));
        assert!(retried[0].reservation.is_none());
        assert!(matches!(
            retried[1].result,
            WorkerStartResult::Conflict { ref key, .. } if key == "a"
        ));
        assert!(retried[1].reservation.is_none());
        assert_eq!(
            ledger
                .list()
                .into_iter()
                .map(|row| row.accepted.key)
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn collection_is_replayable_until_acknowledgement() {
        let mut ledger = WorkerLedger::<()>::default();
        let mut reserved = ledger.reserve_batch(vec![spec("worker", "do work")]);
        let reservation = reserved[0].reservation.take().expect("reservation");
        let handle = reservation.accepted().handle.clone();
        ledger
            .commit_started(reservation, ())
            .expect("commit start");
        assert_eq!(
            ledger.inspect(&handle),
            WorkerInspection::Pending(handle.clone())
        );
        ledger.settle(&handle).expect("settle");
        assert_eq!(
            ledger
                .acknowledge(&handle, AcknowledgementDisposition::Reviewed)
                .0,
            WorkerAcknowledgement::NotCollected(handle.clone())
        );

        assert_eq!(
            ledger.inspect(&handle),
            WorkerInspection::Ready(handle.clone())
        );
        assert_eq!(
            ledger.inspect(&handle),
            WorkerInspection::Ready(handle.clone())
        );
        assert_eq!(ledger.collect_attachment(&handle), Ok(()));
        assert_eq!(ledger.collect_attachment(&handle), Ok(()));
        let acknowledged = ledger.acknowledge(
            &handle,
            AcknowledgementDisposition::IntegratedAs("abc123".into()),
        );
        assert!(matches!(
            acknowledged.0,
            WorkerAcknowledgement::Acknowledged { .. }
        ));
        assert_eq!(acknowledged.1, Some(()));
        let repeated = ledger.acknowledge(
            &handle,
            AcknowledgementDisposition::Rejected {
                reason: "must not replace disposition".into(),
            },
        );
        assert!(matches!(
            repeated.0,
            WorkerAcknowledgement::AlreadyAcknowledged {
                disposition: AcknowledgementDisposition::IntegratedAs(ref oid),
                ..
            } if oid == "abc123"
        ));
        assert_eq!(
            ledger.inspect(&handle),
            WorkerInspection::Acknowledged(handle)
        );
    }

    #[test]
    fn failed_provisioning_is_collectible_acknowledgeable_and_reserved() {
        let mut ledger = WorkerLedger::<()>::default();
        let mut reserved = ledger.reserve_batch(vec![spec("worker", "do work")]);
        let accepted = accepted(&reserved[0]);
        ledger
            .fail_start(
                reserved[0].reservation.take().expect("reservation"),
                "could not launch",
            )
            .expect("fail start");
        assert!(matches!(
            ledger.inspect(&accepted.handle),
            WorkerInspection::StartFailed { ref detail, .. } if detail == "could not launch"
        ));
        assert!(matches!(
            ledger
                .acknowledge(&accepted.handle, AcknowledgementDisposition::Reviewed)
                .0,
            WorkerAcknowledgement::Acknowledged { .. }
        ));
        let retried = ledger.reserve_batch(vec![spec("worker", "do work")]);
        assert_eq!(retried[0].result, WorkerStartResult::Acknowledged(accepted));
    }
}
