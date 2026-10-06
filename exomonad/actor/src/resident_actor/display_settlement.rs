use super::*;
use tidepool_runtime::session::{WorkbenchDisplayPublication, WorkbenchDisplayPublicationIdentity};

/// Completion custody for one exact admitted execution record. Controls borrow
/// this same owner so scheduler failure can freeze receipts without a journal,
/// cursor, native effect owner, or retained Haskell root.
pub(super) struct DisplayExecutionSettlement {
    execution: WorkbenchExecutionId,
    state: Mutex<DisplayExecutionState>,
}

#[derive(Default)]
struct DisplayExecutionState {
    frozen: bool,
    operations: Vec<Arc<DisplayOperationSettlement>>,
}

impl DisplayExecutionSettlement {
    pub(super) fn new(execution: WorkbenchExecutionId) -> Self {
        Self {
            execution,
            state: Mutex::new(DisplayExecutionState::default()),
        }
    }

    pub(super) fn retain(
        &self,
        settlement: Arc<DisplayOperationSettlement>,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let mut state = self.state.lock();
        if state.frozen
            || settlement.id.execution != self.execution
            || state
                .operations
                .iter()
                .any(|existing| existing.id == settlement.id)
        {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "display operation differs from its original receipt owner or is already retained"
                    .into(),
            ));
        }
        state.operations.push(settlement);
        Ok(())
    }

    /// Issuance, attachment and byte reservation share the terminal cutoff.
    /// The supplied native admission is synchronous and never runs guest code.
    fn admit_publication<T>(
        &self,
        admit: impl FnOnce() -> Result<T, ResidentActorWorkbenchError>,
    ) -> Result<T, ResidentActorWorkbenchError> {
        let state = self.state.lock();
        if state.frozen {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "display execution has already frozen its terminal receipt".into(),
            ));
        }
        admit()
    }

    pub(super) fn freeze_step(
        &self,
        result: &mut Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>,
    ) {
        let receipts = match result {
            Ok(KernelStep::Continue(response) | KernelStep::ContinueLater(response))
            | Ok(KernelStep::Stop {
                output: response, ..
            }) => Some(&mut response.items),
            Err(error) => error.receipts_mut(),
        };
        self.freeze_receipts(receipts);
    }

    fn freeze_receipts(&self, mut receipts: Option<&mut Vec<WorkbenchItemReceipt>>) {
        let mut state = self.state.lock();
        state.frozen = true;
        let operations = &state.operations;
        let mut packets = operations
            .iter()
            .filter_map(|settlement| settlement.packet())
            .collect::<Vec<_>>();
        packets.sort_by_key(|packet| Arc::as_ptr(packet) as usize);
        packets.dedup_by(|left, right| Arc::ptr_eq(left, right));
        // Freeze all ready pages as one terminal snapshot. The publisher
        // cannot retain Published until its page has crossed this fence.
        let publications = packets
            .iter()
            .map(|packet| packet.publication.lock())
            .collect::<Vec<_>>();
        for settlement in operations {
            let Some(packet) = settlement.packet() else {
                continue;
            };
            let Some(index) = packets
                .iter()
                .position(|candidate| Arc::ptr_eq(candidate, &packet))
            else {
                continue;
            };
            let Some(snapshot) = settlement.freeze(&publications[index]) else {
                continue;
            };
            if let Some(receipts) = receipts.as_deref_mut() {
                merge_display_receipt(receipts, snapshot);
            }
        }
    }

    pub(super) fn observations(&self) -> Vec<(WorkbenchOperationId, WorkbenchDisplayPublication)> {
        self.state
            .lock()
            .operations
            .iter()
            .filter_map(|operation| {
                operation
                    .observation()
                    .map(|publication| (operation.id.clone(), publication))
            })
            .collect()
    }
}

impl crate::resident_tools::WorkbenchReceiptOwner for DisplayExecutionSettlement {
    fn freeze(&self, reply: &mut crate::KernelWorkbenchReply) {
        let receipts = match reply {
            Ok(response) => Some(&mut response.items),
            Err(error) => error.receipts_mut(),
        };
        self.freeze_receipts(receipts);
    }
}

