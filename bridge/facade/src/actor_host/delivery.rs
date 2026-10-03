use super::*;

pub(super) fn schedule_embedded_notification_send(
    command: Arc<exomonad_actor::NotificationSend>,
    binding: &embedded_harness::EmbeddedActorBinding,
    notifications: &mut JoinSet<(ActorRef, Result<(), String>)>,
) {
    let target = command.target();
    let binding = binding.clone();
    notifications.spawn(async move {
        let result = async {
            let inbox = Arc::clone(&binding.inbox);
            let event = DurableActorEvent::Text(command.message().to_owned());
            let context = DeliveryProvenance::Notification {
                sender: command.owner(),
                target,
            };
            let published = tidepool_runtime::spawn_blocking_in_span(move || {
                inbox.publish_tracked(event, context)
            })
            .await
            .map_err(|error| error.to_string())?;
            match published {
                Ok(row) => command.admitted(binding.inbox_key.clone(), row.sequence),
                Err(error @ exomonad_node::InboxError::UncertainWrite { .. }) => {
                    command.rejected(exomonad_actor::NotificationError::Unconfirmed(
                        error.to_string(),
                    ));
                    return Err(error.to_string());
                }
                Err(error) => {
                    command.rejected(exomonad_actor::NotificationError::StorageFailure(
                        error.to_string(),
                    ));
                    return Err(error.to_string());
                }
            }
            deliver_embedded_notifications(target, binding).await
        }
        .await;
        (target, result)
    });
}

pub(super) fn schedule_embedded_notification_drain(
    target: ActorRef,
    binding: embedded_harness::EmbeddedActorBinding,
    notifications: &mut JoinSet<(ActorRef, Result<(), String>)>,
) {
    if binding.input_observer().is_none() {
        return;
    }
    let pending = match binding.inbox.front_pending() {
        Ok(Some(pending)) => pending,
        Ok(None) => return,
        Err(_) => return,
    };
    if !binding.is_live() {
        let receipt = match binding.inbox.observe_receipt(pending.sequence) {
            Ok(exomonad_node::ReceiptLookup::Retained(receipt)) => receipt,
            Ok(exomonad_node::ReceiptLookup::Unavailable) | Err(_) => return,
        };
        if !matches!(
            receipt.phase,
            exomonad_node::DeliveryPhase::InFlight
                | exomonad_node::DeliveryPhase::Submitted
                | exomonad_node::DeliveryPhase::Unconfirmed
        ) {
            return;
        }
    }
    let delivery = Arc::clone(&binding.delivery);
    let Ok(guard) = delivery.try_lock_owned() else {
        return;
    };
    notifications.spawn(async move {
        let result = deliver_embedded_notifications_locked(target, binding, guard).await;
        (target, result)
    });
}

pub(super) fn embedded_notification_operation_id(
    inbox_key: &str,
    sequence: u64,
    sender: ActorRef,
    target: ActorRef,
) -> String {
    format!(
        "notification:{inbox_key}:{}:{}:{}:{}:{sequence}",
        sender.id.0, sender.incarnation.0, target.id.0, target.incarnation.0,
    )
}

pub(super) async fn deliver_embedded_notifications(
    target: ActorRef,
    binding: embedded_harness::EmbeddedActorBinding,
) -> Result<(), String> {
    let delivery = Arc::clone(&binding.delivery);
    let Ok(guard) = delivery.try_lock_owned() else {
        return Ok(());
    };
    deliver_embedded_notifications_locked(target, binding, guard).await
}

async fn deliver_embedded_notifications_locked(
    target: ActorRef,
    binding: embedded_harness::EmbeddedActorBinding,
    _guard: tokio::sync::OwnedMutexGuard<()>,
) -> Result<(), String> {
    use exomonad_node::{DeliveryPhase, ReceiptLookup};

    let Some(observer) = binding.input_observer() else {
        return Ok(());
    };
    loop {
        let Some(envelope) = binding
            .inbox
            .front_pending()
            .map_err(|error| error.to_string())?
        else {
            return Ok(());
        };
        let Some(context) = envelope.receipt_context else {
            return Err("embedded notification inbox contains an untracked row".into());
        };
        #[cfg(feature = "codex-compat")]
        let (sender, row_target) = match context {
            DeliveryProvenance::Notification { sender, target } => (sender, target),
            _ => {
                return Err("embedded notification inbox contains another delivery kind".into());
            }
        };
        #[cfg(not(feature = "codex-compat"))]
        let DeliveryProvenance::Notification {
            sender,
            target: row_target,
        } = context;
        if row_target != target {
            return Err("embedded notification row targets another incarnation".into());
        }
        let evidence = match binding
            .inbox
            .observe_receipt(envelope.sequence)
            .map_err(|error| error.to_string())?
        {
            ReceiptLookup::Retained(evidence) => evidence,
            ReceiptLookup::Unavailable => {
                return Err("embedded notification receipt is no longer retained".into());
            }
        };
        if evidence.context
            != (DeliveryProvenance::Notification {
                sender,
                target: row_target,
            })
        {
            return Err("embedded notification receipt provenance changed".into());
        }
        if matches!(
            evidence.phase,
            DeliveryPhase::Presented | DeliveryPhase::Withdrawn | DeliveryPhase::Rejected
        ) {
            continue;
        }
        if evidence.phase == DeliveryPhase::Compacted {
            return Ok(());
        }

        let operation_id = embedded_notification_operation_id(
            &binding.inbox_key,
            envelope.sequence,
            sender,
            target,
        );
        if matches!(
            observer
                .input_observation_by_operation(&operation_id)
                .map_err(|error| error.to_string())?,
            Some(harness::embedding::InputObservation::Included(_))
        ) {
            if evidence.phase == DeliveryPhase::Accepted {
                binding
                    .inbox
                    .begin_tracked_delivery(envelope.sequence)
                    .map_err(|error| error.to_string())?
                    .submitted()
                    .map_err(|error| error.to_string())?;
            } else if evidence.phase == DeliveryPhase::InFlight {
                binding
                    .inbox
                    .confirm_admitted(
                        envelope.sequence,
                        &DeliveryProvenance::Notification { sender, target },
                    )
                    .map_err(|error| error.to_string())?;
            }
            binding
                .inbox
                .confirm_presented_exact(
                    envelope.sequence,
                    &DeliveryProvenance::Notification { sender, target },
                )
                .map_err(|error| error.to_string())?;
            continue;
        }
        if !binding.is_live() {
            return Ok(());
        }
        let Some(conversation) = binding.conversation() else {
            return Ok(());
        };
        let attempt = if evidence.phase == DeliveryPhase::Accepted {
            Some(
                binding
                    .inbox
                    .begin_tracked_delivery(envelope.sequence)
                    .map_err(|error| error.to_string())?,
            )
        } else if matches!(
            evidence.phase,
            DeliveryPhase::InFlight | DeliveryPhase::Submitted | DeliveryPhase::Unconfirmed
        ) {
            None
        } else {
            return Err(format!(
                "embedded notification delivery is still {:?}",
                evidence.phase
            ));
        };
        #[cfg(feature = "codex-compat")]
        let message = match envelope.payload {
            DurableActorEvent::Text(message) => message,
            _ => return Err("embedded notification row has a non-text payload".into()),
        };
        #[cfg(not(feature = "codex-compat"))]
        let DurableActorEvent::Text(message) = envelope.payload;
        let sender_label = format!("actor:{}:{}", sender.id.0, sender.incarnation.0);
        let input = conversation
            .input(&operation_id, &sender_label, &message)
            .await
            .map_err(|error| error.to_string())?;
        if let Some(error) = input.wake_error {
            tracing::warn!(?target, sequence = envelope.sequence, %error,
                "embedded notification is durable but its Engine wake failed");
        }
        if let Some(attempt) = attempt {
            attempt.submitted().map_err(|error| error.to_string())?;
        } else {
            binding
                .inbox
                .confirm_admitted(
                    envelope.sequence,
                    &DeliveryProvenance::Notification { sender, target },
                )
                .map_err(|error| error.to_string())?;
        }
        if matches!(
            observer
                .input_observation_by_operation(&operation_id)
                .map_err(|error| error.to_string())?,
            Some(harness::embedding::InputObservation::Included(_))
        ) {
            binding
                .inbox
                .confirm_presented_exact(
                    envelope.sequence,
                    &DeliveryProvenance::Notification { sender, target },
                )
                .map_err(|error| error.to_string())?;
            continue;
        }
        return Ok(());
    }
}

