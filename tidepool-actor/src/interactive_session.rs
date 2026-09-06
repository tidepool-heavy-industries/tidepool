//! Captured typed boundary for one supervised external-agent session.

use tidepool_bridge::FromCore;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_eval::Value;
use tidepool_repr::DataConTable;
use tidepool_runtime::session::{OutputSink, ResidentHole, ResidentSession, RootCustody};

use crate::typed_request::decode_typed_request_site;
use crate::ResponseExpectation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActivationId {
    actor: crate::ActorRef,
    sequence: u64,
}

impl ActivationId {
    #[must_use]
    pub fn actor(self) -> crate::ActorRef {
        self.actor
    }

    #[must_use]
    pub fn sequence(self) -> u64 {
        self.sequence
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentActivation {
    pub id: ActivationId,
    pub request: crate::RequestId,
    pub input_type: String,
    pub message: String,
}

impl ResidentActivation {
    pub(crate) fn mounted(
        actor: crate::ActorRef,
        sequence: u64,
        request: crate::RequestId,
        contract: ActivationContract,
        request_message: Option<&str>,
    ) -> Self {
        let message = contract.message(request, request_message);
        Self {
            id: ActivationId { actor, sequence },
            request,
            input_type: contract.input_type,
            message,
        }
    }
}

pub(crate) struct ActivationContract {
    pub input_type: String,
    pub response: ResponseExpectation,
    pub effects: String,
}

impl ActivationContract {
    pub(crate) fn message(&self, request: crate::RequestId, guidance: Option<&str>) -> String {
        let progress = self
            .response
            .progress_type
            .as_ref()
            .map_or(String::new(), |ty| {
                format!("\nreportProgress :: ({ty}) -> Eff {} ()", self.effects)
            });
        format!("{}\n\nTyped request {} mounted:\n```haskell\nsessionInput :: {}\nsessionReply :: Reply ({})\n{}\n```\nRead `sessionInput` when its value is needed; use `:info` only for unfamiliar types.",
            guidance.unwrap_or("A new typed request is ready."), request.0, self.input_type, self.response.expected_type(),
            format!("{}{progress}", self.response.respond_signature(&self.effects)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveSessionRequest {
    pub request: crate::RequestId,
    pub initial_user_message: Option<String>,
    pub input_type: String,
    pub input_modules: Vec<String>,
    pub response: ResponseExpectation,
    pub output_modules: Vec<String>,
}

impl InteractiveSessionRequest {
    /// Both sides of a retained request can introduce types after the target
    /// was forked. Mount their compiler dependencies without changing the
    /// target's inherited declaration generations.
    pub(crate) fn type_modules(&self) -> Vec<String> {
        let mut modules = self.input_modules.clone();
        modules.extend(self.output_modules.iter().cloned());
        modules.sort();
        modules.dedup();
        modules
    }
}

pub struct ResidentInteractiveSession {
    pub(crate) request: InteractiveSessionRequest,
    pub(crate) hole: ResidentHole,
    pub(crate) input: RootCustody,
}

/// Parked fixed-program continuation after its authoritative input has been
/// mounted into the actor workbench. Unlike the captured boundary, this value
/// owns no duplicate input custody.
pub(crate) struct ResidentInteractiveAwait {
    pub(crate) request: InteractiveSessionRequest,
    pub(crate) hole: ResidentHole,
}

#[derive(Debug, thiserror::Error)]
pub enum InteractiveSessionCaptureError {
    #[error(transparent)]
    Decode(#[from] tidepool_bridge::BridgeError),
    #[error(transparent)]
    Site(#[from] crate::RequestSignatureError),
    #[error("interactive agent session suspended without its declared live input")]
    MissingInput,
    #[error("interactive agent session carried invalid request id {0}")]
    InvalidRequestId(i64),
}

impl ResidentInteractiveSession {
    pub(crate) fn into_parts(self) -> (InteractiveSessionRequest, ResidentHole, RootCustody) {
        (self.request, self.hole, self.input)
    }

    pub(crate) fn capture<H, O>(
        session: &mut ResidentSession<H, O>,
        hole: ResidentHole,
        request: &Value,
        table: &DataConTable,
        actor_realm: tidepool_codegen::suspension::RealmId,
    ) -> Result<Self, InteractiveSessionCaptureError>
    where
        H: DispatchEffect<O> + Send,
        O: OutputSink + Sync,
    {
        let (site, request_id, initial_user_message) =
            match crate::generated::agent_session::AgentSessionReq::from_value(request, table)? {
                crate::generated::agent_session::AgentSessionReq::AgentSessionWith(
                    site,
                    _input,
                    request_id,
                    initial_user_message,
                ) => (site, request_id, initial_user_message),
                crate::generated::agent_session::AgentSessionReq::AgentAttachWith(..) => {
                    unreachable!("agent attachment is classified before session capture")
                }
            };
        let sites = session.parked_program_provenance(&hole).unwrap_or_default();
        let signature = decode_typed_request_site(site, &sites.sites())?;
        let request = u64::try_from(request_id)
            .map(crate::RequestId)
            .map_err(|_| InteractiveSessionCaptureError::InvalidRequestId(request_id))?;
        let input = session
            .live_payload_handle_owned_by(hole.cont_id(), actor_realm)
            .ok_or(InteractiveSessionCaptureError::MissingInput)?;
        Ok(Self {
            request: InteractiveSessionRequest {
                request,
                initial_user_message,
                input_type: signature.input_type,
                input_modules: signature.input_modules,
                response: signature.response,
                output_modules: signature.output_modules,
            },
            hole,
            input,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActorId, ActorRef};

    fn actor() -> ActorRef {
        ActorRef::first(ActorId(7))
    }

    #[test]
    fn request_activation_carries_prompt_and_exact_input_type() {
        let activation = ResidentActivation::mounted(
            actor(),
            3,
            crate::RequestId(11),
            ActivationContract {
                input_type: "Candidate".into(),
                response: ResponseExpectation::new("Review"),
                effects: "ActorEffects".into(),
            },
            Some("Review this candidate."),
        );
        assert_eq!(
            activation.message,
            "Review this candidate.\n\nTyped request 11 mounted:\n```haskell\nsessionInput :: Candidate\nsessionReply :: Reply (Review)\nrespond :: (Review) -> Eff ActorEffects TidepoolVoid.Void\n```\nRead `sessionInput` when its value is needed; use `:info` only for unfamiliar types."
        );
        assert_eq!(activation.request, crate::RequestId(11));
    }
}
