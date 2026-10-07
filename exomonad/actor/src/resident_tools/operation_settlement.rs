//! Provider custody for one already issued native operation.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderFinalizationKind {
    Completed,
    Aborted,
    /// Confirms resource settlement after retirement, never publication.
    RetirementAborted,
    /// Confirms that no native execution was admitted for this issued call.
    RejectedBeforeAdmission,
}

#[derive(Clone, Debug)]
pub enum HostedOperationTerminal {
    Pending,
    Settled(WorkbenchCancellationOutcome),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostedOperationFinalization {
    Pending,
    Settled(Result<ProviderFinalizationKind, String>),
}

#[derive(Default)]
pub(crate) struct ProviderFinalization {
    admitted: std::sync::atomic::AtomicBool,
    result: parking_lot::Mutex<Option<Result<ProviderFinalizationKind, String>>>,
    changed: tokio::sync::Notify,
}

impl ProviderFinalization {
    pub(crate) fn admit(&self) {
        self.admitted
            .store(true, std::sync::atomic::Ordering::Release);
    }

    pub(crate) fn finish(&self, result: Result<ProviderFinalizationKind, String>) {
        let mut retained = self.result.lock();
        if retained.is_none() {
            *retained = Some(result);
            self.changed.notify_waiters();
        }
    }

    pub(crate) fn reject_before_admission(&self) {
        if !self.admitted.load(std::sync::atomic::Ordering::Acquire) {
            self.finish(Ok(ProviderFinalizationKind::RejectedBeforeAdmission));
        }
    }

    fn observation(&self) -> HostedOperationFinalization {
        match self.result.lock().clone() {
            Some(result) => HostedOperationFinalization::Settled(result),
            None => HostedOperationFinalization::Pending,
        }
    }

    async fn settled(&self) -> Result<ProviderFinalizationKind, String> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(result) = self.result.lock().clone() {
                return result;
            }
            changed.await;
        }
    }
}

enum ProviderNativeCustody {
    Computing(Arc<WorkbenchExecutionControl>),
    Finished(Arc<WorkbenchExecutionControl>),
    Acknowledged {
        outcome: WorkbenchCancellationOutcome,
        reply: crate::KernelWorkbenchReply,
    },
}

/// Issued only beside an actual transport entry. Native completion and provider
/// acknowledgement are separate lifetimes; actor retirement does not erase the
/// native outcome or authorize a new call.
#[derive(Clone)]
pub struct HostedOperationSettlement {
    actor: crate::ActorRef,
    key: WorkbenchCallKey,
    native: Arc<parking_lot::Mutex<ProviderNativeCustody>>,
    finalization: Arc<ProviderFinalization>,
}

/// A control may refer back to its issued owner without retaining itself.
pub(crate) struct HostedOperationWeak {
    actor: crate::ActorRef,
    key: WorkbenchCallKey,
    native: std::sync::Weak<parking_lot::Mutex<ProviderNativeCustody>>,
    finalization: std::sync::Weak<ProviderFinalization>,
}

impl HostedOperationWeak {
    pub(crate) fn upgrade(&self) -> Option<HostedOperationSettlement> {
        Some(HostedOperationSettlement {
            actor: self.actor,
            key: self.key.clone(),
            native: self.native.upgrade()?,
            finalization: self.finalization.upgrade()?,
        })
    }
}

impl HostedOperationSettlement {
    pub(crate) fn issue(actor: crate::ActorRef, control: Arc<WorkbenchExecutionControl>) -> Self {
        let owner = Self {
            actor,
            key: control
                .invocation
                .clone()
                .expect("issued provider invocation"),
            finalization: control.provider_finalization.clone(),
            native: Arc::new(parking_lot::Mutex::new(ProviderNativeCustody::Computing(
                control.clone(),
            ))),
        };
        let _ = control.provider_owner.set(HostedOperationWeak {
            actor,
            key: owner.key.clone(),
            native: Arc::downgrade(&owner.native),
            finalization: Arc::downgrade(&owner.finalization),
        });
        owner
    }

