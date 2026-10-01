//! Compose the admitted cell's ModelCall service from the embedded run owners.
use std::{marker::PhantomData, sync::Arc};

use exomonad_actor::{ActorDescriptor, CellModelBinding, CellModelFactory, ForkEffort, ModelReq};
use harness::{
    engine::ResponsesTransport,
    invocation::Limits,
    model::Effort,
    store::Store,
    transport::{auth::CodexFileAuth, Auth, ResponsesClient},
    turn::JobScheduler,
};
use tidepool_effect::{DeferredEffect, Response};
use tidepool_handlers::handlers::model::ModelService;
use tidepool_repr::{DataConTable, PrincipalId};
use tidepool_runtime::session::WorkbenchExecutionId;

use super::{embedded_service::EmbeddedService, ActorHostConfig};
use crate::{
    exomonad::EmbeddedLaunchConfig,
    model_turn::{CellModelService, ModelPolicy},
};

pub(super) struct EmbeddedCellModelFactory<A, C> {
    runtime: tokio::runtime::Handle,
    store: Arc<Store>,
    scheduler: Arc<JobScheduler>,
    default_model: String,
    default_effort: Effort,
    launch_config: Option<Arc<ActorHostConfig>>,
    transport: Arc<dyn Fn() -> C + Send + Sync>,
    auth: PhantomData<fn() -> A>,
}

impl<A: Auth + 'static, C: ResponsesTransport + 'static> EmbeddedCellModelFactory<A, C> {
    pub(super) fn new(
        store: Arc<Store>,
        scheduler: Arc<JobScheduler>,
        default_model: String,
        default_effort: Effort,
        transport: Arc<dyn Fn() -> C + Send + Sync>,
    ) -> Self {
        Self {
            runtime: tokio::runtime::Handle::current(),
            store,
            scheduler,
            default_model,
            default_effort,
            launch_config: None,
            transport,
            auth: PhantomData,
        }
    }
    fn policy(&self, descriptor: &ActorDescriptor) -> Result<ModelPolicy, String> {
        let model = match descriptor.model() {
            Some(model) => match self.launch_config.as_ref() {
                Some(config) => super::resolve_model(config, model),
                None => match model {
                    exomonad_actor::Model::Literal(model) => Ok(model.clone()),
                    exomonad_actor::Model::Alias(alias) => Err(format!(
                        "model alias {alias} has no admitted workspace policy"
                    )),
                },
            },
            None => Ok(self.default_model.clone()),
        };
        let model = model?;
        let effort = match descriptor.fork_effort() {
            Some(ForkEffort::Low) => Effort::Low,
            Some(ForkEffort::Medium) => Effort::Medium,
            Some(ForkEffort::High) => Effort::High,
            None => self.default_effort,
        };
        Ok(ModelPolicy {
            default_model: model.clone(),
            models: vec![model],
            default_effort: effort,
            efforts: vec![effort],
            limits: Limits::default(),
        })
    }
    fn with_launch_config(mut self, config: &ActorHostConfig) -> Self {
        self.launch_config = Some(Arc::new(config.clone()));
        self
    }
}

impl<A: Auth + 'static, C: ResponsesTransport + 'static> CellModelFactory
    for EmbeddedCellModelFactory<A, C>
{
    fn bind(
        &self,
        execution: &WorkbenchExecutionId,
        principal: PrincipalId,
        descriptor: &ActorDescriptor,
    ) -> Arc<dyn CellModelBinding> {
        let policy = match self.policy(descriptor) {
            Ok(policy) => policy,
            Err(reason) => return Arc::new(RefusedCellModelBinding(reason)),
        };
        Arc::new(EmbeddedCellModelBinding(Arc::new(
            CellModelService::<A, C>::new(
                self.runtime.clone(),
                principal,
                execution.as_str().to_owned(),
                self.store.clone(),
                self.scheduler.clone(),
                policy,
                self.transport.clone(),
            ),
        )))
    }
}