fn merge_display_receipt(
    receipts: &mut Vec<WorkbenchItemReceipt>,
    snapshot: WorkbenchOperationReceipt,
) {
    let index = snapshot.id.input_unit_index;
    if !receipts.iter().any(|item| item.index == index) {
        receipts.push(WorkbenchItemReceipt {
            index,
            status: WorkbenchItemStatus::Stopped,
            kind: None,
            span: None,
            source_items: Vec::new(),
            output: String::new(),
            value: None,
            diagnostics: Vec::new(),
            warnings: Vec::new(),
            installed_bindings: Vec::new(),
            operations: Vec::new(),
            terminal_transfer: None,
            failure_layer: None,
        });
        receipts.sort_by_key(|item| item.index);
    }
    let receipt = receipts
        .iter_mut()
        .find(|item| item.index == index)
        .expect("original display input unit");
    let projected = receipt
        .operations
        .iter()
        .find(|operation| operation.id == snapshot.id)
        .is_some_and(|operation| operation.display.is_some());
    if !projected {
        if let Some(display) = &snapshot.display {
            if !display.text.is_empty() {
                if !receipt.output.is_empty() {
                    receipt.output.push('\n');
                }
                receipt.output.push_str(&display.text);
            }
        }
    }
    if let Some(operation) = receipt
        .operations
        .iter_mut()
        .find(|operation| operation.id == snapshot.id)
    {
        operation.disposition = snapshot.disposition;
        operation.display = snapshot.display;
        operation.display_publication = snapshot.display_publication;
    } else {
        receipt.operations.push(snapshot);
        receipt
            .operations
            .sort_by_key(|operation| operation.id.effect_ordinal);
    }
}

/// One packet's retained settlement; receipts and the request share this
/// bounded page without retaining execution state or Haskell roots.
pub(super) struct DisplayPacketSettlement {
    page: WorkbenchDisplayPage,
    pub(super) publication: Mutex<WorkbenchDisplayPublication>,
}

impl DisplayPacketSettlement {
    pub(super) fn new(
        request: &DisplayPublication,
        outcome: Option<&DisplayPublicationOutcome>,
    ) -> Self {
        let packet = Self {
            page: request.page.clone(),
            publication: Mutex::new(WorkbenchDisplayPublication::Pending {
                publication: WorkbenchDisplayPublicationIdentity {
                    display: request.page.identity,
                    page_ordinal: request.page_ordinal,
                },
            }),
        };
        if let Some(outcome) = outcome {
            packet.answer(outcome);
        }
        packet
    }

    pub(super) fn answer(&self, outcome: &DisplayPublicationOutcome) {
        let mut state = self.publication.lock();
        let publication = match &*state {
            WorkbenchDisplayPublication::Pending { publication }
            | WorkbenchDisplayPublication::Unconfirmed { publication, .. }
            | WorkbenchDisplayPublication::Published { publication, .. }
            | WorkbenchDisplayPublication::Refused { publication, .. } => *publication,
        };
        *state = match outcome {
            DisplayPublicationOutcome::Published(output) => {
                WorkbenchDisplayPublication::Published {
                    publication,
                    output: output.clone(),
                }
            }
            DisplayPublicationOutcome::Refused(detail) => WorkbenchDisplayPublication::Refused {
                publication,
                detail: detail.clone(),
            },
            DisplayPublicationOutcome::Unconfirmed(detail) => {
                WorkbenchDisplayPublication::Unconfirmed {
                    publication,
                    detail: detail.clone(),
                }
            }
        };
    }
}

pub(super) struct DisplayOperationSettlement {
    pub(super) id: WorkbenchOperationId,
    effect: String,
    success: WorkbenchOperationDisposition,
    state: Mutex<DisplayOperationState>,
}

#[derive(Default)]
struct DisplayOperationState {
    packet: Option<Arc<DisplayPacketSettlement>>,
    frozen: Option<WorkbenchOperationReceipt>,
}

impl DisplayOperationSettlement {
    pub(super) fn new(
        id: WorkbenchOperationId,
        effect: String,
        success: WorkbenchOperationDisposition,
    ) -> Self {
        Self {
            id,
            effect,
            success,
            state: Mutex::new(DisplayOperationState::default()),
        }
    }

    fn admit(&self, request: &DisplayPublication) {
        // Serialize initial attachment with answers, including an already
        // uncertain request retried by a later operation.
        let outcome = request.outcome.lock();
        let packet = request
            .receipt
            .get_or_init(|| Arc::new(DisplayPacketSettlement::new(request, outcome.as_ref())))
            .clone();
        drop(outcome);
        self.state.lock().packet = Some(packet);
    }

    pub(super) fn packet(&self) -> Option<Arc<DisplayPacketSettlement>> {
        self.state.lock().packet.clone()
    }

