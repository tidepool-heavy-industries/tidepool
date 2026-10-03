use super::*;
use tidepool_runtime::session::{WorkbenchDisplayPublication, WorkbenchDisplayPublicationIdentity};

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
            display_publication: Some(publication),
            disposition: if display.is_some() {
                self.success
            } else {
                WorkbenchOperationDisposition::Unknown
            },
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
    pub(super) settlement: Option<Arc<DisplayOperationSettlement>>,
    pub(super) fragment: &'a mut ResidentWorkbenchFragment,
    pub(super) remaining: &'a mut usize,
}

impl DisplayReceiptSubmission<'_> {
    pub(super) fn prepare(
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
                .retain_display_settlement(settlement.clone())
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
            journal.freeze_display_receipts(&execution, &mut result);
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
            journal.freeze_display_receipts(&execution, &mut result);
            assert_eq!(
                frozen_reply(&result),
                frozen,
                "freeze never rewrites delivered JSON or appends visible text twice"
            );
        }
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
    }
}
