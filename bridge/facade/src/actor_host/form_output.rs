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