pub(super) async fn observe_embedded_notification(
    command: &exomonad_actor::NotificationPoll,
    binding: &embedded_harness::EmbeddedActorBinding,
    target: ActorRef,
) -> Result<exomonad_actor::NotificationState, exomonad_actor::NotificationError> {
    use exomonad_actor::{NotificationError, NotificationState};
    use exomonad_node::{DeliveryPhase, ReceiptLookup};
    let receipt = command.receipt();
    if receipt.owner() != command.owner() {
        return Err(NotificationError::Unauthorized);
    }
    if receipt.target() != target || receipt.inbox() != binding.inbox_key.as_str() {
        return Err(NotificationError::InvalidReceipt);
    }
    if binding.input_observer().is_some() {
        deliver_embedded_notifications(target, binding.clone())
            .await
            .map_err(NotificationError::StorageFailure)?;
    }
    match binding
        .inbox
        .observe_receipt(receipt.sequence())
        .map_err(|error| NotificationError::StorageFailure(error.to_string()))?
    {
        ReceiptLookup::Unavailable => Err(NotificationError::Unavailable),
        ReceiptLookup::Retained(evidence) => {
            if evidence.context
                != (DeliveryProvenance::Notification {
                    sender: command.owner(),
                    target,
                })
            {
                return Err(NotificationError::Unauthorized);
            }
            Ok(match evidence.phase {
                DeliveryPhase::Accepted => NotificationState::Accepted,
                DeliveryPhase::Presented => NotificationState::Presented,
                DeliveryPhase::InFlight
                | DeliveryPhase::Submitted
                | DeliveryPhase::Withdrawn
                | DeliveryPhase::Rejected
                | DeliveryPhase::Unconfirmed
                | DeliveryPhase::Compacted => NotificationState::Unconfirmed,
            })
        }
    }
}

#[cfg(feature = "codex-compat")]
pub(super) fn admit_notification(
    command: &exomonad_actor::NotificationSend,
    key: String,
    inbox: &ActorInbox,
) {
    match inbox.publish_tracked(
        DurableActorEvent::Text(command.message().to_owned()),
        DeliveryProvenance::Notification {
            sender: command.owner(),
            target: command.target(),
        },
    ) {
        Ok(row) => command.admitted(key, row.sequence),
        Err(error @ exomonad_node::InboxError::UncertainWrite { .. }) => {
            command.rejected(exomonad_actor::NotificationError::Unconfirmed(
                error.to_string(),
            ));
        }
        Err(error) => command.rejected(exomonad_actor::NotificationError::StorageFailure(
            error.to_string(),
        )),
    }
}

#[cfg(feature = "codex-compat")]
pub(super) fn observe_notification_receipt(
    command: &exomonad_actor::NotificationPoll,
    target: ActorRef,
    inbox_key: &str,
    inbox: &ActorInbox,
) -> Result<exomonad_actor::NotificationState, exomonad_actor::NotificationError> {
    use exomonad_actor::{NotificationError, NotificationState};
    use exomonad_node::{DeliveryPhase, ReceiptLookup};
    let receipt = command.receipt();
    if receipt.owner() != command.owner() {
        return Err(NotificationError::Unauthorized);
    }
    if receipt.target() != target || receipt.inbox() != inbox_key {
        return Err(NotificationError::InvalidReceipt);
    }
    match inbox
        .observe_receipt(receipt.sequence())
        .map_err(|error| NotificationError::StorageFailure(error.to_string()))?
    {
        ReceiptLookup::Unavailable => Err(NotificationError::Unavailable),
        ReceiptLookup::Retained(evidence) => {
            if evidence.context
                != (DeliveryProvenance::Notification {
                    sender: command.owner(),
                    target,
                })
            {
                return Err(NotificationError::Unauthorized);
            }
            Ok(match evidence.phase {
                DeliveryPhase::Accepted => NotificationState::Accepted,
                DeliveryPhase::Presented => NotificationState::Presented,
                DeliveryPhase::InFlight
                | DeliveryPhase::Submitted
                | DeliveryPhase::Withdrawn
                | DeliveryPhase::Rejected
                | DeliveryPhase::Unconfirmed
                | DeliveryPhase::Compacted => NotificationState::Unconfirmed,
            })
        }
    }
}

#[cfg(all(test, feature = "codex-compat"))]
#[allow(clippy::too_many_arguments)]
pub(super) async fn deliver_pending(
    actor: ActorRef,
    inbox: &Arc<ActorInbox>,
    thread: &QueueReadyThread,
    backend: &dyn InteractiveAgentBackend,
    producer: &InputProducerId,
    reconciliations: &Mutex<BTreeMap<String, PendingUpdateReconciliation>>,
    workspace: &Path,
    runtime_observation: &exomonad_actor::ActorRuntimeObservationHandle,
) -> Result<(), String> {
    deliver_pending_checked(
        actor,
        inbox,
        thread,
        backend,
        producer,
        reconciliations,
        workspace,
        runtime_observation,
        &|_, _| true,
        &|_, _, _| false,
        &|| false,
        &Mutex::new(BTreeMap::new()),
    )
    .await
}

/// A tracked row whose native input control keeps answering without
/// evidence (no record, or an unknown dispatch outcome) for this long is
/// withdrawn and, when the withdrawal settles it, re-delivered. Twice the
/// exchange deadline: a submit that timed out may still be admitted late
/// within one more exchange.
#[cfg(feature = "codex-compat")]
pub(super) const WITHDRAW_WITHOUT_EVIDENCE_AFTER: Duration =
    exomonad_agent::INPUT_CONTROL_DEADLINE.saturating_mul(2);

/// Cadence at which an unchanged pending-delivery WARN is repeated.
#[cfg(feature = "codex-compat")]
const PENDING_DELIVERY_WARN_INTERVAL: Duration = Duration::from_secs(30);

/// Leading line on a re-delivered message whose earlier copy native input
/// control admitted without proving whether the model saw it.
#[cfg(feature = "codex-compat")]
pub(super) const POSSIBLY_SEEN_PREFIX: &str = "(possibly already seen)";

/// Leading line on every re-delivered message: it was queued again behind
/// the messages that were waiting for it.
#[cfg(feature = "codex-compat")]
pub(super) const REDELIVERED_PREFIX: &str =
    "(re-delivered: messages sent after it may have arrived first)";

/// Pump memory for a tracked row whose native input control answers without
/// evidence (no record, or an unknown dispatch outcome).
#[derive(Debug, Clone, Copy)]
#[cfg(feature = "codex-compat")]
pub(super) enum WithoutEvidence {
    /// First answered without evidence at this instant.
    Since(std::time::Instant),
    /// Withdrawal itself had no evidence to act on (native input control is
    /// unavailable); the row stays unconfirmed and is not withdrawn again.
    WithdrawUnavailable,
}

