//! The native interaction boundary publishes through Harness's existing Store.
use exomonad_actor::{ActorRef, FormHost, FormPublication};
use harness::store::{actor_output::*, forms::*, Store, StoreError};
use std::sync::{Arc, OnceLock};
use tidepool_bridge_effects::{FormAttempt, FormAttemptId, FormCause, FormTransition};

pub(super) struct StoreFormHost {
    store: Arc<Store>,
    run: String,
    control: Arc<OnceLock<harness::server::ServerControl>>,
}
pub(super) fn host(
    store: Arc<Store>,
    run: String,
    control: Arc<OnceLock<harness::server::ServerControl>>,
) -> Arc<dyn FormHost> {
    Arc::new(StoreFormHost {
        store,
        run,
        control,
    })
}
fn cause(error: StoreError) -> FormCause {
    match error {
        StoreError::InvalidForm | StoreError::InvalidActorOutput => {
            FormCause::FormMalformed(error.to_string())
        }
        StoreError::FormUnavailable => FormCause::FormClosed,
        StoreError::ActorOutputRefused | StoreError::ActorOutputAuthority(_) => {
            FormCause::FormUnauthorized
        }
        other => FormCause::FormTransportFailed(other.to_string()),
    }
}
impl StoreFormHost {
    fn publish_form(&self, actor: ActorRef, mount: &str) {
        if let Some(control) = self.control.get() {
            match self.store.actor_form(&self.origin(actor), mount) {
                Ok(row) => control.publish_actor_form(&row),
                Err(error) => {
                    tracing::warn!(?actor, mount, %error, "committed form projection awaits reconciliation")
                }
            }
        }
    }
    fn origin(&self, actor: ActorRef) -> ActorOutputOrigin {
        ActorOutputOrigin {
            run: self.run.clone(),
            native_actor: actor.id.0,
            incarnation: actor.incarnation.0,
        }
    }
}
fn execution(publication: &FormPublication) -> ActorOutputExecution {
    match publication.operation.as_ref() {
        Some(operation) => ActorOutputExecution::Notebook {
            execution: operation.execution.to_string(),
            input_unit_index: operation.input_unit_index as u64,
            effect_ordinal: operation.effect_ordinal as u64,
        },
        None => ActorOutputExecution::ActorProgram,
    }
}
struct OpenAuthority<'a>(&'a ActorFormOpen);
impl ActorFormAuthority for OpenAuthority<'_> {
    fn validate_form(&self, opening: &ActorFormOpen) -> Result<bool, String> {
        Ok(opening.origin == self.0.origin
            && opening.mount_id == self.0.mount_id
            && opening.execution == self.0.execution
            && opening.conversation == self.0.conversation)
    }
}
struct ViewAuthority<'a>(&'a ActorOutputEmission);
impl ActorOutputAuthority for ViewAuthority<'_> {
    fn validate_output(&self, emission: &ActorOutputEmission) -> Result<bool, String> {
        Ok(emission.origin == self.0.origin
            && emission.id == self.0.id
            && emission.execution == self.0.execution
            && emission.conversation == self.0.conversation)
    }
}
impl FormHost for StoreFormHost {
    fn changed(&self) -> futures_util::future::BoxFuture<'static, Result<(), FormCause>> {
        let mut changes = self.store.subscribe_actor_form_changes();
        Box::pin(async move {
            changes
                .changed()
                .await
                .map_err(|error| FormCause::FormTransportFailed(error.to_string()))
        })
    }
    fn open(
        &self,
        publication: &FormPublication,
        mount: &str,
        descriptor: &serde_json::Value,
    ) -> Result<(), FormCause> {
        let opening = ActorFormOpen {
            origin: self.origin(publication.actor),
            mount_id: mount.into(),
            execution: execution(publication),
            conversation: None,
            form: serde_json::from_value(descriptor.clone())
                .map_err(|error| FormCause::FormMalformed(error.to_string()))?,
        };
        self.store
            .open_actor_form(&OpenAuthority(&opening), &opening)
            .map(|row| {
                if let Some(control) = self.control.get() {
                    control.publish_actor_form(&row);
                }
            })
            .map_err(cause)
    }
    fn attempt(&self, actor: ActorRef, mount: &str) -> Result<Option<FormAttempt>, FormCause> {
        self.store
            .actor_form_attempt(&self.origin(actor), mount)
            .map_err(cause)?
            .map(|attempt| match attempt {
                ActorFormAttempt::Submitted { attempt_id, draft } => Ok(
                    FormAttempt::FormSubmitted(FormAttemptId::FormAttemptToken(attempt_id), draft),
                ),
                ActorFormAttempt::Dismissed => Ok(FormAttempt::FormDismissed),
                ActorFormAttempt::Unavailable { cause } => Err(match cause {
                    ActorFormUnavailableCause::Interrupted => FormCause::FormInterrupted,
                    ActorFormUnavailableCause::Cancelled | ActorFormUnavailableCause::Closed => {
                        FormCause::FormClosed
                    }
                }),
            })
            .transpose()
    }
    fn reject(
        &self,
        actor: ActorRef,
        mount: &str,
        attempt: &str,
        errors: &serde_json::Value,
    ) -> Result<FormTransition, FormCause> {
        self.store
            .reject_actor_form(&self.origin(actor), mount, attempt, errors)
            .map(|applied| {
                if applied {
                    self.publish_form(actor, mount);
                }
                if applied {
                    FormTransition::FormApplied
                } else {
                    FormTransition::FormStale
                }
            })
            .map_err(cause)
    }
    fn commit(
        &self,
        actor: ActorRef,
        mount: &str,
        attempt: &str,
        presentation: &serde_json::Value,
    ) -> Result<FormTransition, FormCause> {
        self.store
            .commit_actor_form(&self.origin(actor), mount, attempt, presentation)
            .map(|applied| {
                if applied {
                    self.publish_form(actor, mount);
                }
                if applied {
                    FormTransition::FormApplied
                } else {
                    FormTransition::FormStale
                }
            })
            .map_err(cause)
    }
    fn close(&self, actor: ActorRef, mount: &str) -> Result<(), FormCause> {
        self.store
            .close_actor_form(&self.origin(actor), mount)
            .map(|applied| {
                if applied {
                    self.publish_form(actor, mount);
                }
            })
            .map_err(cause)
    }
    fn display(
        &self,
        publication: &FormPublication,
        display_slot: u64,
        view: &serde_json::Value,
    ) -> Result<(), FormCause> {
        // Rich pages have no live expansion callback; media is retained by Store.
        let emission = ActorOutputEmission {
            origin: self.origin(publication.actor),
            id: ActorOutputId {
                display_slot,
                page_ordinal: 0,
            },
            execution: execution(publication),
            conversation: None,
            page: ActorDisplayPage {
                text: String::new(),
                expansions: vec![],
                unavailable: false,
                view: Some(
                    serde_json::from_value(view.clone())
                        .map_err(|error| FormCause::FormMalformed(error.to_string()))?,
                ),
            },
        };
        self.store
            .append_actor_output(&ViewAuthority(&emission), &emission)
            .map(|commit| {
                if let Some(control) = self.control.get() {
                    control.publish_actor_output(commit.output());
                }
            })
            .map_err(cause)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn production_form_wait_observes_submission_before_future_is_polled() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(root.path().join("store.sqlite")).unwrap());
        let host = StoreFormHost {
            store: store.clone(),
            run: "run".into(),
            control: Arc::new(OnceLock::new()),
        };
        let actor = ActorRef::first(exomonad_actor::ActorId(1));
        host.open(
            &FormPublication {
                actor,
                operation: None,
            },
            "form",
            &json!({"version":1,"root":{"kind":"empty"}}),
        )
        .unwrap();

        // The native loop subscribes before inspecting durable state. A browser
        // submission between the read and select must remain observable.
        let changed = host.changed();
        assert!(host.attempt(actor, "form").unwrap().is_none());
        store
            .submit_actor_form(&host.origin(actor), "form", "submission", &json!({}))
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), changed)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            host.attempt(actor, "form").unwrap(),
            Some(FormAttempt::FormSubmitted(_, _))
        ));

        let changed = host.changed();
        host.close(actor, "form").unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), changed)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            host.attempt(actor, "form"),
            Err(FormCause::FormClosed)
        ));
    }

    #[test]
    fn production_form_bridge_retains_drafts_and_commits_before_return() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(root.path().join("store.sqlite")).unwrap());
        let host = StoreFormHost {
            store: store.clone(),
            run: "run".into(),
            control: Arc::new(OnceLock::new()),
        };
        let actor = ActorRef::first(exomonad_actor::ActorId(1));
        let publication = FormPublication {
            actor,
            operation: None,
        };
        let form =
            json!({"version":1,"root":{"kind":"text","id":"f0","label":"Name","initial":null}});
        host.open(&publication, "first", &form).unwrap();
        host.open(&publication, "sibling", &form).unwrap();
        let origin = host.origin(actor);
        let draft = json!({"f0":""});
        let first = store
            .submit_actor_form(&origin, "first", "submission1", &draft)
            .unwrap();
        let duplicate = store
            .submit_actor_form(&origin, "first", "submission1", &draft)
            .unwrap();
        assert_eq!(first.attempt_id, duplicate.attempt_id);
        let Some(FormAttempt::FormSubmitted(FormAttemptId::FormAttemptToken(attempt), observed)) =
            host.attempt(actor, "first").unwrap()
        else {
            panic!("submission must be available")
        };
        assert_eq!(observed, draft);
        assert_eq!(
            host.reject(
                actor,
                "first",
                &attempt,
                &json!([{"field":"f0","message":"Name is required"}])
            )
            .unwrap(),
            FormTransition::FormApplied
        );
        let rejected = store.actor_form(&origin, "first").unwrap();
        assert_eq!(rejected.sequence, first.sequence);
        assert!(rejected.draft.is_some());
        assert_eq!(rejected.errors[0].message, "Name is required");
        assert!(host.attempt(actor, "first").unwrap().is_none());
        assert!(host.attempt(actor, "sibling").unwrap().is_none());
        let corrected = store
            .submit_actor_form(&origin, "first", "submission2", &json!({"f0":"Ada"}))
            .unwrap();
        let corrected_attempt = corrected.attempt_id.unwrap();
        let answer = json!({"kind":"text","text":"Ada"});
        assert_eq!(
            host.commit(actor, "first", &attempt, &answer).unwrap(),
            FormTransition::FormStale
        );
        assert_eq!(
            host.commit(actor, "first", &corrected_attempt, &answer)
                .unwrap(),
            FormTransition::FormApplied
        );
        let committed = store.actor_form(&origin, "first").unwrap();
        assert_eq!(committed.state, ActorFormState::Answered);
        assert_eq!(
            serde_json::to_value(committed.answer.unwrap()).unwrap(),
            answer
        );
        assert_eq!(
            host.commit(actor, "first", &corrected_attempt, &answer)
                .unwrap(),
            FormTransition::FormApplied
        );
        host.close(actor, "first").unwrap();
        assert_eq!(
            store.actor_form(&origin, "first").unwrap().state,
            ActorFormState::Answered
        );
        assert!(host.attempt(actor, "sibling").unwrap().is_none());
    }

    #[test]
    fn production_form_bridge_fences_answer_after_owner_closure() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(root.path().join("store.sqlite")).unwrap());
        let host = StoreFormHost {
            store: store.clone(),
            run: "run".into(),
            control: Arc::new(OnceLock::new()),
        };
        let actor = ActorRef::first(exomonad_actor::ActorId(1));
        host.open(
            &FormPublication {
                actor,
                operation: None,
            },
            "form",
            &json!({"version":1,"root":{"kind":"empty"}}),
        )
        .unwrap();
        let submitted = store
            .submit_actor_form(&host.origin(actor), "form", "submission", &json!({}))
            .unwrap();
        host.close(actor, "form").unwrap();
        assert_eq!(
            host.commit(
                actor,
                "form",
                &submitted.attempt_id.unwrap(),
                &json!({"kind":"text","text":"late"})
            )
            .unwrap(),
            FormTransition::FormStale
        );
        assert!(matches!(
            host.attempt(actor, "form"),
            Err(FormCause::FormClosed)
        ));
        assert_eq!(
            store.actor_form(&host.origin(actor), "form").unwrap().state,
            ActorFormState::Cancelled
        );
    }
}
