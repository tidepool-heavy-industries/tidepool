use super::{operation_journal, CallRequest, CallResponse, OriginalOperation};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BoundaryState {
    Active,
    Reconciling,
    Pending,
    Recoverable,
    Settled,
}

#[derive(Clone)]
pub(super) struct HostedOperationLedger {
    state: Arc<Mutex<LedgerState>>,
}

struct LedgerState {
    boundaries: HashMap<OriginalOperation, BoundaryState>,
    operations: Option<parking_lot::Mutex<operation_journal::OperationJournal>>,
}

pub(super) enum CallAdmission {
    New,
    Known(CallResponse),
    Uncertain,
    BoundaryActive,
    BoundarySettled,
    JournalError(String),
}

pub(super) enum InterruptionAdmission {
    Claimed(BoundaryClaim),
    Pending,
    Settled,
}

pub(super) enum CompletionAdmission {
    Claimed(BoundaryClaim),
    Busy,
    Settled,
}

pub(super) struct BoundaryClaim {
    ledger: HostedOperationLedger,
    key: OriginalOperation,
    previous: Option<BoundaryState>,
}

impl HostedOperationLedger {
    pub(super) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(LedgerState {
                boundaries: HashMap::new(),
                operations: None,
            })),
        }
    }

    pub(super) fn with_journal(path: PathBuf, require_existing: bool) -> Result<Self, String> {
        let journal = if require_existing {
            operation_journal::OperationJournal::open_existing(path)
        } else {
            operation_journal::OperationJournal::open(path)
        }
        .map_err(|error| format!("cannot open hosted-operation journal: {error}"))?;
        let mut boundaries = journal
            .uncertain_boundaries()
            .map(|boundary| (boundary, BoundaryState::Pending))
            .collect::<HashMap<_, _>>();
        boundaries.extend(
            journal
                .settled_boundaries()
                .map(|boundary| (boundary.clone(), BoundaryState::Settled)),
        );
        Ok(Self {
            state: Arc::new(Mutex::new(LedgerState {
                boundaries,
                operations: Some(parking_lot::Mutex::new(journal)),
            })),
        })
    }

    pub(super) fn validate_recovery(path: PathBuf) -> Result<(), String> {
        operation_journal::OperationJournal::open_existing(path)
            .map(|_| ())
            .map_err(|error| {
                format!("hosted-operation recovery evidence is unavailable: {error}")
            })
    }

    /// Admission and journal acceptance share one lock so concurrent retries
    /// cannot pass the boundary gate before the first call is durably accepted.
    pub(super) async fn admit_call(&self, request: &CallRequest) -> CallAdmission {
        let key = request.context_call_id.as_ref().map(|call_id| {
            super::external_operation(
                request.thread_id.clone(),
                request.turn_id.clone(),
                call_id.clone(),
            )
        });
        let mut state = self.state.lock().await;
        if let Some(key) = &key {
            match state.boundaries.get(key) {
                Some(BoundaryState::Settled) => return CallAdmission::BoundarySettled,
                Some(
                    BoundaryState::Active
                    | BoundaryState::Reconciling
                    | BoundaryState::Pending
                    | BoundaryState::Recoverable,
                ) => return CallAdmission::BoundaryActive,
                None => {
                    state
                        .boundaries
                        .insert(key.clone(), BoundaryState::Active);
                }
            }
        }

        let admission = state
            .operations
            .as_ref()
            .map(|operations| operations.lock().admit(request));
        match admission {
            None | Some(Ok(operation_journal::Admission::New)) => CallAdmission::New,
            Some(Ok(operation_journal::Admission::Known(response))) => {
                release_active_boundary(&mut state, key.as_ref());
                CallAdmission::Known(response)
            }
            Some(Ok(operation_journal::Admission::Uncertain)) => {
                release_active_boundary(&mut state, key.as_ref());
                CallAdmission::Uncertain
            }
            Some(Err(error)) => {
                release_active_boundary(&mut state, key.as_ref());
                CallAdmission::JournalError(error.to_string())
            }
        }
    }

    /// Persist the terminal outcome before releasing an active outer boundary.
    /// If the append is uncertain, the journal fences later admissions and the
    /// durable recovery path decides whether the response was retained.
    pub(super) async fn finish_call(
        &self,
        request: &CallRequest,
        response: &CallResponse,
    ) -> Result<(), String> {
        let key = request.context_call_id.as_ref().map(|call_id| {
            super::external_operation(
                request.thread_id.clone(),
                request.turn_id.clone(),
                call_id.clone(),
            )
        });
        let mut state = self.state.lock().await;
        let result = match &state.operations {
            Some(operations) => operations
                .lock()
                .finish(request, response)
                .map_err(|error| error.to_string()),
            None => Ok(()),
        };
        release_active_boundary(&mut state, key.as_ref());
        result
    }

    pub(super) async fn claim_interruption(
        &self,
        key: OriginalOperation,
    ) -> InterruptionAdmission {
        let mut state = self.state.lock().await;
        match state.boundaries.get(&key).copied() {
            Some(BoundaryState::Reconciling) => InterruptionAdmission::Pending,
            Some(BoundaryState::Settled) => InterruptionAdmission::Settled,
            previous @ (None
            | Some(
                BoundaryState::Active | BoundaryState::Pending | BoundaryState::Recoverable,
            )) => {
                state
                    .boundaries
                    .insert(key.clone(), BoundaryState::Reconciling);
                InterruptionAdmission::Claimed(BoundaryClaim {
                    ledger: self.clone(),
                    key,
                    previous,
                })
            }
        }
    }

    pub(super) async fn claim_completion(
        &self,
        key: OriginalOperation,
    ) -> CompletionAdmission {
        let mut state = self.state.lock().await;
        match state.boundaries.get(&key).copied() {
            Some(BoundaryState::Settled) => CompletionAdmission::Settled,
            Some(
                BoundaryState::Active | BoundaryState::Reconciling | BoundaryState::Pending,
            ) => CompletionAdmission::Busy,
            previous @ (None | Some(BoundaryState::Recoverable)) => {
                state
                    .boundaries
                    .insert(key.clone(), BoundaryState::Reconciling);
                CompletionAdmission::Claimed(BoundaryClaim {
                    ledger: self.clone(),
                    key,
                    previous,
                })
            }
        }
    }
}