#[allow(clippy::too_many_arguments)]
#[cfg(feature = "codex-compat")]
pub(super) async fn deliver_pending_checked(
    actor: ActorRef,
    inbox: &Arc<ActorInbox>,
    thread: &QueueReadyThread,
    backend: &dyn InteractiveAgentBackend,
    producer: &InputProducerId,
    reconciliations: &Mutex<BTreeMap<String, PendingUpdateReconciliation>>,
    workspace: &Path,
    runtime_observation: &exomonad_actor::ActorRuntimeObservationHandle,
    watch_retained: &(dyn Fn(ActorRef, exomonad_actor::WatchId) -> bool + Send + Sync),
    watch_observed_since: &(dyn Fn(ActorRef, exomonad_actor::WatchId, u64) -> bool + Send + Sync),
    hosted_cell_computing: &(dyn Fn() -> bool + Send + Sync),
    without_evidence: &Mutex<BTreeMap<String, WithoutEvidence>>,
) -> Result<(), String> {
    let cwd = workspace.to_string_lossy();
    let pending_inbox = Arc::clone(inbox);
    let pending =
        tidepool_runtime::spawn_blocking_in_span(move || pending_inbox.legacy_pending_prefix())
            .await
            .map_err(|error| format!("inbox reader task: {error}"))?
            .map_err(|error| error.to_string())?;
    let Some(last) = pending.last() else {
        // The front row is tracked (a native request update/notification).
        // `deliver_tracked_message` below only ever advances that exact
        // sequence, one at a time; if it is stuck in flight (`Submitted` or
        // `Unconfirmed`), any settlement/watch notice queued behind it would
        // otherwise never reach the model, so those surface out of band. Any
        // other phase, a rejection included, stays a hard barrier.
        if front_tracked_row_is_in_flight(inbox) {
            deliver_out_of_order_notices(
                actor,
                inbox,
                thread,
                backend,
                &cwd,
                runtime_observation,
                watch_retained,
                watch_observed_since,
            )
            .await?;
        }
        return deliver_tracked_message(
            actor,
            inbox,
            thread,
            backend,
            producer,
            reconciliations,
            runtime_observation,
            hosted_cell_computing,
            without_evidence,
        )
        .await;
    };
    let inbox_sequence = last.sequence;
    let already_surfaced = {
        let surfaced_inbox = Arc::clone(inbox);
        tidepool_runtime::spawn_blocking_in_span(move || surfaced_inbox.surfaced_out_of_order())
            .await
            .map_err(|error| format!("inbox reader task: {error}"))?
    };
    let mut out_of_order_delivered = Vec::new();
    let mut suppressed_watches = Vec::new();
    let mut stale_watches = Vec::new();
    let pending = pending
        .into_iter()
        .filter(|message| {
            if already_surfaced.contains(&message.sequence) {
                out_of_order_delivered.push(message.sequence);
                return false;
            }
            match watch_notice_disposition(&message.payload, watch_retained, watch_observed_since) {
                WatchNoticeDisposition::Deliver => true,
                WatchNoticeDisposition::Forgotten(owner, watch) => {
                    suppressed_watches.push((message.sequence, owner, watch));
                    false
                }
                WatchNoticeDisposition::Observed(owner, watch) => {
                    stale_watches.push((message.sequence, owner, watch));
                    false
                }
            }
        })
        .collect::<Vec<_>>();
    if pending.is_empty() {
        let ack_inbox = Arc::clone(inbox);
        tidepool_runtime::spawn_blocking_in_span(move || ack_inbox.acknowledge(inbox_sequence))
            .await
            .map_err(|error| format!("actor inbox acknowledgement task: {error}"))?
            .map_err(|error| error.to_string())?;
        tracing::debug!(
            actor = ?actor,
            inbox_sequence,
            suppressed_watches = ?suppressed_watches,
            "suppressed queued watch notices whose handles were forgotten"
        );
        if !stale_watches.is_empty() {
            tracing::info!(
                actor = ?actor,
                inbox_sequence,
                stale_watches = ?stale_watches,
                "acknowledged queued watch notices the owner had already observed settled"
            );
        }
        if !out_of_order_delivered.is_empty() {
            tracing::info!(
                actor = ?actor,
                inbox_sequence,
                out_of_order_delivered = ?out_of_order_delivered,
                "acknowledged notices already pushed out of order past a stuck delivery"
            );
        }
        return Ok(());
    }
    let inbox_watermark = inbox.watermark();
    let activation = pending
        .iter()
        .rev()
        .find_map(|message| match &message.payload {
            DurableActorEvent::Typed(TypedActorEvent::SessionReady {
                sequence, request, ..
            }) => Some((*request, *sequence)),
            _ => None,
        });
    let event_sequences = pending
        .iter()
        .filter(|message| {
            !matches!(
                message.payload,
                DurableActorEvent::Typed(TypedActorEvent::SessionReady { .. })
            )
        })
        .map(|message| message.sequence)
        .collect::<Vec<_>>();
    let rendered = pending
        .iter()
        .map(|message| {
            message
                .payload
                .render(runtime_observation.snapshot().launched_at_unix_ms)
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    backend
        .push(&cwd, thread, &rendered)
        .await
        .map_err(|error| error.to_string())?;
    if let Some((request, sequence)) = activation {
        runtime_observation.publish_request_activation(request, sequence);
    } else {
        runtime_observation.publish_event_activation(event_sequences, inbox_watermark);
    }
    let ack_inbox = Arc::clone(inbox);
    tidepool_runtime::spawn_blocking_in_span(move || ack_inbox.acknowledge(inbox_sequence))
        .await
        .map_err(|error| format!("inbox acknowledgement task: {error}"))?
        .map_err(|error| error.to_string())?;
    tracing::info!(
        actor = ?actor,
        inbox_sequence,
        inbox_watermark,
        event_count = pending.len(),
        "actor activation batch queued for native presentation"
    );
    if !suppressed_watches.is_empty() {
        tracing::debug!(
            actor = ?actor,
            inbox_sequence,
            suppressed_watches = ?suppressed_watches,
            "suppressed queued watch notices whose handles were forgotten"
        );
    }
    if !stale_watches.is_empty() {
        tracing::info!(
            actor = ?actor,
            inbox_sequence,
            stale_watches = ?stale_watches,
            "acknowledged queued watch notices the owner had already observed settled"
        );
    }
    if !out_of_order_delivered.is_empty() {
        tracing::info!(
            actor = ?actor,
            inbox_sequence,
            out_of_order_delivered = ?out_of_order_delivered,
            "acknowledged notices already pushed out of order past a stuck delivery"
        );
    }
    Ok(())
}

/// Untracked (settlement/watch/cancellation) notices queued anywhere behind a
/// stuck tracked row, pushed to the backend out of order and marked so the
/// ordinary batch path above does not render them again once the barrier
/// clears. See `ActorInbox::legacy_notices_beyond_barrier`.
/// Whether the inbox's front tracked row is in flight: submitted or
/// unconfirmed, and so expected to resolve. Only then may notices overtake
/// it; any other phase keeps the barrier.
#[cfg(feature = "codex-compat")]
fn front_tracked_row_is_in_flight(inbox: &ActorInbox) -> bool {
    let Some(sequence) = inbox.cursor().checked_add(1) else {
        return false;
    };
    matches!(
        inbox.observe_receipt(sequence),
        Ok(exomonad_node::ReceiptLookup::Retained(evidence))
            if matches!(
                evidence.phase,
                exomonad_node::DeliveryPhase::Submitted
                    | exomonad_node::DeliveryPhase::Unconfirmed
            )
    )
}

#[cfg(feature = "codex-compat")]
enum WatchNoticeDisposition {
    Deliver,
    Forgotten(ActorRef, exomonad_actor::WatchId),
    Observed(ActorRef, exomonad_actor::WatchId),
}

#[cfg(feature = "codex-compat")]
fn watch_notice_disposition(
    event: &DurableActorEvent,
    watch_retained: &(dyn Fn(ActorRef, exomonad_actor::WatchId) -> bool + Send + Sync),
    watch_observed_since: &(dyn Fn(ActorRef, exomonad_actor::WatchId, u64) -> bool + Send + Sync),
) -> WatchNoticeDisposition {
    let DurableActorEvent::Typed(TypedActorEvent::WatchChanged { notification }) = event else {
        return WatchNoticeDisposition::Deliver;
    };
    if !watch_retained(notification.owner, notification.watch) {
        return WatchNoticeDisposition::Forgotten(notification.owner, notification.watch);
    }
    // A later watch revision remains meaningful even if an earlier transition
    // was already read through pollWatch or ObserveWatchWith.
    if watch_observed_since(
        notification.owner,
        notification.watch,
        notification.occurred_at_unix_ms,
    ) {
        return WatchNoticeDisposition::Observed(notification.owner, notification.watch);
    }
    WatchNoticeDisposition::Deliver
}

#[cfg(feature = "codex-compat")]
async fn deliver_out_of_order_notices(
    actor: ActorRef,
    inbox: &Arc<ActorInbox>,
    thread: &QueueReadyThread,
    backend: &dyn InteractiveAgentBackend,
    cwd: &str,
    runtime_observation: &exomonad_actor::ActorRuntimeObservationHandle,
    watch_retained: &(dyn Fn(ActorRef, exomonad_actor::WatchId) -> bool + Send + Sync),
    watch_observed_since: &(dyn Fn(ActorRef, exomonad_actor::WatchId, u64) -> bool + Send + Sync),
) -> Result<(), String> {
    let beyond_inbox = Arc::clone(inbox);
    let beyond = tidepool_runtime::spawn_blocking_in_span(move || {
        beyond_inbox.legacy_notices_beyond_barrier()
    })
    .await
    .map_err(|error| format!("inbox reader task: {error}"))?
    .map_err(|error| error.to_string())?;
    // Only notices overtake a stuck row; ordinary messages keep their order.
    let mut suppressed = Vec::new();
    let beyond: Vec<_> = beyond
        .into_iter()
        .filter(|message| {
            if !matches!(
                message.payload,
                DurableActorEvent::Typed(
                    TypedActorEvent::SettlementChanged { .. }
                        | TypedActorEvent::WatchChanged { .. }
                        | TypedActorEvent::RequestCancellation { .. }
                )
            ) {
                return false;
            }
            match watch_notice_disposition(&message.payload, watch_retained, watch_observed_since) {
                WatchNoticeDisposition::Deliver => true,
                WatchNoticeDisposition::Forgotten(..) | WatchNoticeDisposition::Observed(..) => {
                    suppressed.push(message.sequence);
                    false
                }
            }
        })
        .collect();
    // These rows remain behind the tracked cursor, but must also stay silent
    // when that barrier eventually clears in this process.
    inbox.mark_surfaced_out_of_order(suppressed.iter().copied());
    if beyond.is_empty() {
        return Ok(());
    }
    let rendered = beyond
        .iter()
        .map(|message| {
            message
                .payload
                .render(runtime_observation.snapshot().launched_at_unix_ms)
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    backend
        .push(cwd, thread, &rendered)
        .await
        .map_err(|error| error.to_string())?;
    let sequences = beyond
        .iter()
        .map(|message| message.sequence)
        .collect::<Vec<_>>();
    inbox.mark_surfaced_out_of_order(sequences.iter().copied());
    tracing::info!(
        actor = ?actor,
        sequences = ?sequences,
        "queued settlement/watch notice past a pending native delivery"
    );
    Ok(())
}

/// The existing inbox pump is the sole sender for tracked actor messages.
/// Its durable attempt fence prevents a cancelled or uncertain submission from
/// being sent again, and prevents later rows from overtaking it.
///
/// A row is not submitted while the target is inside a computing `haskell`
/// cell: native input control cancels that call before admitting input and
/// cannot complete the exchange until the cell ends. A submitted row whose
/// native evidence stays absent or unknown past
/// `WITHDRAW_WITHOUT_EVIDENCE_AFTER` (first observation tracked per native
/// key in `without_evidence`, pump memory only) is withdrawn; a settled
/// withdrawal retires it and re-delivers its payload (`redeliver_withdrawn`)
/// at the back of the queue, so messages queued after it are presented
/// first; the re-delivered text says so.
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "codex-compat")]
async fn deliver_tracked_message(
    actor: ActorRef,
    inbox: &ActorInbox,
    thread: &QueueReadyThread,
    backend: &dyn InteractiveAgentBackend,
    producer: &InputProducerId,
    reconciliations: &Mutex<BTreeMap<String, PendingUpdateReconciliation>>,
    observation: &exomonad_actor::ActorRuntimeObservationHandle,
    hosted_cell_computing: &(dyn Fn() -> bool + Send + Sync),
    without_evidence: &Mutex<BTreeMap<String, WithoutEvidence>>,
) -> Result<(), String> {
    use exomonad_node::{DeliveryPhase, ReceiptLookup};
    let sequence = inbox
        .cursor()
        .checked_add(1)
        .ok_or("inbox sequence exhausted")?;
    let ReceiptLookup::Retained(evidence) =
        inbox.observe_receipt(sequence).map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    let target = match &evidence.context {
        DeliveryProvenance::Notification { target, .. }
        | DeliveryProvenance::RequestUpdate { target, .. } => *target,
    };
    if target != actor {
        return Err(format!(
            "message {sequence} targets another actor incarnation"
        ));
    }
    let native_sequence =
        std::num::NonZeroU64::new(sequence).ok_or("durable inbox allocated zero sequence")?;
    let operation_id = InputOperationId {
        producer: producer.clone(),
        sequence: native_sequence,
    };
    let native_key = operation_id.native_key();
    if evidence.phase == DeliveryPhase::Compacted {
        finish_update_reconciliation(
            reconciliations,
            &native_key,
            exomonad_actor::LateUpdateEvidence::Compacted(
                "native input evidence is compacted; resubmission remains fenced".into(),
            ),
        )?;
        return Err(format!(
            "message {sequence} native operation {native_key} retains terminal compacted evidence; later delivery is fenced"
        ));
    }
    if matches!(evidence.context, DeliveryProvenance::RequestUpdate { .. })
        && !reconciliations.lock().contains_key(&native_key)
    {
        return Err(format!(
            "request update {sequence} awaits its exact actor correlation"
        ));
    }
    let (purpose, correlation) = match &evidence.context {
        DeliveryProvenance::Notification { .. } => (
            InputPurpose::Notification,
            Some(format!("notification-{sequence}")),
        ),
        DeliveryProvenance::RequestUpdate {
            request, update, ..
        } => (
            InputPurpose::RequestUpdate,
            Some(format!("request-{}-update-{update}", request.0)),
        ),
    };
    let envelope = inbox
        .front_pending()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("tracked receipt {sequence} has no pending row"))?;
    if envelope.sequence != sequence || envelope.receipt_context.as_ref() != Some(&evidence.context)
    {
        return Err(format!(
            "tracked receipt {sequence} does not match the durable front row"
        ));
    }
    let mut rendered = envelope
        .payload
        .render(observation.snapshot().launched_at_unix_ms);
    if let Some(redelivery) = envelope.redelivery {
        rendered = format!("{REDELIVERED_PREFIX}\n{rendered}");
        if redelivery.possibly_seen {
            rendered = format!("{POSSIBLY_SEEN_PREFIX}\n{rendered}");
        }
    }
    let operation = InteractiveInputEnvelope::new(
        operation_id,
        purpose,
        InteractiveInputMode::StartOrSteer,
        InteractiveInputTarget {
            conversation: thread.id().clone(),
            actor: format!("{}@{}", actor.id.0, actor.incarnation.0),
            correlation,
        },
        rendered.into_bytes(),
    )
    .map_err(|error| error.to_string())?;

    let outcome = match evidence.phase {
        DeliveryPhase::Accepted => {
            if hosted_cell_computing() {
                return Err(format!(
                    "message {sequence} deferred: target is inside a computing haskell cell"
                ));
            }
            let attempt = inbox
                .begin_tracked_delivery(sequence)
                .map_err(|e| e.to_string())?;
            match backend.submit_input(thread, &operation).await {
                Ok(outcome) => {
                    let persisted = if matches!(
                        outcome,
                        exomonad_agent::InputAdmission::Unknown
                            | exomonad_agent::InputAdmission::EvidenceUnavailable
                            | exomonad_agent::InputAdmission::Compacted
                    ) {
                        attempt.unconfirmed()
                    } else {
                        attempt.submitted()
                    };
                    persisted.map_err(|e| e.to_string())?;
                    outcome
                }
                Err(exomonad_agent::InteractiveInputError::NotSubmitted(error)) => {
                    attempt.not_submitted().map_err(|e| e.to_string())?;
                    return Err(error.to_string());
                }
                Err(exomonad_agent::InteractiveInputError::Unconfirmed(error)) => {
                    attempt.unconfirmed().map_err(|e| e.to_string())?;
                    retain_update_unconfirmed(reconciliations, &native_key, error.to_string());
                    return Err(error.to_string());
                }
            }
        }
        DeliveryPhase::Submitted | DeliveryPhase::Unconfirmed => {
            let queried = backend
                .query_input(thread, operation.id())
                .await
                .map_err(|error| error.to_string())?;
            if !matches!(
                queried,
                exomonad_agent::InputAdmission::EvidenceUnavailable
                    | exomonad_agent::InputAdmission::Unknown
            ) {
                without_evidence.lock().remove(&native_key);
                queried
            } else {
                let memory = *without_evidence
                    .lock()
                    .entry(native_key.clone())
                    .or_insert_with(|| WithoutEvidence::Since(std::time::Instant::now()));
                match memory {
                    WithoutEvidence::WithdrawUnavailable => queried,
                    WithoutEvidence::Since(since)
                        if since.elapsed() < WITHDRAW_WITHOUT_EVIDENCE_AFTER =>
                    {
                        queried
                    }
                    WithoutEvidence::Since(_) => {
                        let withdrawn = backend
                            .withdraw_input(thread, operation.id())
                            .await
                            .map_err(|error| error.to_string())?;
                        match withdrawn {
                            exomonad_agent::InputAdmission::Withdrawn
                            | exomonad_agent::InputAdmission::Unknown => {
                                without_evidence.lock().remove(&native_key);
                                // A copy already labelled possibly seen keeps
                                // the label through every later re-delivery.
                                let possibly_seen = withdrawn
                                    == exomonad_agent::InputAdmission::Unknown
                                    || envelope
                                        .redelivery
                                        .is_some_and(|redelivery| redelivery.possibly_seen);
                                return redeliver_withdrawn(
                                    actor,
                                    inbox,
                                    sequence,
                                    &evidence.context,
                                    possibly_seen,
                                    reconciliations,
                                    &native_key,
                                );
                            }
                            exomonad_agent::InputAdmission::EvidenceUnavailable => {
                                without_evidence.lock().insert(
                                    native_key.clone(),
                                    WithoutEvidence::WithdrawUnavailable,
                                );
                                tracing::warn!(
                                    actor = ?actor,
                                    sequence,
                                    native_key = %native_key,
                                    "native input control cannot withdraw an input it holds no evidence for; the row stays unconfirmed and later tracked messages stay fenced"
                                );
                                queried
                            }
                            settled => {
                                without_evidence.lock().remove(&native_key);
                                settled
                            }
                        }
                    }
                }
            }
        }
        phase => {
            return Err(format!(
                "message {sequence} retains unexpected delivery phase {phase:?}"
            ));
        }
    };

    match outcome {
        exomonad_agent::InputAdmission::Presented => {
            inbox
                .confirm_presented_exact(sequence, &evidence.context)
                .map_err(|e| e.to_string())?;
            finish_update_reconciliation(
                reconciliations,
                &native_key,
                exomonad_actor::LateUpdateEvidence::Presented,
            )?;
            observation.publish_event_activation(vec![sequence], inbox.watermark());
            Ok(())
        }
        exomonad_agent::InputAdmission::Withdrawn => {
            inbox
                .confirm_withdrawn(sequence, &evidence.context)
                .map_err(|e| e.to_string())?;
            finish_update_reconciliation(
                reconciliations,
                &native_key,
                exomonad_actor::LateUpdateEvidence::NotPresented(
                    "native input was withdrawn before presentation".into(),
                ),
            )?;
            Ok(())
        }
        exomonad_agent::InputAdmission::Rejected => {
            inbox
                .confirm_rejected(sequence, &evidence.context)
                .map_err(|e| e.to_string())?;
            finish_update_reconciliation(
                reconciliations,
                &native_key,
                exomonad_actor::LateUpdateEvidence::NotPresented(
                    "native input was rejected before presentation".into(),
                ),
            )?;
            Ok(())
        }
        exomonad_agent::InputAdmission::Compacted => {
            inbox
                .confirm_compacted_exact(sequence, &evidence.context)
                .map_err(|e| e.to_string())?;
            let detail = format!(
                "message {sequence} native operation {native_key} has compacted input evidence; resubmission remains fenced"
            );
            finish_update_reconciliation(
                reconciliations,
                &native_key,
                exomonad_actor::LateUpdateEvidence::Compacted(detail.clone()),
            )?;
            Err(detail)
        }
        exomonad_agent::InputAdmission::NotSubmitted
        | exomonad_agent::InputAdmission::Admitted
        | exomonad_agent::InputAdmission::Dispatching
        | exomonad_agent::InputAdmission::Unknown
        | exomonad_agent::InputAdmission::EvidenceUnavailable => {
            let phase = match inbox.observe_receipt(sequence) {
                Ok(ReceiptLookup::Retained(receipt)) => format!("{:?}", receipt.phase),
                Ok(ReceiptLookup::Unavailable) => "unavailable".to_owned(),
                Err(error) => format!("unreadable ({error})"),
            };
            Err(format!(
                "message {sequence} native operation {native_key} remains pending in durable phase {phase}; latest provider input state: {outcome:?}"
            ))
        }
    }
}

/// Retire a tracked row whose native input was withdrawn after it stayed
/// without evidence. `possibly_seen` means native control had admitted it and
/// cannot say whether the model saw it. A notification is queued again under
/// a fresh sequence (`ActorInbox::redeliver_withdrawn`), labelled when
/// possibly seen. A request update is never re-queued as a replacement: its
/// owner receives `NotPresented` or `Unconfirmed` and decides.
#[cfg(feature = "codex-compat")]
fn redeliver_withdrawn(
    actor: ActorRef,
    inbox: &ActorInbox,
    sequence: u64,
    context: &DeliveryProvenance,
    possibly_seen: bool,
    reconciliations: &Mutex<BTreeMap<String, PendingUpdateReconciliation>>,
    native_key: &str,
) -> Result<(), String> {
    match context {
        DeliveryProvenance::Notification { .. } => {
            let redelivered = inbox
                .redeliver_withdrawn(sequence, context, possibly_seen)
                .map_err(|e| e.to_string())?;
            tracing::warn!(
                actor = ?actor,
                sequence,
                redelivered = redelivered.sequence,
                possibly_seen,
                native_key,
                "withdrew a native input that stayed without evidence; re-queued its message"
            );
        }
        DeliveryProvenance::RequestUpdate { .. } => {
            inbox
                .confirm_withdrawn(sequence, context)
                .map_err(|e| e.to_string())?;
            let evidence = if possibly_seen {
                exomonad_actor::LateUpdateEvidence::Unconfirmed(
                    "native input was admitted but its presentation is unknown; it was withdrawn from further dispatch".into(),
                )
            } else {
                exomonad_actor::LateUpdateEvidence::NotPresented(
                    "native input was never admitted and was withdrawn".into(),
                )
            };
            finish_update_reconciliation(reconciliations, native_key, evidence)?;
            tracing::warn!(
                actor = ?actor,
                sequence,
                possibly_seen,
                native_key,
                "withdrew a request update that stayed without native evidence"
            );
        }
    }
    Ok(())
}

#[cfg(feature = "codex-compat")]
fn retain_update_unconfirmed(
    reconciliations: &Mutex<BTreeMap<String, PendingUpdateReconciliation>>,
    native_key: &str,
    detail: String,
) {
    if let Some(pending) = reconciliations.lock().get(native_key) {
        pending.retain_unconfirmed(detail);
    }
}

#[cfg(feature = "codex-compat")]
fn finish_update_reconciliation(
    reconciliations: &Mutex<BTreeMap<String, PendingUpdateReconciliation>>,
    native_key: &str,
    evidence: exomonad_actor::LateUpdateEvidence,
) -> Result<(), String> {
    let pending = reconciliations.lock().get(native_key).cloned();
    let Some(pending) = pending else {
        return Ok(());
    };
    pending
        .reconciler
        .reconcile(evidence)
        .map_err(|error| error.to_string())?;
    reconciliations.lock().remove(native_key);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
#[cfg(feature = "codex-compat")]
pub(super) async fn run_delivery_pump(
    actor: ActorRef,
    inbox: Arc<ActorInbox>,
    thread: QueueReadyThread,
    backend: Arc<dyn InteractiveAgentBackend>,
    producer: InputProducerId,
    reconciliations: Arc<Mutex<BTreeMap<String, PendingUpdateReconciliation>>>,
    workspace: PathBuf,
    runtime_observation: exomonad_actor::ActorRuntimeObservationHandle,
    local_actor: LocalActorRef,
    watch_retained: WatchRetentionCheck,
    watch_observed_since: WatchObservationCheck,
    // `None` for the root, which no parent waits on.
    open_request: Option<OpenRequestCheck>,
    source_layers: Option<Arc<crate::exomonad::source::ExomonadSourceReload>>,
    worktrees: WorktreeManager,
    shutdown: oneshot::Receiver<()>,
) {
    let mut health = tokio::time::interval(Duration::from_secs(1));
    health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let provider_backend = Arc::clone(&backend);
    let provider_thread = thread.clone();
    let provider_workspace = workspace.clone();
    let provider_observation = runtime_observation.clone();
    let provider = move |provider_open: Arc<Mutex<bool>>,
                         mut stop_provider: oneshot::Receiver<()>| async move {
        let mut reminded_request = None;
        let mut interval = tokio::time::interval(PROVIDER_POLL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = &mut stop_provider => break,
                _ = interval.tick() => {}
            }
            if !*provider_open.lock() {
                break;
            }
            provider_observation.mark_provider_observation_stale();
            match provider_backend.observe(&provider_thread).await {
                Ok(Some(observation)) => {
                    provider_observation.publish_provider_observation(observation)
                }
                Ok(None) => provider_observation.mark_provider_observation_stale(),
                Err(error) => {
                    provider_observation.mark_provider_observation_stale();
                    tracing::debug!(actor = ?actor, %error, "provider observation unavailable");
                }
            }
            if let Some(open_request) = &open_request {
                let now = u64::try_from(current_time_ms()).unwrap_or_default();
                if let Err(error) = remind_turn_ended_without_respond(
                    actor,
                    &provider_thread,
                    provider_backend.as_ref(),
                    &provider_workspace.to_string_lossy(),
                    &provider_observation.snapshot(),
                    || open_request(actor),
                    &mut reminded_request,
                    now,
                )
                .await
                {
                    tracing::warn!(actor = ?actor, %error, "turn-end reminder was not delivered");
                }
            }
        }
        ActorObservation::Provider
    };
    let source_observation = runtime_observation.clone();
    let source = move |source_open: Arc<Mutex<bool>>, stop_source: oneshot::Receiver<()>| async move {
        run_periodic_observation(stop_source, PROVIDER_POLL_INTERVAL, || {
            let runtime_observation = source_observation.clone();
            let source_layers = source_layers.clone();
            let worktrees = worktrees.clone();
            let admitted = *source_open.lock();
            async move {
                if admitted {
                    poll_source_drift(
                        actor,
                        &runtime_observation,
                        source_layers.as_ref(),
                        &worktrees,
                    )
                    .await;
                }
            }
        })
        .await;
        ActorObservation::Source
    };
    let hosted_cell_computing = move || local_actor.hosted_cell_computing();
    let without_evidence = Mutex::new(BTreeMap::new());
    let mut pending: Option<PendingDeliveryWarning> = None;
    let mut last_message = None;
    let delivery = async {
        loop {
            health.tick().await;
            let result = deliver_pending_checked(
                actor,
                &inbox,
                &thread,
                backend.as_ref(),
                &producer,
                &reconciliations,
                &workspace,
                &runtime_observation,
                watch_retained.as_ref(),
                watch_observed_since.as_ref(),
                &hosted_cell_computing,
                &without_evidence,
            )
            .await;
            match result {
                Ok(()) => {
                    if let Some(pending) = pending.take() {
                        tracing::info!(
                            actor = ?actor,
                            pending_secs = pending.since.elapsed().as_secs(),
                            "actor inbox delivery recovered"
                        );
                    }
                }
                Err(error) => PendingDeliveryWarning::observe(&mut pending, actor, error),
            }
            runtime_observation.publish_inbound_delivery(observe_inbound_delivery(
                &inbox,
                &producer,
                hosted_cell_computing(),
                &without_evidence,
                &mut last_message,
            ));
        }
    };
    supervise_delivery(
        actor,
        &runtime_observation,
        shutdown,
        delivery,
        provider,
        source,
    )
    .await;
}

/// Own the single inbox future and both background observers through the same
/// retirement boundary. An admitted observation finishes or makes delivery
/// cleanup forced, which retains the checkout it might still be reading.
#[cfg(feature = "codex-compat")]
pub(super) async fn supervise_delivery<D, PF, P, SF, S>(
    actor: ActorRef,
    runtime_observation: &exomonad_actor::ActorRuntimeObservationHandle,
    mut shutdown: oneshot::Receiver<()>,
    delivery: D,
    provider: PF,
    source: SF,
) where
    D: std::future::Future<Output = ()>,
    PF: FnOnce(Arc<Mutex<bool>>, oneshot::Receiver<()>) -> P,
    P: std::future::Future<Output = ActorObservation> + Send + 'static,
    SF: FnOnce(Arc<Mutex<bool>>, oneshot::Receiver<()>) -> S,
    S: std::future::Future<Output = ActorObservation> + Send + 'static,
{
    let mut observations = JoinSet::new();
    let (provider_shutdown, stop_provider) = oneshot::channel();
    let (source_shutdown, stop_source) = oneshot::channel();
    // Closing admission orders new work against retirement. Previously
    // admitted work may still start, so shutdown also joins both observers.
    let observation_open = Arc::new(Mutex::new(true));
    observations.spawn(provider(Arc::clone(&observation_open), stop_provider));
    observations.spawn(source(Arc::clone(&observation_open), stop_source));
    let mut observation_failed = false;
    tokio::pin!(delivery);
    until_shutdown(&mut shutdown, async {
        loop {
            tokio::select! {
                _ = &mut delivery => break,
                joined = observations.join_next(), if !observations.is_empty() => {
                    match joined {
                        Some(Ok(kind)) => {
                            tracing::error!(actor = ?actor, ?kind, "actor observation task stopped before retirement");
                            observation_failed = true;
                            if matches!(kind, ActorObservation::Provider) {
                                runtime_observation.mark_provider_observation_stale();
                            }
                        }
                        Some(Err(error)) => {
                            tracing::error!(actor = ?actor, %error, "actor observation task failed before retirement");
                            observation_failed = true;
                            runtime_observation.mark_provider_observation_stale();
                        }
                        None => unreachable!("nonempty observation task set returned no task"),
                    }
                }
            }
        }
    })
    .await;
    *observation_open.lock() = false;
    provider_shutdown.send(()).ok();
    source_shutdown.send(()).ok();
    while let Some(result) = observations.join_next().await {
        if let Err(error) = result {
            panic!("actor {actor:?} observation task failed: {error}");
        }
    }
    assert!(
        !observation_failed,
        "actor {actor:?} observation task failed before retirement"
    );
}

#[derive(Debug)]
#[cfg(feature = "codex-compat")]
pub(super) enum ActorObservation {
    Provider,
    Source,
}

/// One observation at a time. A shutdown requested during a probe waits for
/// that probe to finish, so retirement can retain its workspace if it exceeds
/// the delivery grace period. Dropping the owning JoinSet aborts async tasks;
/// a detached blocking Git read is then covered by the forced retirement.
#[cfg(feature = "codex-compat")]
pub(super) async fn run_periodic_observation<F, Fut>(
    mut shutdown: oneshot::Receiver<()>,
    period: Duration,
    mut observe: F,
) where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => break,
            _ = interval.tick() => observe().await,
        }
    }
}

