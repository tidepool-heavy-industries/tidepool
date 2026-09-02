//! Rust-owned worker lifecycle layered over ordinary local actors.

use std::collections::HashMap;

use parking_lot::{Mutex, RwLock};
use serde::Deserialize;

use crate::{
    AcknowledgementDisposition, ActorExitKind, ActorRef, ActorTerminal, LocalActorRef,
    WorkerCollection, WorkerHandle, WorkerLedger, WorkerPhase, WorkerStartResult, WorkerTerminal,
};
use tidepool_runtime::session::ResidentHole;

pub(crate) enum ResidentWorkerRequest {
    ReserveBatch {
        specs: serde_json::Value,
        continuation: ResidentHole,
    },
    Attach {
        handle: String,
        actor: ActorRef,
        continuation: ResidentHole,
    },
    FailStart {
        handle: String,
        detail: String,
        continuation: ResidentHole,
    },
    Submit {
        handle: String,
        receipt: serde_json::Value,
        continuation: ResidentHole,
    },
    List {
        continuation: ResidentHole,
    },
    Collect {
        handles: serde_json::Value,
        continuation: ResidentHole,
    },
    Acknowledge {
        acknowledgements: serde_json::Value,
        continuation: ResidentHole,
    },
    SessionContext {
        continuation: ResidentHole,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerWake {
    pub event: u64,
    pub handle: WorkerHandle,
}

#[derive(Default)]
pub(crate) struct WorkerRuntime {
    root: RwLock<Option<ActorRef>>,
    ledger: Mutex<WorkerLedger<serde_json::Value>>,
    by_actor: Mutex<HashMap<ActorRef, WorkerHandle>>,
    submitted: Mutex<HashMap<WorkerHandle, serde_json::Value>>,
    wakes: Mutex<Vec<WorkerWake>>,
    next_event: Mutex<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestedWorker {
    key: String,
    assignment: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestedAcknowledgement {
    worker: HandleWire,
    disposition: DispositionWire,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HandleWire {
    worker_id: String,
}

#[derive(Deserialize)]
#[serde(tag = "tag")]
enum DispositionWire {
    IntegratedAs { oid: String },
    Reviewed,
    Rejected { reason: String },
}

impl WorkerRuntime {
    pub(crate) fn install_root(&self, actor: ActorRef) {
        *self.root.write() = Some(actor);
    }

    fn require_root(&self, actor: ActorRef) -> Result<(), String> {
        if self.root.read().as_ref() == Some(&actor) {
            Ok(())
        } else {
            Err("worker administration is available only to the owning root actor".into())
        }
    }

    pub(crate) fn reserve_batch(
        &self,
        actor: ActorRef,
        specs: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.require_root(actor)?;
        let requested: Vec<RequestedWorker> =
            serde_json::from_value(specs).map_err(|error| error.to_string())?;
        let specs = requested
            .into_iter()
            .map(|spec| crate::WorkerSpec::new(spec.key, spec.assignment))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let reserved = self.ledger.lock().reserve_batch(specs);
        Ok(serde_json::Value::Array(
            reserved
                .into_iter()
                .map(|reservation| start_json(&reservation.result))
                .collect(),
        ))
    }

    pub(crate) fn attach(
        &self,
        owner: ActorRef,
        handle: String,
        actor: LocalActorRef,
    ) -> Result<serde_json::Value, String> {
        self.require_root(owner)?;
        let handle = WorkerHandle::from_raw(handle);
        let accepted = {
            let mut ledger = self.ledger.lock();
            ledger
                .commit_started_handle(&handle)
                .map_err(|error| error.to_string())?;
            ledger
                .accepted(&handle)
                .cloned()
                .ok_or_else(|| format!("worker {handle} disappeared after attachment"))?
        };
        self.by_actor
            .lock()
            .insert(actor.identity(), handle.clone());
        Ok(start_json(&WorkerStartResult::Accepted(accepted)))
    }

    pub(crate) fn fail_start(
        &self,
        actor: ActorRef,
        handle: String,
        detail: String,
    ) -> Result<serde_json::Value, String> {
        self.require_root(actor)?;
        let handle = WorkerHandle::from_raw(handle);
        let reported_detail = detail.clone();
        let accepted = {
            let mut ledger = self.ledger.lock();
            ledger
                .fail_start_handle(&handle, detail)
                .map_err(|error| error.to_string())?;
            ledger
                .accepted(&handle)
                .cloned()
                .ok_or_else(|| format!("worker {handle} disappeared after failed start"))?
        };
        Ok(start_json(&WorkerStartResult::Failed {
            accepted,
            detail: reported_detail,
        }))
    }

    pub(crate) fn submit(
        &self,
        actor: ActorRef,
        handle: String,
        receipt: serde_json::Value,
    ) -> Result<(), String> {
        let handle = WorkerHandle::from_raw(handle);
        if self.by_actor.lock().get(&actor) != Some(&handle) {
            return Err("worker submission did not match the executing actor principal".into());
        }
        if self.submitted.lock().insert(handle, receipt).is_some() {
            return Err("worker submitted more than one candidate receipt".into());
        }
        Ok(())
    }

    pub(crate) fn child_exited(
        &self,
        actor: ActorRef,
        terminal: &ActorTerminal,
    ) -> Option<WorkerWake> {
        let handle = self.by_actor.lock().remove(&actor)?;
        let submitted = self.submitted.lock().remove(&handle);
        let outcome =
            match terminal.kind {
                ActorExitKind::Completed => submitted
                    .map(WorkerTerminal::Completed)
                    .unwrap_or_else(|| WorkerTerminal::Failed {
                        detail: "worker exited without submitting a candidate receipt".into(),
                    }),
                ActorExitKind::Failed => WorkerTerminal::Failed {
                    detail: terminal.summary.clone(),
                },
                ActorExitKind::Cancelled => WorkerTerminal::Cancelled {
                    detail: terminal.summary.clone(),
                },
            };
        if let Err(error) = self.ledger.lock().settle(&handle, outcome) {
            tracing::error!(
                worker = %handle.as_str(),
                actor = ?actor,
                %error,
                "failed to settle an exited worker in the authoritative ledger"
            );
            return None;
        }
        let mut next = self.next_event.lock();
        *next += 1;
        let wake = WorkerWake {
            event: *next,
            handle,
        };
        self.wakes.lock().push(wake.clone());
        Some(wake)
    }

    pub(crate) fn list(&self, actor: ActorRef) -> Result<serde_json::Value, String> {
        self.require_root(actor)?;
        Ok(serde_json::Value::Array(
            self.ledger.lock().list().iter().map(summary_json).collect(),
        ))
    }

    pub(crate) fn collect(
        &self,
        actor: ActorRef,
        handles: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.require_root(actor)?;
        let handles: Vec<HandleWire> =
            serde_json::from_value(handles).map_err(|error| error.to_string())?;
        let mut ledger = self.ledger.lock();
        Ok(serde_json::Value::Array(
            handles
                .into_iter()
                .map(|handle| {
                    collection_json(&ledger.collect(&WorkerHandle::from_raw(handle.worker_id)))
                })
                .collect(),
        ))
    }

    pub(crate) fn acknowledge(
        &self,
        actor: ActorRef,
        acknowledgements: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.require_root(actor)?;
        let acknowledgements: Vec<RequestedAcknowledgement> =
            serde_json::from_value(acknowledgements).map_err(|error| error.to_string())?;
        let mut ledger = self.ledger.lock();
        Ok(serde_json::Value::Array(
            acknowledgements
                .into_iter()
                .map(|request| {
                    let disposition = match request.disposition {
                        DispositionWire::IntegratedAs { oid } => {
                            AcknowledgementDisposition::IntegratedAs(oid)
                        }
                        DispositionWire::Reviewed => AcknowledgementDisposition::Reviewed,
                        DispositionWire::Rejected { reason } => {
                            AcknowledgementDisposition::Rejected { reason }
                        }
                    };
                    acknowledgement_json(&ledger.acknowledge(
                        &WorkerHandle::from_raw(request.worker.worker_id),
                        disposition,
                    ))
                })
                .collect(),
        ))
    }

    pub(crate) fn take_wakes(&self, actor: ActorRef) -> Result<Vec<WorkerWake>, String> {
        self.require_root(actor)?;
        Ok(std::mem::take(&mut *self.wakes.lock()))
    }
}

fn accepted_json(accepted: &crate::AcceptedWorker) -> serde_json::Value {
    serde_json::json!({
        "key": accepted.key,
        "handle": { "workerId": accepted.handle.as_str() },
        "fingerprint": accepted.fingerprint,
    })
}

fn start_json(result: &WorkerStartResult) -> serde_json::Value {
    match result {
        WorkerStartResult::Accepted(accepted) => {
            serde_json::json!({"tag":"WorkerAccepted","accepted":accepted_json(accepted)})
        }
        WorkerStartResult::Existing(accepted) => {
            serde_json::json!({"tag":"WorkerAlreadyRunning","accepted":accepted_json(accepted)})
        }
        WorkerStartResult::Conflict {
            key,
            existing_fingerprint,
            requested_fingerprint,
        } => {
            serde_json::json!({"tag":"WorkerStartConflict","key":key,"existingFingerprint":existing_fingerprint,"requestedFingerprint":requested_fingerprint})
        }
        WorkerStartResult::Failed { accepted, detail } => {
            serde_json::json!({"tag":"WorkerStartFailed","accepted":accepted_json(accepted),"detail":detail})
        }
        WorkerStartResult::Tombstoned(accepted) => {
            serde_json::json!({"tag":"WorkerTombstoned","accepted":accepted_json(accepted)})
        }
    }
}

fn summary_json(summary: &crate::WorkerSummary) -> serde_json::Value {
    serde_json::json!({
        "accepted": accepted_json(&summary.accepted),
        "phase": match summary.phase {
            WorkerPhase::Provisioning => "WorkerProvisioning",
            WorkerPhase::Running => "WorkerRunning",
            WorkerPhase::Terminal => "WorkerTerminal",
            WorkerPhase::Collected => "WorkerCollectedPhase",
            WorkerPhase::Acknowledged => "WorkerAcknowledgedPhase",
            WorkerPhase::StartFailed => "WorkerStartFailedPhase",
        }
    })
}

fn terminal_json(terminal: &WorkerTerminal<serde_json::Value>) -> serde_json::Value {
    match terminal {
        WorkerTerminal::Completed(receipt) => {
            serde_json::json!({"tag":"WorkCompleted","receipt":receipt})
        }
        WorkerTerminal::Failed { detail } => {
            serde_json::json!({"tag":"WorkFailed","detail":detail})
        }
        WorkerTerminal::Cancelled { detail } => {
            serde_json::json!({"tag":"WorkCancelled","detail":detail})
        }
    }
}

fn collection_json(collection: &WorkerCollection<serde_json::Value>) -> serde_json::Value {
    match collection {
        WorkerCollection::Pending(handle) => {
            serde_json::json!({"tag":"WorkerPending","worker":{"workerId":handle.as_str()}})
        }
        WorkerCollection::Collected { handle, outcome } => {
            serde_json::json!({"tag":"WorkerCollected","worker":{"workerId":handle.as_str()},"outcome":terminal_json(outcome)})
        }
        WorkerCollection::Acknowledged { handle, outcome } => {
            serde_json::json!({"tag":"WorkerCollectionAcknowledged","worker":{"workerId":handle.as_str()},"outcome":terminal_json(outcome)})
        }
        WorkerCollection::NotFound(handle) => {
            serde_json::json!({"tag":"WorkerNotFound","worker":{"workerId":handle.as_str()}})
        }
    }
}

fn acknowledgement_json(
    acknowledgement: &crate::WorkerAcknowledgement<serde_json::Value>,
) -> serde_json::Value {
    use crate::WorkerAcknowledgement::*;
    match acknowledgement {
        Acknowledged {
            handle,
            disposition,
            custody,
            ..
        } => {
            serde_json::json!({"tag":"WorkerAcknowledged","worker":{"workerId":handle.as_str()},"disposition":disposition_json(disposition),"custody":custody_json(custody)})
        }
        AlreadyAcknowledged {
            handle,
            disposition,
            custody,
            ..
        } => {
            serde_json::json!({"tag":"WorkerAlreadyAcknowledged","worker":{"workerId":handle.as_str()},"disposition":disposition_json(disposition),"custody":custody_json(custody)})
        }
        NotCollected(handle) => {
            serde_json::json!({"tag":"WorkerNotCollected","worker":{"workerId":handle.as_str()}})
        }
        NotFound(handle) => {
            serde_json::json!({"tag":"WorkerAcknowledgementUnknown","worker":{"workerId":handle.as_str()}})
        }
    }
}

fn disposition_json(disposition: &AcknowledgementDisposition) -> serde_json::Value {
    match disposition {
        AcknowledgementDisposition::IntegratedAs(oid) => {
            serde_json::json!({"tag":"IntegratedAs","oid":oid})
        }
        AcknowledgementDisposition::Reviewed => serde_json::json!({"tag":"Reviewed"}),
        AcknowledgementDisposition::Rejected { reason } => {
            serde_json::json!({"tag":"Rejected","reason":reason})
        }
    }
}

fn custody_json(custody: &crate::WorkerCustody) -> serde_json::Value {
    match custody {
        crate::WorkerCustody::WorktreeRetained => {
            serde_json::json!({"tag":"WorktreeRetained"})
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ActorId;

    #[test]
    fn typed_json_boundary_preserves_idempotency_collection_and_tombstones() {
        let runtime = WorkerRuntime::default();
        let root = ActorRef::first(ActorId(1));
        runtime.install_root(root);
        let request = serde_json::json!([
            {"key":"implementation","assignment":"build it"},
            {"key":"review","assignment":"review it"}
        ]);
        let first = runtime.reserve_batch(root, request.clone()).unwrap();
        let retried = runtime.reserve_batch(root, request).unwrap();
        assert_eq!(first[0]["tag"], "WorkerAccepted");
        assert_eq!(retried[0]["tag"], "WorkerAlreadyRunning");
        assert_eq!(runtime.list(root).unwrap().as_array().unwrap().len(), 2);

        let handle = first[0]["accepted"]["handle"]["workerId"]
            .as_str()
            .unwrap()
            .to_owned();
        runtime
            .fail_start(root, handle.clone(), "launch failed".into())
            .unwrap();
        let collected = runtime
            .collect(root, serde_json::json!([{"workerId":handle}]))
            .unwrap();
        assert_eq!(collected[0]["tag"], "WorkerCollected");
        assert_eq!(collected[0]["outcome"]["tag"], "WorkFailed");
        let acknowledged = runtime
            .acknowledge(
                root,
                serde_json::json!([{
                    "worker":{"workerId":handle},
                    "disposition":{"tag":"Reviewed"}
                }]),
            )
            .unwrap();
        assert_eq!(acknowledged[0]["tag"], "WorkerAcknowledged");
        let tombstoned = runtime
            .reserve_batch(
                root,
                serde_json::json!([{"key":"implementation","assignment":"build it"}]),
            )
            .unwrap();
        assert_eq!(tombstoned[0]["tag"], "WorkerTombstoned");
    }

    #[test]
    fn worker_administration_is_root_scoped() {
        let runtime = WorkerRuntime::default();
        let root = ActorRef::first(ActorId(1));
        runtime.install_root(root);
        let child = ActorRef::first(ActorId(2));
        assert!(runtime.list(child).is_err());
        assert!(runtime.reserve_batch(child, serde_json::json!([])).is_err());
        assert!(runtime.take_wakes(child).is_err());
    }
}
