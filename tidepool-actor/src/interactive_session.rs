//! Captured typed boundary for one supervised external-agent session.

use tidepool_bridge::FromCore;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_eval::Value;
use tidepool_repr::DataConTable;
use tidepool_runtime::session::{OutputSink, ResidentHole, ResidentSession, RootCustody};

use crate::completion::{decode_typed_session_site, CompletionRequestError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveSessionRequest {
    pub initial_user_message: Option<String>,
    pub input_type: String,
    pub input_modules: Vec<String>,
    pub output_type: String,
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
    Site(#[from] CompletionRequestError),
    #[error("interactive agent session suspended without its declared live input")]
    MissingInput,
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
        let crate::generated::agent_session::AgentSessionReq::AgentSessionWith(
            site,
            _input,
            initial_user_message,
        ) = crate::generated::agent_session::AgentSessionReq::from_value(request, table)?;
        let sites = session.parked_program_provenance(&hole).unwrap_or_default();
        let signature = decode_typed_session_site(site, &sites.sites())?;
        let input = session
            .live_payload_handle_owned_by(hole.cont_id(), actor_realm)
            .ok_or(InteractiveSessionCaptureError::MissingInput)?;
        Ok(Self {
            request: InteractiveSessionRequest {
                initial_user_message,
                input_type: signature.input_type,
                input_modules: signature.input_modules,
                output_type: signature.output_type,
                output_modules: signature.output_modules,
            },
            hole,
            input,
        })
    }
}