/// Run `work` until `shutdown` fires, dropping it at whatever await it is
/// parked on. Shutdown is sent only by retirement, which joins the pump and
/// its observation tasks within `APPLICATION_TASK_GRACE_TIMEOUT`.
#[cfg(feature = "codex-compat")]
pub(super) async fn until_shutdown(
    shutdown: &mut oneshot::Receiver<()>,
    work: impl std::future::Future,
) {
    tokio::select! {
        biased;
        _ = shutdown => {}
        _ = work => {}
    }
}

#[cfg(feature = "codex-compat")]
pub(super) const PROVIDER_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// Push one reminder to an actor whose provider turn ended after its current
/// request was activated and the request is still open. The idle age is the status line's
/// `provider_idle_since_unix_ms`; the reminder waits one poll interval of
/// idleness so a model about to call `respond` is not interrupted, and fires
/// once per request in this actor pump. A later turn for the same request does
/// not make waiting for a dependency a new failure to respond.
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "codex-compat")]
pub(super) async fn remind_turn_ended_without_respond(
    actor: ActorRef,
    thread: &QueueReadyThread,
    backend: &dyn InteractiveAgentBackend,
    cwd: &str,
    observation: &exomonad_actor::ActorRuntimeObservation,
    open_request: impl FnOnce() -> Option<exomonad_actor::RequestId>,
    reminded_request: &mut Option<exomonad_actor::RequestId>,
    now_unix_ms: u64,
) -> Result<(), String> {
    if observation.provider_observation_stale {
        return Ok(());
    }
    let Some(turn) = observation
        .provider_turn
        .as_ref()
        .filter(|turn| turn.state == exomonad_agent::ProviderTurnState::Succeeded)
    else {
        return Ok(());
    };
    let Some(idle_since) = observation.provider_idle_since_unix_ms else {
        return Ok(());
    };
    let Some(activation) = observation.request_activation.as_ref() else {
        return Ok(());
    };
    if idle_since < activation.at_unix_ms
        || activation
            .provider_turn
            .as_ref()
            .is_some_and(|(thread, prior_turn)| turn.thread == *thread && turn.turn == *prior_turn)
    {
        return Ok(());
    }
    let poll_ms = u64::try_from(PROVIDER_POLL_INTERVAL.as_millis()).unwrap_or(u64::MAX);
    if now_unix_ms.saturating_sub(idle_since) < poll_ms {
        return Ok(());
    }
    let Some(request) = open_request() else {
        return Ok(());
    };
    if activation.request != request {
        return Ok(());
    }
    if *reminded_request == Some(request) {
        return Ok(());
    }
    backend
        .push(cwd, thread, &turn_end_reminder(request))
        .await
        .map_err(|error| error.to_string())?;
    *reminded_request = Some(request);
    tracing::info!(
        actor = ?actor,
        request = request.0,
        idle_since_unix_ms = idle_since,
        "reminded an actor whose turn ended with its request open"
    );
    Ok(())
}

