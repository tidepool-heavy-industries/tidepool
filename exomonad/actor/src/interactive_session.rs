//! Captured typed boundary for one supervised external-agent session.

use tidepool_bridge::FromHaskell;
use tidepool_bridge::HaskellValue;
use tidepool_effect::dispatch::DispatchEffect;
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

/// One other branch admitted in the same `unfold` call as this activation's
/// target, as reported by `Tidepool.Actors.Unfold.requestBranch` before this
/// request was sent. Empty for any request outside `unfold`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiblingPreview {
    pub label: String,
    pub path: String,
    pub preview: String,
}

impl From<(String, String, String)> for SiblingPreview {
    fn from((label, path, preview): (String, String, String)) -> Self {
        Self {
            label,
            path,
            preview,
        }
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
    pub input_preview: String,
    pub reply_preview: String,
    pub siblings: Vec<SiblingPreview>,
}

impl ActivationContract {
    pub(crate) fn message(&self, request: crate::RequestId, guidance: Option<&str>) -> String {
        let progress = self
            .response
            .progress_type
            .as_ref()
            .map_or_else(|| "\nThis request has no progress stream; `reportProgress` is unavailable. Request-local bindings from inherited history do not apply.".to_owned(), |ty| {
                format!("\nProgress updates for this request: `reportProgress` accepts {ty}.")
            });
        let reply_type = self.response.expected_type();
        let siblings = self.siblings_block();
        format!(
            "Request {}{}\n\nAssignment (available as `sessionInput :: {}`):\n\n{}{}\n\nReturn with `respond` (reply type {reply_type}). It takes exactly one argument, the reply value itself: `respond (… :: {reply_type})`.\n{}{}",
            request.0,
            guidance.map(|text| format!(": {text}")).unwrap_or_default(),
            self.input_type,
            self.input_preview,
            siblings,
            self.reply_preview,
            progress
        )
    }

    /// "Siblings admitted with you (n):" plus one line per sibling, or empty
    /// when this activation admitted none. Placed right after the assignment
    /// so a child sees who else arrived in the same `unfold` before it reads
    /// its own reply obligation.
    fn siblings_block(&self) -> String {
        if self.siblings.is_empty() {
            return String::new();
        }
        let mut block = format!("\n\nSiblings admitted with you ({}):", self.siblings.len());
        for sibling in &self.siblings {
            block.push_str(&format!(
                "\n- {} ({}): {}",
                sibling.label, sibling.path, sibling.preview
            ));
        }
        block
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
    pub siblings: Vec<SiblingPreview>,
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
/// owns no duplicate input handle.
pub(crate) struct ResidentInteractiveAwait {
    pub(crate) request: InteractiveSessionRequest,
    pub(crate) hole: ResidentHole,
}

#[derive(Debug, thiserror::Error)]
pub enum InteractiveSessionCaptureError {
    #[error(transparent)]
    Resident(#[from] tidepool_runtime::session::ResidentError),
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
        request: &HaskellValue,
        table: &DataConTable,
        actor_realm: tidepool_codegen::suspension::RealmId,
    ) -> Result<Self, InteractiveSessionCaptureError>
    where
        H: DispatchEffect<O> + Send,
        O: OutputSink + Sync,
    {
        let (site, request_id, initial_user_message, siblings) =
            match crate::generated::agent_session::AgentSessionReq::from_value(request, table)? {
                crate::generated::agent_session::AgentSessionReq::AgentSessionWith(
                    site,
                    _input,
                    request_id,
                    initial_user_message,
                    siblings,
                ) => (
                    site,
                    request_id,
                    initial_user_message,
                    siblings.into_iter().map(SiblingPreview::from).collect(),
                ),
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
            .live_payload_handle_owned_by(hole.cont_id(), actor_realm)?
            .ok_or(InteractiveSessionCaptureError::MissingInput)?;
        Ok(Self {
            request: InteractiveSessionRequest {
                request,
                initial_user_message,
                input_type: signature.input_type,
                input_modules: signature.input_modules,
                response: signature.response,
                output_modules: signature.output_modules,
                siblings,
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
                input_preview: "candidate".into(),
                reply_preview: "data Review = Accepted | Rejected".into(),
                siblings: vec![
                    SiblingPreview {
                        label: "worker-a".into(),
                        path: "exomonad/wave/worker-a".into(),
                        preview:
                            "CodingTask { ownedPaths = [\"src/a.rs\"], obligation = \"add a\" }"
                                .into(),
                    },
                    SiblingPreview {
                        label: "worker-b".into(),
                        path: "exomonad/wave/worker-b".into(),
                        preview:
                            "CodingTask { ownedPaths = [\"src/b.rs\"], obligation = \"add b\" }"
                                .into(),
                    },
                ],
            },
            Some("Review this candidate."),
        );
        assert_eq!(
            activation.message,
            "Request 11: Review this candidate.\n\nAssignment (available as `sessionInput :: Candidate`):\n\ncandidate\n\nSiblings admitted with you (2):\n- worker-a (exomonad/wave/worker-a): CodingTask { ownedPaths = [\"src/a.rs\"], obligation = \"add a\" }\n- worker-b (exomonad/wave/worker-b): CodingTask { ownedPaths = [\"src/b.rs\"], obligation = \"add b\" }\n\nReturn with `respond` (reply type Review). It takes exactly one argument, the reply value itself: `respond (… :: Review)`.\ndata Review = Accepted | Rejected\nThis request has no progress stream; `reportProgress` is unavailable. Request-local bindings from inherited history do not apply."
        );
        assert_eq!(activation.request, crate::RequestId(11));
    }
}