impl BoundaryClaim {
    pub(super) async fn restore(self) {
        let mut state = self.ledger.state.lock().await;
        restore_boundary(&mut state, &self.key, self.previous);
    }

    pub(super) async fn finish_interruption(self, next: BoundaryState) {
        self.ledger
            .state
            .lock()
            .await
            .boundaries
            .insert(self.key, next);
    }

    /// The endpoint completion is already confirmed at this point. A failed
    /// append leaves Reconciling in memory; restart reconstructs from the
    /// journal's retained prefix instead of retrying this transition in place.
    pub(super) async fn settle(self) -> Result<(), String> {
        {
            let state = self.ledger.state.lock().await;
            if let Some(operations) = &state.operations {
                operations
                    .lock()
                    .settle_boundary(self.key.clone())
                    .map_err(|error| error.to_string())?;
            }
        }
        self.ledger
            .state
            .lock()
            .await
            .boundaries
            .insert(self.key, BoundaryState::Settled);
        Ok(())
    }
}

fn release_active_boundary(state: &mut LedgerState, key: Option<&OriginalOperation>) {
    if let Some(key) = key {
        if state.boundaries.get(key) == Some(&BoundaryState::Active) {
            state.boundaries.remove(key);
        }
    }
}

fn restore_boundary(
    state: &mut LedgerState,
    key: &OriginalOperation,
    previous: Option<BoundaryState>,
) {
    if state.boundaries.get(key) != Some(&BoundaryState::Reconciling) {
        return;
    }
    match previous {
        Some(previous) => {
            state.boundaries.insert(key.clone(), previous);
        }
        None => {
            state.boundaries.remove(key);
        }
    }
}

#[cfg(test)]
impl HostedOperationLedger {
    pub(super) async fn boundary_state(
        &self,
        key: &OriginalOperation,
    ) -> Option<BoundaryState> {
        self.state.lock().await.boundaries.get(key).copied()
    }

    pub(super) async fn set_boundary_state(
        &self,
        key: OriginalOperation,
        boundary_state: BoundaryState,
    ) {
        self.state
            .lock()
            .await
            .boundaries
            .insert(key, boundary_state);
    }

    pub(super) async fn boundaries_is_empty(&self) -> bool {
        self.state.lock().await.boundaries.is_empty()
    }
}