#[cfg(feature = "codex-compat")]
pub(super) fn turn_end_reminder(request: exomonad_actor::RequestId) -> String {
    format!(
        "Request {} appeared open when this reminder was queued. If you already submitted its reply, ignore this notice. If the work is finished, use respond with the assigned reply type. If you are waiting for a prerequisite, keep the request open; inform the requester of the specific prerequisite and its owner if you have not already done so, then resume when an update arrives.",
        request.0
    )
}

/// The inbound-delivery observation for a parent's status view, derived from
/// the same durable front row, cell check and no-evidence memory that
/// `deliver_tracked_message` acts on. `last_message` is pump memory of the
/// most recent tracked front row and when its current state was first seen.
#[cfg(feature = "codex-compat")]
pub(super) fn observe_inbound_delivery(
    inbox: &ActorInbox,
    producer: &InputProducerId,
    cell_computing: bool,
    without_evidence: &Mutex<BTreeMap<String, WithoutEvidence>>,
    last_message: &mut Option<exomonad_actor::TrackedMessageObservation>,
) -> exomonad_actor::InboundDeliveryObservation {
    use exomonad_actor::{InboundNext, InboxDelivery, TrackedMessageState};
    use exomonad_node::{DeliveryPhase, ReceiptLookup};
    let now = u64::try_from(current_time_ms()).unwrap_or_default();
    let state_of = |phase| match phase {
        DeliveryPhase::Accepted => TrackedMessageState::Queued,
        DeliveryPhase::InFlight | DeliveryPhase::Submitted => TrackedMessageState::Submitted,
        DeliveryPhase::Unconfirmed => TrackedMessageState::Unconfirmed,
        DeliveryPhase::Presented => TrackedMessageState::Presented,
        DeliveryPhase::Withdrawn => TrackedMessageState::Withdrawn,
        DeliveryPhase::Rejected => TrackedMessageState::Rejected,
        DeliveryPhase::Compacted => TrackedMessageState::Compacted,
    };
    let record = |last_message: &mut Option<exomonad_actor::TrackedMessageObservation>,
                  sequence: u64,
                  phase: DeliveryPhase| {
        let state = state_of(phase);
        if !last_message
            .as_ref()
            .is_some_and(|message| message.sequence == sequence && message.state == state)
        {
            *last_message = Some(exomonad_actor::TrackedMessageObservation {
                sequence,
                state,
                at_unix_ms: now,
            });
        }
    };
    let front = inbox
        .front_pending()
        .ok()
        .flatten()
        .filter(|row| row.receipt_context.is_some())
        .and_then(|row| match inbox.observe_receipt(row.sequence) {
            Ok(ReceiptLookup::Retained(evidence)) => {
                Some((row.sequence, evidence.phase, row.redelivery.is_some()))
            }
            _ => None,
        });
    // A message that left the front resolved; record how.
    if let Some(previous) = last_message.as_ref().map(|message| message.sequence) {
        if front.is_none_or(|(sequence, _, _)| sequence != previous) {
            if let Ok(ReceiptLookup::Retained(evidence)) = inbox.observe_receipt(previous) {
                record(last_message, previous, evidence.phase);
            }
        }
    }
    let Some((sequence, phase, redelivered)) = front else {
        return exomonad_actor::InboundDeliveryObservation {
            inbox: InboxDelivery::Open,
            last_message: last_message.clone(),
            next: InboundNext::AwaitEvent,
        };
    };
    record(last_message, sequence, phase);
    let since_unix_ms = last_message
        .as_ref()
        .map_or(now, |message| message.at_unix_ms);
    let behind = inbox.watermark().saturating_sub(sequence);
    let no_evidence = std::num::NonZeroU64::new(sequence).and_then(|native_sequence| {
        let key = InputOperationId {
            producer: producer.clone(),
            sequence: native_sequence,
        }
        .native_key();
        without_evidence.lock().get(&key).copied()
    });
    let (inbox_state, next) = match phase {
        DeliveryPhase::Accepted if cell_computing => (
            InboxDelivery::WaitingForCell { since_unix_ms },
            InboundNext::AwaitCell,
        ),
        DeliveryPhase::Accepted if redelivered => (InboxDelivery::Open, InboundNext::Resubmitting),
        // An in-flight message fences later ones only once it has stayed
        // without native evidence past the grace period.
        DeliveryPhase::InFlight | DeliveryPhase::Submitted | DeliveryPhase::Unconfirmed => {
            let word = if phase == DeliveryPhase::Unconfirmed {
                "unconfirmed"
            } else {
                "submitted"
            };
            let fenced = |reason: &str| InboxDelivery::Fenced {
                reason: format!("message {sequence} {word}, {reason}, {behind} behind"),
                since_unix_ms,
            };
            match no_evidence {
                Some(WithoutEvidence::WithdrawUnavailable) => (
                    fenced("withdrawal unavailable"),
                    InboundNext::NoHostRecovery,
                ),
                Some(WithoutEvidence::Since(first))
                    if first.elapsed() >= WITHDRAW_WITHOUT_EVIDENCE_AFTER =>
                {
                    (fenced("no native evidence"), InboundNext::Resubmitting)
                }
                _ if redelivered => (InboxDelivery::Open, InboundNext::Resubmitting),
                _ => (InboxDelivery::Open, InboundNext::AwaitEvent),
            }
        }
        DeliveryPhase::Compacted => (
            InboxDelivery::Fenced {
                reason: format!("message {sequence} compacted, {behind} behind"),
                since_unix_ms,
            },
            InboundNext::NoHostRecovery,
        ),
        DeliveryPhase::Accepted
        | DeliveryPhase::Presented
        | DeliveryPhase::Withdrawn
        | DeliveryPhase::Rejected => (InboxDelivery::Open, InboundNext::AwaitEvent),
    };
    exomonad_actor::InboundDeliveryObservation {
        inbox: inbox_state,
        last_message: last_message.clone(),
        next,
    }
}

