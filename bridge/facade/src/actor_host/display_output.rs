//! Commit actor output in the existing run Store before projecting or acknowledging it.
use super::*;
use exomonad_actor::{
    ActorDisplayAdmission, DisplayConversationIdentity, DisplayPublication,
    DisplayPublicationHostContext, DisplayPublicationOutcome,
};
use harness::{
    embedding::HostIdentity,
    model::{AgentPath, ConversationIdentity},
    store::{actor_output::*, Store, StoreError},
};

pub(super) fn open_run_store(run_root: &Path) -> Result<Arc<Store>, String> {
    let root = run_root.join("harness");
    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    Store::open(root.join("store.sqlite"))
        .map(Arc::new)
        .map_err(|error| error.to_string())
}

struct PublicationAuthority<'a> {
    forest: &'a ResidentForest<ExomonadHandlerStack, CapturedOutput>,
    admission: Arc<ActorDisplayAdmission>,
    request: &'a Arc<DisplayPublication>,
    expected: &'a ActorOutputEmission,
}

impl ActorOutputAuthority for PublicationAuthority<'_> {
    fn validate_output(&self, emission: &ActorOutputEmission) -> Result<bool, String> {
        if emission != self.expected {
            return Ok(false);
        }
        self.forest
            .validate_display_publication(&self.admission, self.request)
            .map(|()| true)
            .map_err(|error| error.to_string())
    }
}

/// The request retains its first provenance, so retries after attachment cannot
/// rewrite the exact page already offered to Store. No provider turn is created.
pub(super) fn publish(
    forest: &ResidentForest<ExomonadHandlerStack, CapturedOutput>,
    store: &Store,
    run: &str,
    conversation: Option<HostIdentity>,
    control: Option<&harness::server::ServerControl>,
    request: &Arc<DisplayPublication>,
) {
    publish_inner(forest, store, run, conversation, control, request, || {});
}

#[cfg(test)]
pub(super) fn publish_before_ack(
    forest: &ResidentForest<ExomonadHandlerStack, CapturedOutput>,
    store: &Store,
    run: &str,
    request: &Arc<DisplayPublication>,
    before_ack: impl FnOnce(),
) {
    publish_inner(forest, store, run, None, None, request, before_ack);
}

fn publish_inner(
    forest: &ResidentForest<ExomonadHandlerStack, CapturedOutput>,
    store: &Store,
    run: &str,
    conversation: Option<HostIdentity>,
    control: Option<&harness::server::ServerControl>,
    request: &Arc<DisplayPublication>,
    before_ack: impl FnOnce(),
) {
    let previous = request.outcome();
    if matches!(
        previous,
        Some(DisplayPublicationOutcome::Published(_) | DisplayPublicationOutcome::Refused(_))
    ) {
        return;
    }
    let result = (|| {
        let admission = forest
            .authorize_display_publication(request)
            .map_err(|error| StoreError::ActorOutputAuthority(error.to_string()))?;
        let context = request.admit_host_context(DisplayPublicationHostContext {
            run: run.to_owned(),
            conversation: conversation.map(|identity| DisplayConversationIdentity {
                run: identity.run,
                actor: identity.actor.0,
                incarnation: identity.incarnation,
            }),
        });
        if context.run != run {
            return Err(StoreError::ActorOutputRefused);
        }
        let actor = request.actor;
        let page = &request.page;
        if page.identity.0
            != i64::try_from(actor.id.0).map_err(|_| StoreError::InvalidActorOutput)?
            || page.identity.1
                != i64::try_from(actor.incarnation.0).map_err(|_| StoreError::InvalidActorOutput)?
        {
            return Err(StoreError::InvalidActorOutput);
        }
        let emission = ActorOutputEmission {
            origin: ActorOutputOrigin {
                run: context.run.clone(),
                native_actor: actor.id.0,
                incarnation: actor.incarnation.0,
            },
            id: ActorOutputId {
                display_slot: u64::try_from(page.identity.2)
                    .map_err(|_| StoreError::InvalidActorOutput)?,
                page_ordinal: request.page_ordinal,
            },
            execution: match request.operation.as_ref() {
                Some(operation) => ActorOutputExecution::Notebook {
                    execution: operation.execution.to_string(),
                    input_unit_index: operation.input_unit_index as u64,
                    effect_ordinal: operation.effect_ordinal as u64,
                },
                None => ActorOutputExecution::ActorProgram,
            },
            conversation: context.conversation.as_ref().map(|identity| {
                ConversationIdentity::Embedded {
                    run: identity.run.clone(),
                    actor: AgentPath(identity.actor.clone()),
                    incarnation: identity.incarnation.clone(),
                }
            }),
            page: ActorDisplayPage {
                text: page.text.clone(),
                expansions: page
                    .expansions
                    .iter()
                    .map(|(key, label)| {
                        u64::try_from(*key)
                            .map(|key| (key, label.clone()))
                            .map_err(|_| StoreError::InvalidActorOutput)
                    })
                    .collect::<Result<_, _>>()?,
                unavailable: page.unavailable,
                view: None,
            },
        };
        store.append_actor_output(
            &PublicationAuthority {
                forest,
                admission,
                request,
                expected: &emission,
            },
            &emission,
        )
    })();
    match result {
        Ok(commit) => {
            // Store's returned row is the only authority accepted by projection.
            if let Some(control) = control {
                // A reconciled existing row may have committed before its
                // original host attempt could project it. The stream owner
                // deduplicates this same canonical reference.
                control.publish_actor_output(commit.output());
            }
            before_ack();
            let reference = commit.output().reference();
            request.answer(DisplayPublicationOutcome::Published(
                tidepool_runtime::session::ActorOutputReference {
                    run: reference.origin.run.clone(),
                    sequence: reference.sequence,
                },
            ));
        }
        Err(error) => {
            let detail = error.to_string();
            let outcome = match error {
                _ if request.was_unconfirmed() => DisplayPublicationOutcome::Unconfirmed(detail),
                StoreError::InvalidActorOutput
                | StoreError::ActorOutputRefused
                | StoreError::ActorOutputAuthority(_)
                | StoreError::ConflictingActorOutput
                | StoreError::ActorOutputNeedsAuthority => {
                    DisplayPublicationOutcome::Refused(detail)
                }
                // A Store commit error does not prove that the page was rolled back.
                _ => DisplayPublicationOutcome::Unconfirmed(detail),
            };
            request.answer(outcome);
        }
    }
}
