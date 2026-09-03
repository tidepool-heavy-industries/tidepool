//! Captured typed boundary for one supervised external-agent session.

use tidepool_bridge::FromCore;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_eval::Value;
use tidepool_repr::DataConTable;
use tidepool_runtime::session::{OutputSink, ResidentHole, ResidentSession, RootCustody};

use crate::completion::{decode_typed_session_site, CompletionRequestError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationReason {
    InitialUser,
    ActionCompleted,
    ActionFailed,
    ManualReady,
}

impl TryFrom<i64> for ActivationReason {
    type Error = InteractiveSessionCaptureError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::InitialUser),
            1 => Ok(Self::ActionCompleted),
            2 => Ok(Self::ActionFailed),
            3 => Ok(Self::ManualReady),
            _ => Err(InteractiveSessionCaptureError::InvalidActivationReason(
                value,
            )),
        }
    }
}

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
    pub reason: ActivationReason,
    pub input_type: String,
    pub message: String,
}

impl ResidentActivation {
    pub(crate) fn mounted(
        actor: crate::ActorRef,
        sequence: u64,
        reason: ActivationReason,
        input_type: String,
    ) -> Option<Self> {
        let message = match reason {
            ActivationReason::ActionCompleted => format!(
                "Your Haskell continuation completed. Its typed result is mounted as `sessionInput :: {input_type}`; continue the program from that value."
            ),
            ActivationReason::ActionFailed => format!(
                "Your returned Haskell action stopped at an actor lifecycle failure. The typed failure is mounted as `sessionInput :: {input_type}`; decide the next program explicitly."
            ),
            ActivationReason::InitialUser | ActivationReason::ManualReady => return None,
        };
        Some(Self {
            id: ActivationId { actor, sequence },
            reason,
            input_type,
            message,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveSessionRequest {
    pub initial_user_message: Option<String>,
    pub input_type: String,
    pub input_modules: Vec<String>,
    pub output_type: String,
    pub output_modules: Vec<String>,
    pub activation_reason: ActivationReason,
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
    #[error("interactive agent session carried invalid activation reason {0}")]
    InvalidActivationReason(i64),
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
            activation_reason,
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
                activation_reason: activation_reason.try_into()?,
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
    fn successful_activation_names_the_exact_mounted_type() {
        let activation = ResidentActivation::mounted(
            actor(),
            1,
            ActivationReason::ActionCompleted,
            "Either WorktreeError WorktreeHandle".into(),
        )
        .unwrap();
        assert_eq!(activation.id.sequence(), 1);
        assert!(activation
            .message
            .contains("sessionInput :: Either WorktreeError WorktreeHandle"));
        assert!(!activation.message.contains("failure"));
    }

    #[test]
    fn failed_activation_names_the_just_failure_mount() {
        let activation = ResidentActivation::mounted(
            actor(),
            2,
            ActivationReason::ActionFailed,
            "Maybe ActionFailure".into(),
        )
        .unwrap();
        assert!(activation.message.contains("lifecycle failure"));
        assert!(activation
            .message
            .contains("sessionInput :: Maybe ActionFailure"));
    }

    #[test]
    fn initial_and_manual_readiness_do_not_manufacture_wakes() {
        for reason in [ActivationReason::InitialUser, ActivationReason::ManualReady] {
            assert!(
                ResidentActivation::mounted(actor(), 1, reason, "Maybe ActionFailure".into(),)
                    .is_none()
            );
        }
    }
}