/// The delivery pump's WARN for a front row that keeps failing: logged when
/// the error changes and again every `PENDING_DELIVERY_WARN_INTERVAL` while
/// it repeats, with the tick count and how long delivery has been pending.
#[cfg(feature = "codex-compat")]
struct PendingDeliveryWarning {
    error: String,
    since: std::time::Instant,
    logged_at: std::time::Instant,
    ticks: u64,
}

#[cfg(feature = "codex-compat")]
impl PendingDeliveryWarning {
    fn observe(pending: &mut Option<Self>, actor: ActorRef, error: String) {
        let now = std::time::Instant::now();
        let state = pending.get_or_insert_with(|| Self {
            error: String::new(),
            since: now,
            logged_at: now,
            ticks: 0,
        });
        state.ticks += 1;
        let changed = state.error != error;
        if changed || now.duration_since(state.logged_at) >= PENDING_DELIVERY_WARN_INTERVAL {
            tracing::warn!(
                actor = ?actor,
                %error,
                ticks = state.ticks,
                pending_secs = now.duration_since(state.since).as_secs(),
                "actor inbox delivery remains pending"
            );
            state.logged_at = now;
        }
        if changed {
            state.error = error;
        }
    }
}

/// Observe source drift for `actor` on a 10-second cadence. Reading a source
/// layer's disk revision or a checkout's dirty
/// files is real filesystem and Git work, and the status view this feeds
/// (`ResidentKernelBehavior::live_status_text`) must stay cheap on every
/// call.
///
/// Each row is observed independently. An absent service stays unobserved;
/// every attempted poll publishes pending before I/O and unavailable on error.
/// All filesystem and Git work
/// runs on a blocking thread; nothing here runs on the async executor.
#[cfg(feature = "codex-compat")]
async fn poll_source_drift(
    actor: ActorRef,
    runtime_observation: &exomonad_actor::ActorRuntimeObservationHandle,
    source_layers: Option<&Arc<crate::exomonad::source::ExomonadSourceReload>>,
    worktrees: &WorktreeManager,
) {
    let observation = runtime_observation.snapshot();
    let worktree_id = observation
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.worktree_id.clone());
    // The root holds no managed worktree: its checkout is the run's
    // workspace repository, the operator checkout other actors' `revisions`
    // status compares against.
    let root_checkout = observation
        .workspace
        .as_ref()
        .filter(|workspace| {
            workspace.worktree_id.is_none()
                && observation
                    .launch_role
                    .as_ref()
                    .is_some_and(|role| role.role() == exomonad_actor::ActorRole::Root)
        })
        .map(|workspace| workspace.host_storage_path.clone());
    let source_layers = source_layers.cloned();
    let worktrees = worktrees.clone();
    let caller = tidepool_repr::PrincipalId::from(actor);
    let targets = exomonad_actor::SourceDriftTargets {
        layer: source_layers.is_some(),
        frozen: source_layers.is_some(),
        checkout: worktree_id.is_some() || root_checkout.is_some(),
    };
    runtime_observation.begin_source_drift_poll(targets);
    let result = tidepool_runtime::spawn_blocking_in_span(move || {
        let layer = source_layers
            .as_ref()
            .map(|layers| layers.drift(caller).map_err(|error| format!("{error:?}")));
        let frozen = source_layers
            .as_ref()
            .map(|layers| layers.frozen_drift().map_err(|error| error.to_string()));
        let checkout = match (worktree_id, root_checkout) {
            (Some(id), _) => Some(checkout_git_drift(&worktrees, &id)),
            (None, Some(path)) => Some(git_drift_at(worktrees.git(), &path)),
            (None, None) => None,
        };
        (layer, frozen, checkout)
    })
    .await;
    publish_source_drift_result(runtime_observation, targets, result);
}