struct EmbeddedCellModelBinding<A, C>(Arc<CellModelService<A, C>>);
impl<A: Auth + 'static, C: ResponsesTransport + 'static> CellModelBinding
    for EmbeddedCellModelBinding<A, C>
{
    fn prepare(
        &self,
        request: ModelReq,
        principal: PrincipalId,
        table: DataConTable,
    ) -> DeferredEffect {
        let service = self.0.clone();
        DeferredEffect::blocking(move || {
            let json = |value: &tidepool_bridge::HaskellValue| {
                tidepool_runtime::value_to_json(value, &table, 0)
            };
            // Responses retain the schema-owned failure and original result shape.
            // The actor resumes the original hole under its observed constructor table.
            Ok(match request {
                ModelReq::ModelStartWith(request) => {
                    Response::new(service.start(principal, json(&request)))
                }
                ModelReq::ModelResumeWith(invocation, call, answer) => {
                    Response::new(service.resume(principal, &invocation, &call, json(&answer)))
                }
                ModelReq::ModelAnnotateWith(invocation, operation, annotation) => Response::new(
                    service.annotate(principal, &invocation, &operation, json(&annotation)),
                ),
                ModelReq::ModelCloseWith(invocation) => {
                    Response::new(service.close(principal, &invocation))
                }
            })
        })
    }

    fn cancel(&self) {
        self.0.cancel();
    }
    fn settle(
        &self,
    ) -> futures_util::future::BoxFuture<'_, Result<(), tidepool_effect::error::EffectError>> {
        let service = self.0.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || service.settle())
                .await
                .map_err(|error| tidepool_effect::error::EffectError::Handler(error.to_string()))?
                .map_err(|error| {
                    tidepool_effect::error::EffectError::Handler(format!(
                        "model settlement failed: {error:?}"
                    ))
                })
        })
    }
}

struct RefusedCellModelBinding(String);
impl CellModelBinding for RefusedCellModelBinding {
    fn prepare(&self, request: ModelReq, _: PrincipalId, _: DataConTable) -> DeferredEffect {
        let reason = self.0.clone();
        DeferredEffect::blocking(move || {
            let error =
                tidepool_handlers::handlers::model::ModelBoundaryError::ModelRejected(reason);
            Ok(match request {
                ModelReq::ModelCloseWith(_) => Response::new(Err::<(), _>(error)),
                _ => Response::new(Err::<serde_json::Value, _>(error)),
            })
        })
    }
    fn cancel(&self) {}
    fn settle(
        &self,
    ) -> futures_util::future::BoxFuture<'_, Result<(), tidepool_effect::error::EffectError>> {
        Box::pin(async { Ok(()) })
    }
}

pub(super) fn admitted_factory(
    service: &EmbeddedService,
    settings: &EmbeddedLaunchConfig,
    config: &ActorHostConfig,
) -> Arc<dyn CellModelFactory> {
    let effort = match config.effort {
        exomonad_agent::ReasoningEffort::Low => Effort::Low,
        exomonad_agent::ReasoningEffort::Medium => Effort::Medium,
        exomonad_agent::ReasoningEffort::High => Effort::High,
    };
    #[cfg(test)]
    if let Some(transport) = service.test_transport() {
        return Arc::new(
            EmbeddedCellModelFactory::<CodexFileAuth, _>::new(
                service.runtime.store(),
                service.runtime.scheduler(),
                config.model.clone(),
                effort,
                Arc::new(move || transport.clone()),
            )
            .with_launch_config(config),
        );
    }
    let auth_file = settings.codex_auth_file.clone();
    Arc::new(
        EmbeddedCellModelFactory::<CodexFileAuth, _>::new(
            service.runtime.store(),
            service.runtime.scheduler(),
            config.model.clone(),
            effort,
            Arc::new(move || ResponsesClient::new(CodexFileAuth::new(auth_file.clone()))),
        )
        .with_launch_config(config),
    )
}

#[cfg(test)]
mod tests;
