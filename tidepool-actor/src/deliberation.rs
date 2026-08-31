//! Decoding and settlement metadata for the public Haskell `deliberate` effect.
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
    run_typed_deliberation, ActorAgentSession, ActorMachineRegistry, ActorRegistryError,
    ActorWorkbenchSource, AgentExecutionError, ResidentActorWorkbench, ResidentActorWorkbenchError,
    TurnLease, TypedGoal,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliberationRequest {
    pub task: String,
    pub input_type: String,
    pub input_modules: Vec<String>,
    pub output_type: String,
    pub output_modules: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum DeliberationRequestError {
    #[error(transparent)]
    Decode(#[from] BridgeError),
    #[error("deliberation request carried invalid site id {0}")]
    InvalidSite(i64),
    #[error("deliberation site {0} is absent from its GHC metadata")]
    MissingSite(u32),
    #[error("deliberation site {site} describes {actual} live input types, expected one")]
    InputArity { site: u32, actual: usize },
}

impl DeliberationRequest {
    pub fn decode(
        request: &Value,
        table: &DataConTable,
        sites: &[YieldSite],
    ) -> Result<Self, DeliberationRequestError> {
        let DeliberateReq::DeliberateWith(site, _input, task) =
            DeliberateReq::from_value(request, table)?;
        let site = u32::try_from(site).map_err(|_| DeliberationRequestError::InvalidSite(site))?;
        let metadata = sites
            .iter()
            .find(|metadata| metadata.site == site)
            .ok_or(DeliberationRequestError::MissingSite(site))?;
        let [input] = metadata.inputs.as_slice() else {
            return Err(DeliberationRequestError::InputArity {
                site,
                actual: metadata.inputs.len(),
            });
        };
        Ok(Self {
            task,
            input_type: input.ty.clone(),
            input_modules: input.modules.clone(),
            output_type: metadata.ty.clone(),
            output_modules: metadata.modules.clone(),
        })
    }
}

/// One exact authored-Haskell continuation parked on `deliberate`, paired
/// with exclusive custody of the request's live input. This is the complete
/// obligation crossing into the model/Haskell executor: neither the input nor
/// the continuation can be reconstructed from bridged metadata.
pub struct ResidentDeliberation {
    request: DeliberationRequest,
    hole: ResidentHole,
    input: RootCustody,
}

#[derive(Debug, thiserror::Error)]
pub enum DeliberationCaptureError {
    #[error(transparent)]
    Decode(#[from] DeliberationRequestError),
    #[error("typed deliberation suspended without its declared live input")]
    MissingInput,
}

impl ResidentDeliberation {
    /// Decode and claim a newly suspended `deliberate` request while its
    /// resident machine is checked out. The input is immediately rehomed to
    /// the actor realm so disposable execution scopes cannot invalidate it.
    pub fn capture<H, O>(
        session: &mut ResidentSession<H, O>,
        hole: ResidentHole,
        request: &Value,
        table: &DataConTable,
        sites: &[YieldSite],
        actor_realm: tidepool_codegen::suspension::RealmId,
    ) -> Result<Self, DeliberationCaptureError>
    where
        H: DispatchEffect<O> + Send,
        O: OutputSink + Sync,
    {
        let decoded = DeliberationRequest::decode(request, table, sites)?;
        let input = session
            .live_payload_handle_owned_by(hole.cont_id(), actor_realm)
            .ok_or(DeliberationCaptureError::MissingInput)?;
        Ok(Self {
            request: decoded,
            hole,
            input,
        })
    }

    #[must_use]
    pub fn request(&self) -> &DeliberationRequest {
        &self.request
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ResidentDeliberationError {
    #[error(transparent)]
    Admission(#[from] ActorRegistryError),
    #[error(transparent)]
    Workbench(#[from] ResidentActorWorkbenchError),
    #[error(transparent)]
    Execution(#[from] AgentExecutionError<ResidentActorWorkbenchError>),
}

/// Shared resident dependencies for resolving typed actor deliberations.
/// The executor owns no actor turn and keeps no machine checked out between
/// calls; one instance can therefore serve every actor using the same machine
/// registry and trusted Haskell source facade.
pub struct ResidentDeliberationExecutor<H, O> {
    machines: Arc<ActorMachineRegistry<H, O>>,
    source: ActorWorkbenchSource,
    max_tokens: Option<u32>,
}

impl<H, O> ResidentDeliberationExecutor<H, O> {
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

impl<H, O> ResidentDeliberationExecutor<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    /// Resolve one public `deliberate` suspension and resume the exact
    /// authored Haskell continuation. Actor admission transitions Haskell →
    /// agent session → Haskell without a release gap; the resident machine is
    /// checked out only for input mounting, fenced execution, and final
    /// continuation resumption.
    pub async fn resolve(
        &self,
        agent: &ActorAgentSession,
        haskell_turn: TurnLease,
        provider: &dyn DynModelProvider,
        deliberation: ResidentDeliberation,
        sink: Option<StreamSink>,
    ) -> Result<(TurnLease, ResidentOutcome), ResidentDeliberationError> {
        let ResidentDeliberation {
            request,
            hole,
            input,
        } = deliberation;
        let mut admitted = agent.enter_from_haskell_turn(haskell_turn)?;
        let mut type_modules = request.input_modules.clone();
        for module in &request.output_modules {
            if !type_modules.contains(module) {
                type_modules.push(module.clone());
            }
        }
        let mut workbench = ResidentActorWorkbench::new(
            Arc::clone(&self.machines),
            self.source.clone(),
            request.output_type.clone(),
            type_modules,
        );
        workbench
            .mount_goal_input(&admitted, request.input_type, input)
            .await?;
        let answer = run_typed_deliberation(
            &mut admitted,
            provider,
            &mut workbench,
            TypedGoal::new(request.task, request.output_type),
            self.max_tokens,
            sink,
        )
        .await?;
        let haskell_turn = admitted.return_to_haskell()?;
        let outcome = workbench
            .resume_deliberation(haskell_turn.session_context(), hole, answer)
            .await?;
        Ok((haskell_turn, outcome))
    }
}
