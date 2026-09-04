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
        input_type: String,
        request_message: Option<&str>,
    ) -> Self {
        let message = request_message.map_or_else(
            || format!("A new typed request is ready as `sessionInput :: {input_type}`."),
            |message| {
                format!("{message}\n\nThe authoritative request input is mounted as `sessionInput :: {input_type}`.")
            },
        );
        Self {
            id: ActivationId { actor, sequence },
            request,
            input_type,
            message,
        }
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
            "Candidate".into(),
            Some("Review this candidate."),
        );
        assert_eq!(
            activation.message,
            "Review this candidate.\n\nThe authoritative request input is mounted as `sessionInput :: Candidate`."
        );
        assert_eq!(activation.request, crate::RequestId(11));
    }
}