#[cfg(feature = "codex-compat")]
type SourceDriftResults = (
    Option<Result<exomonad_actor::SourceLayerDrift, String>>,
    Option<Result<exomonad_actor::FrozenSourceDrift, String>>,
    Option<Result<exomonad_actor::CheckoutGitDrift, String>>,
);

#[cfg(feature = "codex-compat")]
fn publish_source_drift_result(
    observation: &exomonad_actor::ActorRuntimeObservationHandle,
    targets: exomonad_actor::SourceDriftTargets,
    result: Result<SourceDriftResults, tokio::task::JoinError>,
) {
    let (layer, frozen, checkout) = match result {
        Ok(rows) => rows,
        Err(error) => {
            let reason = format!("source drift observation task failed: {error}");
            tracing::debug!(%reason, "source drift unavailable");
            if targets.layer {
                observation.fail_source_layer_drift(reason.clone());
            }
            if targets.frozen {
                observation.fail_frozen_source_drift(reason.clone());
            }
            if targets.checkout {
                observation.fail_checkout_git_drift(reason);
            }
            return;
        }
    };
    match layer {
        Some(Ok(sample)) => observation.publish_source_layer_drift(sample),
        Some(Err(reason)) => observation.fail_source_layer_drift(reason),
        None if targets.layer => {
            observation.fail_source_layer_drift("source layer poll returned no result")
        }
        None => {}
    }
    match frozen {
        Some(Ok(sample)) => observation.publish_frozen_source_drift(sample),
        Some(Err(reason)) => observation.fail_frozen_source_drift(reason),
        None if targets.frozen => {
            observation.fail_frozen_source_drift("frozen source poll returned no result")
        }
        None => {}
    }
    match checkout {
        Some(Ok(sample)) => observation.publish_checkout_git_drift(sample),
        Some(Err(reason)) => observation.fail_checkout_git_drift(reason),
        None if targets.checkout => {
            observation.fail_checkout_git_drift("checkout poll returned no result")
        }
        None => {}
    }
}