    pub(crate) fn same_owner(&self, other: &Self) -> bool {
        self.actor == other.actor
            && self.key == other.key
            && Arc::ptr_eq(&self.native, &other.native)
            && Arc::ptr_eq(&self.finalization, &other.finalization)
    }

    pub(crate) fn key(&self) -> &WorkbenchCallKey {
        &self.key
    }

    pub(crate) fn native_finished(&self) {
        let mut native = self.native.lock();
        if let ProviderNativeCustody::Computing(control) = &*native {
            *native = ProviderNativeCustody::Finished(control.clone());
        }
    }

    pub(crate) fn control(&self) -> Option<Arc<WorkbenchExecutionControl>> {
        match &*self.native.lock() {
            ProviderNativeCustody::Computing(control)
            | ProviderNativeCustody::Finished(control) => Some(control.clone()),
            ProviderNativeCustody::Acknowledged { .. } => None,
        }
    }

    pub fn terminal(&self) -> HostedOperationTerminal {
        match &*self.native.lock() {
            ProviderNativeCustody::Acknowledged { outcome, .. } => {
                HostedOperationTerminal::Settled(outcome.clone())
            }
            ProviderNativeCustody::Computing(control)
            | ProviderNativeCustody::Finished(control) => match control.terminal_reply() {
                Some(reply) => HostedOperationTerminal::Settled(
                    control.cancellation_outcome(control.execution_id(self.actor), reply),
                ),
                None => HostedOperationTerminal::Pending,
            },
        }
    }

    pub(crate) fn reply(&self) -> Option<crate::KernelWorkbenchReply> {
        match &*self.native.lock() {
            ProviderNativeCustody::Acknowledged { reply, .. } => Some(reply.clone()),
            ProviderNativeCustody::Computing(control)
            | ProviderNativeCustody::Finished(control) => control.terminal_reply(),
        }
    }

    pub fn finalization(&self) -> HostedOperationFinalization {
        self.finalization.observation()
    }

    pub(crate) async fn acknowledge(&self) -> Result<ProviderFinalizationKind, ResidentToolError> {
        let result = self
            .finalization
            .settled()
            .await
            .map_err(ResidentToolError::Unavailable)?;
        let mut native = self.native.lock();
        let (outcome, reply) = match &*native {
            ProviderNativeCustody::Acknowledged { .. } => return Ok(result),
            ProviderNativeCustody::Computing(_) => {
                return Err(ResidentToolError::Unavailable(
                    "native operation is still computing".into(),
                ))
            }
            ProviderNativeCustody::Finished(control) => {
                let reply = control.terminal_reply().ok_or_else(|| {
                    ResidentToolError::Unavailable(
                        "native operation lacks an immutable terminal reply".into(),
                    )
                })?;
                if !control.cell_finished()
                    && result != ProviderFinalizationKind::RejectedBeforeAdmission
                {
                    return Err(ResidentToolError::Unavailable(
                        "native cleanup lacks an owning cell exit".into(),
                    ));
                }
                (
                    control.cancellation_outcome(control.execution_id(self.actor), reply.clone()),
                    reply,
                )
            }
        };
        if matches!(
            outcome,
            WorkbenchCancellationOutcome::Unconfirmed { .. }
                | WorkbenchCancellationOutcome::UnknownEvaluation { .. }
        ) {
            return Err(ResidentToolError::Unavailable(
                "native operation cleanup is unconfirmed".into(),
            ));
        }
        // Keep compact repeated-ack evidence; release the control's source,
        // compiler and context-binding custody after its actual finalizer.
        *native = ProviderNativeCustody::Acknowledged { outcome, reply };
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActorId, ActorRef, CellExitCause, KernelStep};
    use exomonad_tool::{ConversationOrigin, OriginalOperation, ToolInvocationOrigin};
    use tidepool_runtime::session::{WorkbenchForkBoundary, WorkbenchRunStatus};