    pub(super) fn freeze(
        &self,
        publication: &WorkbenchDisplayPublication,
    ) -> Option<WorkbenchOperationReceipt> {
        let mut state = self.state.lock();
        if let Some(frozen) = &state.frozen {
            return Some(frozen.clone());
        }
        let packet = state.packet.as_ref()?.clone();
        let publication = publication.clone();
        let display = match &publication {
            WorkbenchDisplayPublication::Published { output, .. } => Some(WorkbenchDisplayOutput {
                page: packet.page.clone(),
                output: output.clone(),
            }),
            _ => None,
        };
        let receipt = WorkbenchOperationReceipt {
            disposition: match &publication {
                WorkbenchDisplayPublication::Published { .. } => self.success,
                WorkbenchDisplayPublication::Refused { .. } => {
                    WorkbenchOperationDisposition::Rejected
                }
                WorkbenchDisplayPublication::Pending { .. }
                | WorkbenchDisplayPublication::Unconfirmed { .. } => {
                    WorkbenchOperationDisposition::Unknown
                }
            },
            display_publication: Some(publication),
            display,
            id: self.id.clone(),
            effect: self.effect.clone(),
        };
        state.frozen = Some(receipt.clone());
        Some(receipt)
    }

    pub(super) fn observation(&self) -> Option<WorkbenchDisplayPublication> {
        let packet = self.packet()?;
        let publication = packet.publication.lock().clone();
        Some(publication)
    }
}

/// The original exclusive fragment charges its prepared page before enqueue.
/// Acknowledgment only updates the shared settlement, never this cursor.
pub(super) struct DisplayReceiptSubmission<'a> {
    pub(super) owner: Option<Arc<DisplayExecutionSettlement>>,
    pub(super) settlement: Option<Arc<DisplayOperationSettlement>>,
    pub(super) fragment: &'a mut ResidentWorkbenchFragment,
    pub(super) remaining: &'a mut usize,
}

