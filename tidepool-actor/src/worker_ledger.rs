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
    Tombstoned(AcceptedWorker),
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
pub enum WorkerTerminal<R> {
    Completed(R),
    Failed { detail: String },
    Cancelled { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerCollection<R> {
    Pending(WorkerHandle),
    Collected {
        handle: WorkerHandle,
        outcome: WorkerTerminal<R>,
    },
    Acknowledged {
        handle: WorkerHandle,
        outcome: WorkerTerminal<R>,
    },
    NotFound(WorkerHandle),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcknowledgementDisposition {
    IntegratedAs(String),
    Reviewed,
    Rejected { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerCustody {
    /// V0 retains the managed worktree and branch for later inspection.
    WorktreeRetained,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerAcknowledgement<R> {
    Acknowledged {
        handle: WorkerHandle,
        disposition: AcknowledgementDisposition,
        custody: WorkerCustody,
        outcome: WorkerTerminal<R>,
    },
    AlreadyAcknowledged {
        handle: WorkerHandle,
        disposition: AcknowledgementDisposition,
        custody: WorkerCustody,
        outcome: WorkerTerminal<R>,
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

enum EntryState<R> {
    Reserved,
    Running,
    Terminal {
        outcome: WorkerTerminal<R>,
        collected: bool,
    },
    Acknowledged {
        outcome: WorkerTerminal<R>,
        disposition: AcknowledgementDisposition,
        custody: WorkerCustody,
    },
    StartFailed {
        detail: String,
        collected: bool,
    },
}

struct WorkerEntry<R> {
    spec: WorkerSpec,
    accepted: AcceptedWorker,
    state: EntryState<R>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkerLedgerError {
    #[error("worker reservation is unknown or no longer pending: {0}")]
    InvalidReservation(WorkerHandle),
    #[error("worker is unknown: {0}")]
    Unknown(WorkerHandle),
    #[error("worker is already terminal: {0}")]
    AlreadyTerminal(WorkerHandle),
}

impl std::fmt::Display for WorkerHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

pub struct WorkerLedger<R> {
    by_key: HashMap<String, WorkerHandle>,
    entries: HashMap<WorkerHandle, WorkerEntry<R>>,
}

impl<R> Default for WorkerLedger<R> {
    fn default() -> Self {
        Self {
            by_key: HashMap::new(),
            entries: HashMap::new(),
        }
    }
}

impl<R> WorkerLedger<R> {
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
                    WorkerStartResult::Tombstoned(entry.accepted.clone())
                }
                EntryState::StartFailed { detail, .. } => WorkerStartResult::Failed {
                    accepted: entry.accepted.clone(),
                    detail: detail.clone(),
                },
                EntryState::Reserved | EntryState::Running | EntryState::Terminal { .. } => {
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
    ) -> Result<(), WorkerLedgerError> {
        let handle = reservation.accepted.handle;
        let Some(entry) = self.entries.get_mut(&handle) else {
            return Err(WorkerLedgerError::InvalidReservation(handle));
        };
        if !matches!(entry.state, EntryState::Reserved) {
            return Err(WorkerLedgerError::InvalidReservation(handle));
        }
        entry.state = EntryState::Running;
        Ok(())
    }

    pub fn commit_started_handle(
        &mut self,
        handle: &WorkerHandle,
    ) -> Result<(), WorkerLedgerError> {
        let Some(entry) = self.entries.get_mut(handle) else {
            return Err(WorkerLedgerError::InvalidReservation(handle.clone()));
        };
        if !matches!(entry.state, EntryState::Reserved) {
            return Err(WorkerLedgerError::InvalidReservation(handle.clone()));
        }
        entry.state = EntryState::Running;
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

    pub fn settle(
        &mut self,
        handle: &WorkerHandle,
        outcome: WorkerTerminal<R>,
    ) -> Result<(), WorkerLedgerError> {
        let Some(entry) = self.entries.get_mut(handle) else {
            return Err(WorkerLedgerError::Unknown(handle.clone()));
        };
        if !matches!(entry.state, EntryState::Running) {
            return Err(WorkerLedgerError::AlreadyTerminal(handle.clone()));
        }
        entry.state = EntryState::Terminal {
            outcome,
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
}

impl<R: Clone> WorkerLedger<R> {
    #[must_use]
    pub fn collect(&mut self, handle: &WorkerHandle) -> WorkerCollection<R> {
        let Some(entry) = self.entries.get_mut(handle) else {
            return WorkerCollection::NotFound(handle.clone());
        };
        match &mut entry.state {
            EntryState::Reserved | EntryState::Running => WorkerCollection::Pending(handle.clone()),
            EntryState::Terminal { outcome, collected } => {
                *collected = true;
                WorkerCollection::Collected {
                    handle: handle.clone(),
                    outcome: outcome.clone(),
                }
            }
            EntryState::Acknowledged { outcome, .. } => WorkerCollection::Acknowledged {
                handle: handle.clone(),
                outcome: outcome.clone(),
            },
            EntryState::StartFailed { detail, collected } => {
                *collected = true;
                WorkerCollection::Collected {
                    handle: handle.clone(),
                    outcome: WorkerTerminal::Failed {
                        detail: detail.clone(),
                    },
                }
            }
        }
    }

    #[must_use]
    pub fn acknowledge(
        &mut self,
        handle: &WorkerHandle,
        disposition: AcknowledgementDisposition,
    ) -> WorkerAcknowledgement<R> {
        let Some(entry) = self.entries.get_mut(handle) else {
            return WorkerAcknowledgement::NotFound(handle.clone());
        };
        match &entry.state {
            EntryState::Terminal {
                outcome,
                collected: true,
            } => {
                let outcome = outcome.clone();
                entry.state = EntryState::Acknowledged {
                    outcome: outcome.clone(),
                    disposition: disposition.clone(),
                    custody: WorkerCustody::WorktreeRetained,
                };
                WorkerAcknowledgement::Acknowledged {
                    handle: handle.clone(),
                    disposition,
                    custody: WorkerCustody::WorktreeRetained,
                    outcome,
                }
            }
            EntryState::StartFailed {
                detail,
                collected: true,
            } => {
                let outcome = WorkerTerminal::Failed {
                    detail: detail.clone(),
                };
                entry.state = EntryState::Acknowledged {
                    outcome: outcome.clone(),
                    disposition: disposition.clone(),
                    custody: WorkerCustody::WorktreeRetained,
                };
                WorkerAcknowledgement::Acknowledged {
                    handle: handle.clone(),
                    disposition,
                    custody: WorkerCustody::WorktreeRetained,
                    outcome,
                }
            }
            EntryState::Acknowledged {
                outcome,
                disposition,
                custody,
            } => WorkerAcknowledgement::AlreadyAcknowledged {
                handle: handle.clone(),
                disposition: disposition.clone(),
                custody: custody.clone(),
                outcome: outcome.clone(),
            },
            EntryState::Reserved
            | EntryState::Running
            | EntryState::Terminal {
                collected: false, ..
            }
            | EntryState::StartFailed {
                collected: false, ..
            } => WorkerAcknowledgement::NotCollected(handle.clone()),
        }
    }
}

fn phase<R>(state: &EntryState<R>) -> WorkerPhase {
    match state {
        EntryState::Reserved => WorkerPhase::Provisioning,
        EntryState::Running => WorkerPhase::Running,
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
        let mut ledger = WorkerLedger::<String>::default();
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
    fn collection_is_replayable_and_acknowledgement_owns_cleanup() {
        let mut ledger = WorkerLedger::<String>::default();
        let mut reserved = ledger.reserve_batch(vec![spec("worker", "do work")]);
        let reservation = reserved[0].reservation.take().expect("reservation");
        let handle = reservation.accepted().handle.clone();
        ledger.commit_started(reservation).expect("commit start");
        assert_eq!(
            ledger.collect(&handle),
            WorkerCollection::Pending(handle.clone())
        );
        ledger
            .settle(&handle, WorkerTerminal::Completed("receipt".into()))
            .expect("settle");
        assert_eq!(
            ledger.acknowledge(&handle, AcknowledgementDisposition::Reviewed),
            WorkerAcknowledgement::NotCollected(handle.clone())
        );

        let first = ledger.collect(&handle);
        assert_eq!(ledger.collect(&handle), first);
        let acknowledged = ledger.acknowledge(
            &handle,
            AcknowledgementDisposition::IntegratedAs("abc123".into()),
        );
        assert!(matches!(
            acknowledged,
            WorkerAcknowledgement::Acknowledged {
                custody: WorkerCustody::WorktreeRetained,
                ..
            }
        ));
        let repeated = ledger.acknowledge(
            &handle,
            AcknowledgementDisposition::Rejected {
                reason: "must not replace disposition".into(),
            },
        );
        assert!(matches!(
            repeated,
            WorkerAcknowledgement::AlreadyAcknowledged {
                disposition: AcknowledgementDisposition::IntegratedAs(ref oid),
                custody: WorkerCustody::WorktreeRetained,
                ..
            } if oid == "abc123"
        ));
        assert!(matches!(
            ledger.collect(&handle),
            WorkerCollection::Acknowledged {
                outcome: WorkerTerminal::Completed(ref receipt),
                ..
            } if receipt == "receipt"
        ));
    }

    #[test]
    fn failed_provisioning_is_collectible_acknowledgeable_and_tombstoned() {
        let mut ledger = WorkerLedger::<String>::default();
        let mut reserved = ledger.reserve_batch(vec![spec("worker", "do work")]);
        let accepted = accepted(&reserved[0]);
        ledger
            .fail_start(
                reserved[0].reservation.take().expect("reservation"),
                "could not launch",
            )
            .expect("fail start");
        assert!(matches!(
            ledger.collect(&accepted.handle),
            WorkerCollection::Collected {
                outcome: WorkerTerminal::Failed { ref detail },
                ..
            } if detail == "could not launch"
        ));
        assert!(matches!(
            ledger.acknowledge(&accepted.handle, AcknowledgementDisposition::Reviewed),
            WorkerAcknowledgement::Acknowledged { .. }
        ));
        let retried = ledger.reserve_batch(vec![spec("worker", "do work")]);
        assert_eq!(retried[0].result, WorkerStartResult::Tombstoned(accepted));
    }
}
