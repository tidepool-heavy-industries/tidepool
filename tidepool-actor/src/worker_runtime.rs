//! Rust-owned worker lifecycle layered over ordinary local actors.

use parking_lot::{Mutex, RwLock};
use serde::Deserialize;
use tidepool_codegen::suspension::RealmId;
use tidepool_runtime::session::{ResidentHole, RootCustody, RootedValueRef};

use crate::{
    AcknowledgementDisposition, ActorRef, LocalActorRef, WorkerHandle, WorkerInspection,
    WorkerLedger, WorkerPhase, WorkerStartResult,
};

pub(crate) enum ResidentWorkerRequest {
    ReserveBatch {
        specs: serde_json::Value,
        continuation: ResidentHole,
    },
    Attach {
        handle: String,
        actor: ActorRef,
        exit_ref: RootCustody,
        custody_realm: RealmId,
        continuation: ResidentHole,
    },
    FailStart {
        handle: String,
        detail: String,
        continuation: ResidentHole,
    },
    List {
        continuation: ResidentHole,
    },
    Inspect {
        handles: serde_json::Value,
        continuation: ResidentHole,
    },
    BorrowExit {
        handle: String,
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

#[derive(Clone)]
pub(crate) struct WorkerExitLease {
    actor: LocalActorRef,
    exit_ref: RootedValueRef,
    custody_realm: RealmId,
}

impl WorkerExitLease {
    fn actor(&self) -> ActorRef {
        self.actor.identity()
    }

    pub(crate) fn custody_realm(&self) -> RealmId {
        self.custody_realm
    }

    pub(crate) fn exit_ref(&self) -> RootedValueRef {
        self.exit_ref.clone()
    }
}

#[derive(Default)]
pub(crate) struct WorkerRuntime {
    root: RwLock<Option<ActorRef>>,
    ledger: Mutex<WorkerLedger<WorkerExitLease>>,
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
        exit_ref: RootCustody,
        custody_realm: RealmId,
    ) -> Result<serde_json::Value, String> {
        self.require_root(owner)?;
        let handle = WorkerHandle::from_raw(handle);
        let accepted = {
            let mut ledger = self.ledger.lock();
            let lease = WorkerExitLease {
                actor: actor.clone(),
                exit_ref: exit_ref.into_rooted_ref(),
                custody_realm,
            };
            ledger
                .commit_started_handle(&handle, lease)
                .map_err(|error| error.to_string())?;
            ledger
                .accepted(&handle)
                .cloned()
                .ok_or_else(|| format!("worker {handle} disappeared after attachment"))?
        };
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

    pub(crate) fn child_exited(&self, actor: ActorRef) -> Option<WorkerWake> {
        let mut ledger = self.ledger.lock();
        let handle = ledger.find_attached(|lease| lease.actor() == actor)?;
        if let Err(error) = ledger.settle(&handle) {
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

    pub(crate) fn inspect(
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
                    inspection_json(&ledger.inspect(&WorkerHandle::from_raw(handle.worker_id)))
                })
                .collect(),
        ))
    }

    pub(crate) fn borrow_exit(
        &self,
        actor: ActorRef,
        handle: String,
    ) -> Result<RootedValueRef, String> {
        self.require_root(actor)?;
        self.ledger
            .lock()
            .collect_attachment(&WorkerHandle::from_raw(handle))
            .map(|lease| lease.exit_ref())
            .map_err(|error| error.to_string())
    }

    pub(crate) fn acknowledge(
        &self,
        actor: ActorRef,
        acknowledgements: serde_json::Value,
    ) -> Result<(serde_json::Value, Vec<WorkerExitLease>), String> {
        self.require_root(actor)?;
        let acknowledgements: Vec<RequestedAcknowledgement> =
            serde_json::from_value(acknowledgements).map_err(|error| error.to_string())?;
        let mut ledger = self.ledger.lock();
        let mut released = Vec::new();
        let values = acknowledgements
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
                let (acknowledgement, lease) = ledger.acknowledge(
                    &WorkerHandle::from_raw(request.worker.worker_id),
                    disposition,
                );
                released.extend(lease);
                acknowledgement_json(&acknowledgement)
            })
            .collect();
        Ok((serde_json::Value::Array(values), released))
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
        WorkerStartResult::Acknowledged(accepted) => {
            serde_json::json!({"tag":"WorkerStartAcknowledged","accepted":accepted_json(accepted)})
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

fn inspection_json(inspection: &WorkerInspection) -> serde_json::Value {
    match inspection {
        WorkerInspection::Pending(handle) => {
            serde_json::json!({"tag":"WorkerInspectionPending","worker":{"workerId":handle.as_str()}})
        }
        WorkerInspection::Ready(handle) => {
            serde_json::json!({"tag":"WorkerInspectionExitReady","worker":{"workerId":handle.as_str()}})
        }
        WorkerInspection::StartFailed { handle, detail } => {
            serde_json::json!({"tag":"WorkerInspectionStartFailed","worker":{"workerId":handle.as_str()},"detail":detail})
        }
        WorkerInspection::Acknowledged(handle) => {
            serde_json::json!({"tag":"WorkerInspectionAcknowledged","worker":{"workerId":handle.as_str()}})
        }
        WorkerInspection::NotFound(handle) => {
            serde_json::json!({"tag":"WorkerInspectionUnknown","worker":{"workerId":handle.as_str()}})
        }
    }
}

fn acknowledgement_json(acknowledgement: &crate::WorkerAcknowledgement) -> serde_json::Value {
    use crate::WorkerAcknowledgement::*;
    match acknowledgement {
        Acknowledged {
            handle,
            disposition,
        } => {
            serde_json::json!({"tag":"WorkerAcknowledged","worker":{"workerId":handle.as_str()},"disposition":disposition_json(disposition)})
        }
        AlreadyAcknowledged {
            handle,
            disposition,
        } => {
            serde_json::json!({"tag":"WorkerAlreadyAcknowledged","worker":{"workerId":handle.as_str()},"disposition":disposition_json(disposition)})
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ActorId;

    #[test]
    fn typed_metadata_boundary_preserves_idempotency_and_acknowledged_keys() {
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
            .inspect(root, serde_json::json!([{"workerId":handle}]))
            .unwrap();
        assert_eq!(collected[0]["tag"], "WorkerInspectionStartFailed");
        let (acknowledged, released) = runtime
            .acknowledge(
                root,
                serde_json::json!([{
                    "worker":{"workerId":handle},
                    "disposition":{"tag":"Reviewed"}
                }]),
            )
            .unwrap();
        assert!(released.is_empty());
        assert_eq!(acknowledged[0]["tag"], "WorkerAcknowledged");
        let tombstoned = runtime
            .reserve_batch(
                root,
                serde_json::json!([{"key":"implementation","assignment":"build it"}]),
            )
            .unwrap();
        assert_eq!(tombstoned[0]["tag"], "WorkerStartAcknowledged");
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