    fn issued(
        call: &str,
    ) -> (
        ActorRef,
        Arc<WorkbenchExecutionControl>,
        WorkbenchForkBoundary,
    ) {
        let actor = ActorRef::first(ActorId(7));
        let original = OriginalOperation {
            origin: ConversationOrigin::Embedded {
                run: "run".into(),
                actor: "/root".into(),
                incarnation: "1".into(),
            },
            request_id: "request".into(),
            call_id: call.into(),
        };
        let control = WorkbenchExecutionControl::from_invocation(Some(ToolInvocationContext {
            origin: ToolInvocationOrigin::Model(original.clone()),
            call_id: call.into(),
            namespace: None,
        }));
        (actor, control, WorkbenchForkBoundary::Hosted(original))
    }

    fn native_finish(
        actor: ActorRef,
        control: &Arc<WorkbenchExecutionControl>,
        clean: bool,
    ) -> crate::CellExit {
        let response = WorkbenchResponse {
            status: WorkbenchRunStatus::Completed,
            summary: None,
            items: Vec::new(),
            next_index: 0,
            total: 0,
            publication: None,
        };
        let exit = control.finish_cell(
            control.execution_id(actor),
            &Ok(KernelStep::Continue(response.clone())),
            clean,
        );
        control.settle(Ok(response));
        exit
    }

    #[tokio::test]
    async fn native_terminal_retirement_and_provider_ack_histories_preserve_exact_evidence() {
        // Independent event oracle: retirement/cancellation before the native
        // cutoff wins cancellation; successful provider completion processed
        // before retirement remains immutable. Scheduler notification order is
        // deliberately absent from this native ownership model.
        for history in ["NCR", "NRC", "RNC", "CNR", "CRN", "RCN"] {
            let (actor, control, boundary) = issued(history);
            let slot = crate::kernel::HostedCellPublications::default();
            slot.publish_provider_transport(actor, control.clone());
            slot.accept(&control);
            let owner = slot.retained_boundary(&boundary).unwrap();
            let weak = Arc::downgrade(&control);
            let mut native = false;
            let mut retired = false;
            let mut commit_requested = false;
            let mut expected_kind = None;
            let mut expected_cancelled = false;
            for event in history.chars() {
                match event {
                    'N' => {
                        expected_cancelled = retired;
                        let exit = native_finish(actor, &control, true);
                        assert_eq!(
                            exit.cause,
                            if expected_cancelled {
                                CellExitCause::Cancelled
                            } else {
                                CellExitCause::FullReturn
                            }
                        );
                        slot.complete(&control);
                        native = true;
                    }
                    'R' => {
                        retired = true;
                        if !native {
                            control.request_cancellation();
                        }
                    }
                    'C' => {
                        commit_requested = true;
                    }
                    _ => unreachable!(),
                }
                if native && expected_kind.is_none() && (retired || commit_requested) {
                    let kind = if retired {
                        ProviderFinalizationKind::RetirementAborted
                    } else {
                        ProviderFinalizationKind::Completed
                    };
                    control.provider_finalization.finish(Ok(kind));
                    expected_kind = Some(kind);
                }
            }
            slot.finalization_owner_lost();
            assert!(
                slot.find(|_| true).is_none(),
                "native computation ended: {history}"
            );
            assert!(
                matches!(
                    owner.terminal(),
                    HostedOperationTerminal::Settled(
                        WorkbenchCancellationOutcome::Cancelled { .. }
                    )
                ) == expected_cancelled
            );
            drop(control);
            assert_eq!(owner.acknowledge().await.unwrap(), expected_kind.unwrap());
            assert!(
                weak.upgrade().is_none(),
                "ack releases large native custody: {history}"
            );
            assert_eq!(
                owner.acknowledge().await.unwrap(),
                expected_kind.unwrap(),
                "repeated ack is immutable"
            );
            assert!(
                matches!(
                    owner.terminal(),
                    HostedOperationTerminal::Settled(
                        WorkbenchCancellationOutcome::Cancelled { .. }
                    )
                ) == expected_cancelled
            );
        }
    }

