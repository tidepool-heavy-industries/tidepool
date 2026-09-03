//! Completion capture and settlement for the public Haskell `deliberate` effect.
//!
//! The request's input field is decoded only to validate constructor shape.
//! Its authoritative heap value is claimed separately from the parked frame's
//! live-payload root and mounted as `goalInput` by the resident workbench.

use std::sync::Arc;

use tidepool_bridge::{BridgeError, FromCore};
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_eval::Value;
use tidepool_model::{DynModelProvider, StreamSink};
use tidepool_repr::DataConTable;
use tidepool_runtime::session::{
    OutputSink, ResidentHole, ResidentOutcome, ResidentSession, RootCustody,
};
use tidepool_runtime::YieldSite;

use crate::generated::deliberate::DeliberateReq;
use crate::{
    run_result_session, ActorMachineRegistry, ActorWorkbenchSource, AgentExecutionError,
    CompletionExpectation, ResidentActorWorkbench, ResidentActorWorkbenchError,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionRequest {
    pub task: String,
    pub input_type: String,
    pub input_modules: Vec<String>,
    pub completion: CompletionExpectation,
    pub output_modules: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypedSessionSignature {
    pub(crate) input_type: String,
    pub(crate) input_modules: Vec<String>,
    pub(crate) completion: CompletionExpectation,
    pub(crate) output_modules: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum CompletionRequestError {
    #[error(transparent)]
    Decode(#[from] BridgeError),
    #[error("completion request carried invalid site id {0}")]
    InvalidSite(i64),
    #[error("completion site {0} is absent from its GHC metadata")]
    MissingSite(u64),
    #[error("completion site {site} describes {actual} live input types, expected one")]
    InputArity { site: u64, actual: usize },
}

impl CompletionRequest {
    pub fn decode(
        request: &Value,
        table: &DataConTable,
        sites: &[YieldSite],
    ) -> Result<Self, CompletionRequestError> {
        let DeliberateReq::DeliberateWith(site, _input, task) =
            DeliberateReq::from_value(request, table)?;
        let signature = decode_typed_session_site(site, sites)?;
        Ok(Self {
            task,
            input_type: signature.input_type,
            input_modules: signature.input_modules,
            completion: signature.completion,
            output_modules: signature.output_modules,
        })
    }
}

pub(crate) fn decode_typed_session_site(
    site: i64,
    sites: &[YieldSite],
) -> Result<TypedSessionSignature, CompletionRequestError> {
    let site = u64::try_from(site).map_err(|_| CompletionRequestError::InvalidSite(site))?;
    let metadata = sites
        .iter()
        .find(|metadata| metadata.site == site)
        .ok_or(CompletionRequestError::MissingSite(site))?;
    let [input] = metadata.inputs.as_slice() else {
        return Err(CompletionRequestError::InputArity {
            site,
            actual: metadata.inputs.len(),
        });
    };
    Ok(TypedSessionSignature {
        input_type: input.ty.clone(),
        input_modules: input.modules.clone(),
        completion: CompletionExpectation::new(metadata.ty.clone()),
        output_modules: metadata.modules.clone(),
    })
}

/// One exact authored-Haskell continuation parked on `deliberate`, paired
/// with exclusive custody of the request's live input. This is the complete
/// obligation crossing into the model/Haskell executor: neither the input nor
/// the continuation can be reconstructed from bridged metadata.
pub struct ResidentCompletion {
    request: CompletionRequest,
    hole: ResidentHole,
    input: RootCustody,
}

#[derive(Debug, thiserror::Error)]
pub enum CompletionCaptureError {
    #[error(transparent)]
    Decode(#[from] CompletionRequestError),
    #[error("result-bearing agent session suspended without its declared live input")]
    MissingInput,
}

impl ResidentCompletion {
    /// Decode and claim a newly suspended `deliberate` request while its
    /// resident machine is checked out. The input is immediately rehomed to
    /// the actor realm so disposable execution scopes cannot invalidate it.
    pub fn capture<H, O>(
        session: &mut ResidentSession<H, O>,
        hole: ResidentHole,
        request: &Value,
        table: &DataConTable,
        actor_realm: tidepool_codegen::suspension::RealmId,
    ) -> Result<Self, CompletionCaptureError>
    where
        H: DispatchEffect<O> + Send,
        O: OutputSink + Sync,
    {
        let sites = session.parked_program_provenance(&hole).unwrap_or_default();
        let decoded = CompletionRequest::decode(request, table, &sites.sites())?;
        let input = session
            .live_payload_handle_owned_by(hole.cont_id(), actor_realm)
            .ok_or(CompletionCaptureError::MissingInput)?;
        Ok(Self {
            request: decoded,
            hole,
            input,
        })
    }

    #[must_use]
    pub fn request(&self) -> &CompletionRequest {
        &self.request
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ResidentCompletionError {
    #[error(transparent)]
    Workbench(#[from] ResidentActorWorkbenchError),
    #[error(transparent)]
    Execution(#[from] AgentExecutionError<ResidentActorWorkbenchError>),
}

/// Shared resident dependencies for resolving typed completion obligations.
/// The executor owns no actor turn and keeps no machine checked out between
/// calls; one instance can therefore serve every actor using the same machine
/// registry and trusted Haskell source facade.
pub struct ResidentCompletionExecutor<H, O> {
    machines: Arc<ActorMachineRegistry<H, O>>,
    source: ActorWorkbenchSource,
    max_tokens: Option<u32>,
}

impl<H, O> Clone for ResidentCompletionExecutor<H, O> {
    fn clone(&self) -> Self {
        Self {
            machines: Arc::clone(&self.machines),
            source: self.source.clone(),
            max_tokens: self.max_tokens,
        }
    }
}

impl<H, O> ResidentCompletionExecutor<H, O> {
    #[must_use]
    pub fn new(machines: Arc<ActorMachineRegistry<H, O>>, source: ActorWorkbenchSource) -> Self {
        Self {
            machines,
            source,
            max_tokens: None,
        }
    }

    #[must_use]
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }
}

impl<H, O> ResidentCompletionExecutor<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(crate) async fn resolve_admitted(
        &self,
        admitted: &mut crate::AdmittedAgentSession,
        provider: &dyn DynModelProvider,
        completion: ResidentCompletion,
        sink: Option<StreamSink>,
    ) -> Result<ResidentOutcome, ResidentCompletionError> {
        let ResidentCompletion {
            request,
            hole,
            input,
        } = completion;
        let mut type_modules = request.input_modules.clone();
        for module in &request.output_modules {
            if !type_modules.contains(module) {
                type_modules.push(module.clone());
            }
        }
        let completion = request.completion;
        let mut workbench = ResidentActorWorkbench::new(
            Arc::clone(&self.machines),
            self.source.clone(),
            completion.clone(),
            type_modules,
        );
        workbench
            .mount_goal_input(admitted, request.input_type, input)
            .await?;
        let answer = run_result_session(
            admitted,
            provider,
            &mut workbench,
            request.task,
            completion,
            self.max_tokens,
            sink,
        )
        .await?;
        let outcome = workbench
            .resume_completion(admitted.session_context(), hole, answer)
            .await?;
        Ok(outcome)
    }
}
