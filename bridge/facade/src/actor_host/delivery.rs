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

#[cfg(test)]
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

#[cfg(test)]
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