    #[tokio::test]
    async fn unknown_nested_and_unclean_operations_cannot_acknowledge_retirement() {
        let (actor, control, boundary) = issued("original");
        let slot = crate::kernel::HostedCellPublications::default();
        assert!(slot.retained_boundary(&boundary).is_err());
        slot.publish_provider_transport(actor, control.clone());
        slot.accept(&control);
        let owner = slot.retained_boundary(&boundary).unwrap();
        native_finish(actor, &control, false);
        slot.complete(&control);
        control
            .provider_finalization
            .finish(Ok(ProviderFinalizationKind::RetirementAborted));
        assert!(owner.acknowledge().await.is_err());
        assert!(
            owner.control().is_some(),
            "unconfirmed cleanup retains custody"
        );
        let mut nested = control.invocation.as_ref().unwrap().invocation().clone();
        nested.call_id = "subcell".into();
        let nested = WorkbenchExecutionControl::from_invocation(Some(nested));
        slot.publish_provider_transport(actor, nested);
        assert!(
            slot.retained_boundary(&boundary).is_err(),
            "subcell is not an enclosing terminal"
        );
        let (_, other, other_boundary) = issued("other");
        assert!(slot.retained_boundary(&other_boundary).is_err());
        assert!(slot
            .retained_operation(other.invocation.as_ref().unwrap())
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn finalizer_failure_and_owner_loss_preserve_unconfirmed_custody() {
        for failure in [Some("scope cleanup failed"), None] {
            let (actor, control, boundary) = issued("failed");
            let slot = crate::kernel::HostedCellPublications::default();
            slot.publish_provider_transport(actor, control.clone());
            slot.accept(&control);
            native_finish(actor, &control, true);
            slot.complete(&control);
            if let Some(failure) = failure {
                control.provider_finalization.finish(Err(failure.into()));
            }
            slot.finalization_owner_lost();
            let owner = slot.retained_boundary(&boundary).unwrap();
            assert!(owner.acknowledge().await.is_err());
            control
                .provider_finalization
                .finish(Ok(ProviderFinalizationKind::Completed));
            assert!(
                owner.acknowledge().await.is_err(),
                "late success cannot erase retained cleanup failure"
            );
            assert!(owner.control().is_some());
        }
    }

    #[tokio::test]
    async fn deferred_transport_keeps_pending_custody_and_restores_active_tracking() {
        let (actor, control, boundary) = issued("deferred");
        let slot = crate::kernel::HostedCellPublications::default();
        slot.publish_provider_transport(actor, control.clone());
        slot.accept(&control);
        slot.complete(&control);
        assert!(slot.find(|_| true).is_none());
        let owner = slot.retained_boundary(&boundary).unwrap();
        assert!(matches!(owner.terminal(), HostedOperationTerminal::Pending));
        assert!(matches!(
            owner.finalization(),
            HostedOperationFinalization::Pending
        ));
        slot.claim(&control);
        assert!(slot
            .find(|candidate| std::ptr::eq(candidate, control.as_ref()))
            .is_some());
        native_finish(actor, &control, true);
        slot.complete(&control);
        control
            .provider_finalization
            .finish(Ok(ProviderFinalizationKind::Completed));
        assert_eq!(
            owner.acknowledge().await.unwrap(),
            ProviderFinalizationKind::Completed
        );
        assert!(slot.find(|_| true).is_none());
    }

    #[tokio::test]
    async fn pre_effect_rejection_has_issued_evidence_and_survives_transport_acceptance_race() {
        let (actor, control, boundary) = issued("rejected");
        let slot = crate::kernel::HostedCellPublications::default();
        slot.publish_provider_transport(actor, control.clone());
        control.settle(Err(crate::KernelInvocationFailure::Rejected {
            actor,
            receipts: Vec::new(),
            detail: "sealed".into(),
            diagnostic: None,
        }));
        control.provider_finalization.reject_before_admission();
        slot.complete(&control);
        slot.accept(&control);
        slot.withdraw_transport(&control);
        let owner = slot.retained_boundary(&boundary).unwrap();
        assert_eq!(
            owner.acknowledge().await.unwrap(),
            ProviderFinalizationKind::RejectedBeforeAdmission
        );
    }
}