#[cfg(all(test, feature = "codex-compat"))]
#[tokio::test]
async fn source_drift_join_failure_marks_attempted_rows_unavailable() {
    let observation = exomonad_actor::ActorRuntimeObservationHandle::default();
    let targets = exomonad_actor::SourceDriftTargets {
        layer: true,
        frozen: true,
        checkout: false,
    };
    observation.publish_source_layer_drift(exomonad_actor::SourceLayerDrift {
        active_identity: "old".into(),
        active_generation: 1,
        disk_identity: "old".into(),
        disk_generation: 1,
        changed_modules: Vec::new(),
    });
    observation.begin_source_drift_poll(targets);
    let failed: Result<SourceDriftResults, _> = tokio::spawn(async {
        panic!("source observation task panicked");
        #[allow(unreachable_code)]
        (None, None, None)
    })
    .await;
    publish_source_drift_result(&observation, targets, failed);
    let observed = observation.snapshot().source_drift;
    assert!(matches!(
        observed.layer,
        exomonad_actor::SourceObservation::Unavailable {
            last_known: Some(_),
            ..
        }
    ));
    assert!(matches!(
        observed.frozen,
        exomonad_actor::SourceObservation::Unavailable {
            last_known: None,
            ..
        }
    ));
    assert!(matches!(
        observed.checkout,
        exomonad_actor::SourceObservation::NotObserved
    ));

    observation.begin_source_drift_poll(targets);
    publish_source_drift_result(
        &observation,
        targets,
        Ok((
            Some(Err("source read failed".into())),
            Some(Ok(exomonad_actor::FrozenSourceDrift {
                changed_modules: Vec::new(),
            })),
            None,
        )),
    );
    let observed = observation.snapshot().source_drift;
    assert!(
        matches!(observed.layer, exomonad_actor::SourceObservation::Unavailable { reason, last_known: Some(_), .. } if reason == "source read failed")
    );
    assert!(observed.frozen.current().is_some());
}