impl DisplayReceiptSubmission<'_> {
    pub(super) fn stage_and_prepare(
        &mut self,
        stage: impl FnOnce() -> Result<DisplayPublicationSubmission, ResidentActorWorkbenchError>,
    ) -> Result<DisplayPublicationSubmission, ResidentActorWorkbenchError> {
        let owner = self.owner.clone();
        let admit = || {
            let submission = stage()?;
            self.prepare_unfenced(&submission.request)?;
            Ok(submission)
        };
        match owner {
            Some(owner) => owner.admit_publication(admit),
            None => admit(),
        }
    }

    pub(super) fn prepare(
        &mut self,
        request: &DisplayPublication,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let owner = self.owner.clone();
        match owner {
            Some(owner) => owner.admit_publication(|| self.prepare_unfenced(request)),
            None => self.prepare_unfenced(request),
        }
    }

    fn prepare_unfenced(
        &mut self,
        request: &DisplayPublication,
    ) -> Result<(), ResidentActorWorkbenchError> {
        if let Some(settlement) = &self.settlement {
            settlement.admit(request);
        }
        let text = self
            .fragment
            .present_output(&request.page.text, self.remaining);
        if text != request.page.text {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "display page exceeds its reserved output allowance".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frozen_reply(
        result: &Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>,
    ) -> crate::KernelWorkbenchReply {
        match result {
            Ok(KernelStep::Continue(response) | KernelStep::ContinueLater(response))
            | Ok(KernelStep::Stop {
                output: response, ..
            }) => Ok(response.clone()),
            Err(error) => Err(error.clone()),
        }
    }

    #[tokio::test]
    async fn publication_ack_and_terminal_freeze_preserve_original_receipt_ordering() {
        for (terminal_first, cleanup_failure) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let actor = ActorRef::first(crate::ActorId(37));
            let execution = WorkbenchExecutionId::from_digest([23; 16]);
            let id = WorkbenchOperationId {
                execution: execution.clone(),
                input_unit_index: 0,
                effect_ordinal: 0,
            };
            let request_input = WorkbenchRequest::from_cell_input("display value")
                .with_execution_id(execution.clone());
            let settlement = Arc::new(DisplayOperationSettlement::new(
                id.clone(),
                "Console.DisplayWith".into(),
                WorkbenchOperationDisposition::Committed,
            ));
            let mut journal = WorkbenchExecutions::default();
            journal.begin(&execution, request_input.clone(), None);
            journal
                .display_receipt_owner(&execution, None)
                .unwrap()
                .retain(settlement.clone())
                .unwrap();
            let displays = Arc::new(Mutex::new(ActorDisplays::default()));
            let page = WorkbenchDisplayPage {
                identity: (0, 0, 0),
                text: "visible".into(),
                expansions: Vec::new(),
                unavailable: false,
            };
            let (request, answer) = displays
                .lock()
                .stage(actor, page, None, false, 8192, Some(id.clone()))
                .unwrap();
            settlement.admit(&request);
            let submission =
                DisplayPublicationSubmission::new(displays.clone(), request.clone(), answer);
            let (sender, mut receiver) = mpsc::channel(1);
            assert!(sender
                .try_send(LocalResidentDeployment::DisplayPublished(request.clone()))
                .is_ok());
            let LocalResidentDeployment::DisplayPublished(host_request) =
                receiver.recv().await.unwrap()
            else {
                panic!("one issued packet");
            };
            assert!(Arc::ptr_eq(&host_request, &request));
            assert!(receiver.try_recv().is_err());
            let output = tidepool_runtime::session::ActorOutputReference {
                run: "run-1".into(),
                sequence: 7,
            };
            let cancelled_receipts = vec![WorkbenchItemReceipt {
                index: 0,
                status: WorkbenchItemStatus::Stopped,
                kind: None,
                span: None,
                source_items: Vec::new(),
                output: "prior output".into(),
                value: None,
                diagnostics: Vec::new(),
                warnings: Vec::new(),
                installed_bindings: Vec::new(),
                operations: vec![WorkbenchOperationReceipt {
                    id: id.clone(),
                    effect: "Console.DisplayWith".into(),
                    disposition: WorkbenchOperationDisposition::Unknown,
                    display: None,
                    display_publication: None,
                }],
                terminal_transfer: None,
                failure_layer: None,
            }];
            let response = WorkbenchResponse {
                status: WorkbenchRunStatus::RequestCancelled,
                summary: Some("cancelled".into()),
                items: cancelled_receipts,
                next_index: 0,
                total: 1,
                publication: None,
            };
            let mut result = if cleanup_failure {
                owned_workbench::retain_cleanup_failure(
                    actor,
                    "native cleanup unconfirmed".into(),
                    WorkbenchFinalizationResult {
                        result: Ok(KernelStep::Continue(response)),
                        cleanup_confirmed: true,
                    },
                )
                .result
            } else {
                Ok(KernelStep::Continue(response))
            };
            if !terminal_first {
                // The host answer is retained, but its waiter has never polled.
                assert!(request.answer(DisplayPublicationOutcome::Published(output.clone())));
            }
            journal.freeze_display_receipts(&execution, None, &mut result);
            let frozen = frozen_reply(&result);
            let receipts = match &frozen {
                Ok(response) => {
                    assert!(!cleanup_failure);
                    assert_eq!(response.status, WorkbenchRunStatus::RequestCancelled);
                    &response.items
                }
                Err(error) => {
                    assert!(cleanup_failure);
                    assert!(matches!(
                        error,
                        KernelInvocationFailure::CleanupUnconfirmed { .. }
                    ));
                    error.receipts()
                }
            };
            let operation = &receipts[0].operations[0];
            if terminal_first {
                assert!(operation.display.is_none());
                assert_eq!(receipts[0].output, "prior output");
                assert!(
                    matches!(operation.display_publication, Some(WorkbenchDisplayPublication::Pending { publication }) if publication.display == request.page.identity && publication.page_ordinal == 1)
                );
            } else {
                assert_eq!(
                    operation.disposition,
                    WorkbenchOperationDisposition::Committed
                );
                assert_eq!(operation.display.as_ref().unwrap().output, output);
                assert_eq!(receipts[0].output, "prior output\nvisible");
            }
            journal.record(
                execution.clone(),
                request_input.clone(),
                frozen.clone(),
                crate::WorkbenchCancellationOutcome::NotSleeping {
                    execution: execution.clone(),
                },
                None,
            );
            // Drop the outer execution before consuming the acknowledgment.
            drop(submission);
            if terminal_first {
                assert_eq!(displays.lock().pending_count(), 1);
                assert!(request.answer(DisplayPublicationOutcome::Published(output.clone())));
                displays.lock().reconcile(request.page.identity.2).unwrap();
            }
            assert_eq!(displays.lock().pending_count(), 0);
            assert_eq!(
                displays.lock().slots[&request.page.identity.2].page_ordinal,
                1
            );
            assert!(!request.answer(DisplayPublicationOutcome::Published(output.clone())));
            assert!(
                matches!(journal.display_observations()[0].1, WorkbenchDisplayPublication::Published { ref output, .. } if output.sequence == 7)
            );
            assert_eq!(
                journal
                    .lookup(&execution, &request_input, None)
                    .unwrap()
                    .unwrap(),
                frozen.clone()
            );
            journal.freeze_display_receipts(&execution, None, &mut result);
            assert_eq!(
                frozen_reply(&result),
                frozen,
                "freeze never rewrites delivered JSON or appends visible text twice"
            );
        }
    }

    #[test]
    fn scheduler_terminal_freezes_only_the_exact_hosted_receipt_owner() {
        for (terminal_first, transport_exit) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let actor = ActorRef::first(crate::ActorId(37));
            let execution = WorkbenchExecutionId::from_digest([23; 16]);
            let key = |call: &str| {
                crate::resident_tools::WorkbenchCallKey::from(
                    exomonad_tool::ToolInvocationContext::external(
                        "thread".into(),
                        "turn".into(),
                        call.into(),
                        Some("outer".into()),
                        None,
                    ),
                )
            };
            let first = key("first");
            let second = key("second");
            let input = WorkbenchRequest::from_cell_input("display value")
                .with_execution_id(execution.clone());
            let mut journal = WorkbenchExecutions::default();
            journal.begin(&execution, input.clone(), Some(&first));
            journal.begin(&execution, input, Some(&second));
            let owner = journal
                .display_receipt_owner(&execution, Some(&first))
                .unwrap();
            let other = journal
                .display_receipt_owner(&execution, Some(&second))
                .unwrap();
            assert!(!Arc::ptr_eq(&owner, &other));
            let operation = WorkbenchOperationId {
                execution: execution.clone(),
                input_unit_index: 0,
                effect_ordinal: 0,
            };
            let settlement = Arc::new(DisplayOperationSettlement::new(
                operation.clone(),
                "Console.DisplayWith".into(),
                WorkbenchOperationDisposition::Committed,
            ));
            owner.retain(settlement.clone()).unwrap();
            let control = crate::WorkbenchExecutionControl::untracked();
            control.bind_receipt_owner(owner.clone());
            let displays = Arc::new(Mutex::new(ActorDisplays::default()));
            let (request, answer) = displays
                .lock()
                .stage(
                    actor,
                    WorkbenchDisplayPage {
                        identity: (0, 0, 0),
                        text: "visible".into(),
                        expansions: Vec::new(),
                        unavailable: false,
                    },
                    None,
                    false,
                    8192,
                    Some(operation),
                )
                .unwrap();
            settlement.admit(&request);
            let submission =
                DisplayPublicationSubmission::new(displays.clone(), request.clone(), answer);
            let output = tidepool_runtime::session::ActorOutputReference {
                run: "run-1".into(),
                sequence: 7,
            };
            if !terminal_first {
                request.answer(DisplayPublicationOutcome::Published(output.clone()));
            }
            let fail = || {
                if transport_exit {
                    KernelInvocationFailure::ActorExited(actor)
                } else {
                    KernelInvocationFailure::Failed {
                        actor,
                        detail: "scheduler worker stopped".into(),
                        receipts: Vec::new(),
                        diagnostic: None,
                    }
                }
            };
            let mut untouched = Err(fail());
            journal.freeze_display_receipts(&execution, Some(&second), &mut untouched);
            assert!(frozen_reply(&untouched).unwrap_err().receipts().is_empty());
            assert!(other.observations().is_empty());
            // Retained control carries only this metadata owner even when
            // forest/journal custody and the discarded execution are gone.
            drop(journal);
            drop(owner);
            let delivered = control.settle_reply(Err(fail()));
            assert!(matches!(
                &delivered,
                Err(KernelInvocationFailure::Failed { .. })
            ));
            assert_eq!(control.terminal_reply(), Some(delivered.clone()));
            let receipt = &delivered.as_ref().unwrap_err().receipts()[0];
            if terminal_first {
                assert!(receipt.operations[0].display.is_none());
                assert!(
                    matches!(receipt.operations[0].display_publication, Some(WorkbenchDisplayPublication::Pending { publication }) if publication.display == request.page.identity && publication.page_ordinal == 1)
                );
                assert!(receipt.output.is_empty());
            } else {
                assert_eq!(
                    receipt.operations[0].disposition,
                    WorkbenchOperationDisposition::Committed
                );
                assert_eq!(
                    receipt.operations[0].display.as_ref().unwrap().output,
                    output
                );
                assert_eq!(receipt.output, "visible");
            }
            drop(submission);
            if terminal_first {
                request.answer(DisplayPublicationOutcome::Published(output));
                displays.lock().reconcile(request.page.identity.2).unwrap();
            }
            assert_eq!(displays.lock().pending_count(), 0);
            assert_eq!(
                control.settle_reply(Err(fail())),
                delivered,
                "late commit never rewrites the first delivered scheduler outcome"
            );
            assert!(matches!(
                settlement.observation(),
                Some(WorkbenchDisplayPublication::Published { .. })
            ));
        }
    }

    #[test]
    fn terminal_control_seals_admission_before_first_display_capture() {
        for terminal_before_binding in [false, true] {
            let actor = ActorRef::first(crate::ActorId(37));
            let execution = WorkbenchExecutionId::from_digest([25; 16]);
            let input = WorkbenchRequest::from_cell_input("display value")
                .with_execution_id(execution.clone());
            let mut journal = WorkbenchExecutions::default();
            journal.begin(&execution, input, None);
            let owner = journal.display_receipt_owner(&execution, None).unwrap();
            let control = crate::WorkbenchExecutionControl::untracked();
            let failed = || Err(KernelInvocationFailure::ActorExited(actor));
            let delivered = if terminal_before_binding {
                let delivered = control.settle_reply(failed());
                control.bind_receipt_owner(owner.clone());
                delivered
            } else {
                control.bind_receipt_owner(owner.clone());
                control.settle_reply(failed())
            };
            assert_eq!(delivered, Err(KernelInvocationFailure::ActorExited(actor)));
            let operation = WorkbenchOperationId {
                execution,
                input_unit_index: 0,
                effect_ordinal: 0,
            };
            let late_capture = Arc::new(DisplayOperationSettlement::new(
                operation.clone(),
                "Console.DisplayWith".into(),
                WorkbenchOperationDisposition::Committed,
            ));
            assert!(owner.retain(late_capture).is_err());
            let displays = Arc::new(Mutex::new(ActorDisplays::default()));
            assert!(owner
                .admit_publication(|| {
                    let (request, answer) = displays.lock().stage(
                        actor,
                        WorkbenchDisplayPage {
                            identity: (0, 0, 0),
                            text: "visible".into(),
                            expansions: Vec::new(),
                            unavailable: false,
                        },
                        None,
                        false,
                        8192,
                        Some(operation),
                    )?;
                    Ok(DisplayPublicationSubmission::new(
                        displays.clone(),
                        request,
                        answer,
                    ))
                })
                .is_err());
            assert!(displays.lock().slots.is_empty());
            assert_eq!(displays.lock().next_slot, 0);
            assert_eq!(
                control.terminal_reply(),
                Some(delivered),
                "late owner binding never changes a delivered terminal reply"
            );
        }
        let actor = ActorRef::first(crate::ActorId(37));
        let preadmission = crate::WorkbenchExecutionControl::untracked();
        assert_eq!(
            preadmission.settle_reply(Err(KernelInvocationFailure::ActorExited(actor))),
            Err(KernelInvocationFailure::ActorExited(actor))
        );
    }

    #[test]
    fn native_stage_and_terminal_freeze_share_one_publication_cutoff() {
        let actor = ActorRef::first(crate::ActorId(37));
        let execution = WorkbenchExecutionId::from_digest([23; 16]);
        let operation = WorkbenchOperationId {
            execution: execution.clone(),
            input_unit_index: 0,
            effect_ordinal: 0,
        };
        let owner = Arc::new(DisplayExecutionSettlement::new(execution));
        let settlement = Arc::new(DisplayOperationSettlement::new(
            operation.clone(),
            "Console.DisplayWith".into(),
            WorkbenchOperationDisposition::Committed,
        ));
        owner.retain(settlement.clone()).unwrap();
        let control = crate::WorkbenchExecutionControl::untracked();
        control.bind_receipt_owner(owner.clone());
        let displays = Arc::new(Mutex::new(ActorDisplays::default()));
        let (issued, issued_request) = std::sync::mpsc::channel();
        let (release_stage, stage_release) = std::sync::mpsc::channel();
        let stage = std::thread::spawn({
            let owner = owner.clone();
            let settlement = settlement.clone();
            let displays = displays.clone();
            move || {
                owner.admit_publication(|| {
                    let (request, answer) = displays.lock().stage(
                        actor,
                        WorkbenchDisplayPage {
                            identity: (0, 0, 0),
                            text: "visible".into(),
                            expansions: Vec::new(),
                            unavailable: false,
                        },
                        None,
                        false,
                        8192,
                        Some(operation),
                    )?;
                    let submission =
                        DisplayPublicationSubmission::new(displays, request.clone(), answer);
                    issued.send(request.clone()).unwrap();
                    // Freeze attempts its cutoff after native slot issuance and
                    // before packet attachment, with no host submission yet.
                    stage_release.recv().unwrap();
                    settlement.admit(&request);
                    Ok(submission)
                })
            }
        });
        let request = issued_request.recv().unwrap();
        let (started, freeze_started) = std::sync::mpsc::channel();
        let (frozen, freeze_done) = std::sync::mpsc::channel();
        let freeze = std::thread::spawn({
            let control = control.clone();
            move || {
                started.send(()).unwrap();
                let reply = control.settle_reply(Err(KernelInvocationFailure::Failed {
                    actor,
                    detail: "scheduler stopped execution".into(),
                    receipts: Vec::new(),
                    diagnostic: None,
                }));
                frozen.send(reply).unwrap();
            }
        });
        freeze_started.recv().unwrap();
        assert!(matches!(
            freeze_done.recv_timeout(std::time::Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        release_stage.send(()).unwrap();
        let submission = stage.join().unwrap().unwrap();
        let delivered = freeze_done.recv().unwrap();
        freeze.join().unwrap();
        let operation = &delivered.as_ref().unwrap_err().receipts()[0].operations[0];
        assert!(
            matches!(operation.display_publication, Some(WorkbenchDisplayPublication::Pending { publication }) if publication.display == request.page.identity && publication.page_ordinal == 1)
        );
        assert!(operation.display.is_none());
        drop(submission);
        request.answer(DisplayPublicationOutcome::Published(
            tidepool_runtime::session::ActorOutputReference {
                run: "run-1".into(),
                sequence: 7,
            },
        ));
        displays.lock().reconcile(request.page.identity.2).unwrap();
        assert_eq!(control.terminal_reply(), Some(delivered));
        assert!(matches!(
            settlement.observation(),
            Some(WorkbenchDisplayPublication::Published { .. })
        ));
        let attempts = std::sync::atomic::AtomicUsize::new(0);
        let rejected: Result<(), ResidentActorWorkbenchError> = owner.admit_publication(|| {
            attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        });
        assert!(rejected.is_err());
        assert_eq!(
            attempts.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "a frozen execution cannot issue a later publication"
        );
        assert!(owner
            .retain(Arc::new(DisplayOperationSettlement::new(
                WorkbenchOperationId {
                    execution: owner.execution.clone(),
                    input_unit_index: 0,
                    effect_ordinal: 1
                },
                "Console.DisplayWith".into(),
                WorkbenchOperationDisposition::Committed
            )))
            .is_err());
    }

    #[test]
    fn terminal_freeze_before_issuance_rejects_uncaptured_packet() {
        let actor = ActorRef::first(crate::ActorId(37));
        let execution = WorkbenchExecutionId::from_digest([24; 16]);
        let operation = WorkbenchOperationId {
            execution: execution.clone(),
            input_unit_index: 0,
            effect_ordinal: 0,
        };
        let owner = Arc::new(DisplayExecutionSettlement::new(execution));
        let settlement = Arc::new(DisplayOperationSettlement::new(
            operation.clone(),
            "Console.DisplayWith".into(),
            WorkbenchOperationDisposition::Committed,
        ));
        owner.retain(settlement).unwrap();
        let control = crate::WorkbenchExecutionControl::untracked();
        control.bind_receipt_owner(owner.clone());
        let delivered = control.settle_reply(Err(KernelInvocationFailure::Failed {
            actor,
            detail: "scheduler stopped before issuance".into(),
            receipts: Vec::new(),
            diagnostic: None,
        }));
        assert!(delivered.as_ref().unwrap_err().receipts().is_empty());
        let displays = Arc::new(Mutex::new(ActorDisplays::default()));
        let stage = owner.admit_publication(|| {
            let (request, answer) = displays.lock().stage(
                actor,
                WorkbenchDisplayPage {
                    identity: (0, 0, 0),
                    text: "visible".into(),
                    expansions: Vec::new(),
                    unavailable: false,
                },
                None,
                false,
                8192,
                Some(operation),
            )?;
            Ok(DisplayPublicationSubmission::new(
                displays.clone(),
                request,
                answer,
            ))
        });
        assert!(stage.is_err());
        assert!(displays.lock().slots.is_empty());
        assert_eq!(displays.lock().next_slot, 0);
        assert_eq!(control.terminal_reply(), Some(delivered));
    }

    #[test]
    fn definitive_host_refusal_freezes_rejected_operation_without_output() {
        let actor = ActorRef::first(crate::ActorId(37));
        let execution = WorkbenchExecutionId::from_digest([23; 16]);
        let operation = WorkbenchOperationId {
            execution: execution.clone(),
            input_unit_index: 0,
            effect_ordinal: 0,
        };
        let owner = Arc::new(DisplayExecutionSettlement::new(execution));
        let settlement = Arc::new(DisplayOperationSettlement::new(
            operation.clone(),
            "Console.DisplayWith".into(),
            WorkbenchOperationDisposition::Committed,
        ));
        owner.retain(settlement.clone()).unwrap();
        let control = crate::WorkbenchExecutionControl::untracked();
        control.bind_receipt_owner(owner);
        let displays = Arc::new(Mutex::new(ActorDisplays::default()));
        let (request, answer) = displays
            .lock()
            .stage(
                actor,
                WorkbenchDisplayPage {
                    identity: (0, 0, 0),
                    text: "visible".into(),
                    expansions: Vec::new(),
                    unavailable: false,
                },
                None,
                false,
                8192,
                Some(operation),
            )
            .unwrap();
        settlement.admit(&request);
        let submission =
            DisplayPublicationSubmission::new(displays.clone(), request.clone(), answer);
        assert!(request.answer(DisplayPublicationOutcome::Refused(
            "authority rejected before commit".into()
        )));
        let delivered = control.settle_reply(Err(KernelInvocationFailure::Rejected {
            actor,
            detail: "display refused".into(),
            receipts: Vec::new(),
            diagnostic: None,
        }));
        let receipt = &delivered.as_ref().unwrap_err().receipts()[0];
        assert_eq!(
            receipt.operations[0].disposition,
            WorkbenchOperationDisposition::Rejected
        );
        assert!(receipt.operations[0].display.is_none());
        assert!(receipt.output.is_empty());
        assert!(matches!(
            receipt.operations[0].display_publication,
            Some(WorkbenchDisplayPublication::Refused { .. })
        ));
        drop(submission);
        assert_eq!(displays.lock().pending_count(), 0);
        assert!(!request.was_unconfirmed());
    }

    #[test]
    fn publication_owner_bounds_uncertain_detail_and_rejects_invalid_reference() {
        let actor = ActorRef::first(crate::ActorId(37));
        let page = WorkbenchDisplayPage {
            identity: (37, 1, 1),
            text: String::new(),
            expansions: Vec::new(),
            unavailable: false,
        };
        let (request, answer) = DisplayPublication::channel(actor, 1, None, page);
        drop(answer);
        request.answer(DisplayPublicationOutcome::Unconfirmed("🙂".repeat(4096)));
        let Some(DisplayPublicationOutcome::Unconfirmed(detail)) = request.outcome() else {
            panic!("retained uncertainty");
        };
        assert!(detail.len() <= 2048);
        assert!(request.retry_channel().is_some());
        assert!(request.answer(DisplayPublicationOutcome::Published(
            tidepool_runtime::session::ActorOutputReference {
                run: String::new(),
                sequence: 0
            }
        )));
        assert!(matches!(
            request.outcome(),
            Some(DisplayPublicationOutcome::Unconfirmed(_))
        ));
        assert!(request.was_unconfirmed());
        for (run, accepted) in [
            ("🙂".repeat(256), true),
            ("🙂".repeat(257), false),
            ("\n".repeat(1024), true),
            ("\n".repeat(1025), false),
        ] {
            let (request, _answer) =
                DisplayPublication::channel(actor, 1, None, request.page.clone());
            request.answer(DisplayPublicationOutcome::Published(
                tidepool_runtime::session::ActorOutputReference { run, sequence: 7 },
            ));
            assert_eq!(
                matches!(
                    request.outcome(),
                    Some(DisplayPublicationOutcome::Published(_))
                ),
                accepted
            );
        }
    }
}
