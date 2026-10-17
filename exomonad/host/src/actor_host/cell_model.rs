//! Compose the admitted cell's ModelCall service from the embedded run owners.
use std::{marker::PhantomData, sync::Arc};

use super::run_auth::RunAuth;
use exomonad_actor::{
    ActorDescriptor, CellModelBinding, CellModelFactory, Model, ModelReq, Reasoning,
};
use harness::{
    engine::ResponsesTransport, invocation::Limits, model::Effort, store::Store, transport::Auth,
    turn::JobScheduler,
};
use tidepool_effect::{DeferredEffect, Response};
use tidepool_handlers::handlers::model::ModelService;
use tidepool_repr::{DataConTable, PrincipalId};
use tidepool_runtime::session::WorkbenchExecutionId;

use super::{embedded_service::EmbeddedService, ActorHostConfig};
use crate::{
    exomonad::EmbeddedLaunchConfig,
    model_turn::{CellModelService, ModelPolicy, ModelResolver},
};

pub(super) struct EmbeddedCellModelFactory<A, C> {
    runtime: tokio::runtime::Handle,
    store: Arc<Store>,
    scheduler: Arc<JobScheduler>,
    default_model: Model,
    default_reasoning: Reasoning,
    launch_config: Option<Arc<ActorHostConfig>>,
    transport: Arc<dyn Fn() -> C + Send + Sync>,
    auth: PhantomData<fn() -> A>,
}

impl<A: Auth + 'static, C: ResponsesTransport + 'static> EmbeddedCellModelFactory<A, C> {
    pub(super) fn new(
        store: Arc<Store>,
        scheduler: Arc<JobScheduler>,
        default_model: Model,
        default_reasoning: Reasoning,
        transport: Arc<dyn Fn() -> C + Send + Sync>,
    ) -> Self {
        Self {
            runtime: tokio::runtime::Handle::current(),
            store,
            scheduler,
            default_model,
            default_reasoning,
            launch_config: None,
            transport,
            auth: PhantomData,
        }
    }
    fn policy(
        &self,
        descriptor: &ActorDescriptor,
        context: Option<Arc<dyn exomonad_actor::HostedContextBinding>>,
    ) -> Result<ModelPolicy, String> {
        let backend = self.default_model.backend();
        let config = self.launch_config.clone();
        let resolve_model: Arc<ModelResolver> = Arc::new(move |model| match config.as_ref() {
            Some(config) => super::resolve_model(config, model),
            None if model.backend() == backend && !model.value().trim().is_empty() => {
                Ok(model.value().to_owned())
            }
            None => Err("model differs from the fixed inference backend".into()),
        });
        let config = self.launch_config.clone();
        let resolve_reasoning: Arc<dyn Fn(&Reasoning) -> Result<Effort, String> + Send + Sync> =
            Arc::new(move |reasoning| match config.as_ref() {
                Some(config) => super::resolve_reasoning(config, reasoning),
                None if reasoning.backend() == backend => Ok(super::reasoning_effort(reasoning)),
                None => Err("reasoning differs from the fixed inference backend".into()),
            });
        let model = descriptor
            .model()
            .cloned()
            .unwrap_or_else(|| self.default_model.clone());
        let reasoning = descriptor.reasoning().unwrap_or(self.default_reasoning);
        // Validate admitted defaults before allocating the bounded invocation.
        resolve_model(&model)?;
        resolve_reasoning(&reasoning)?;
        let inherited_model = resolve_model.clone();
        let inherited_reasoning = resolve_reasoning.clone();
        let inherit_selection = Arc::new(move || {
            let effective = context
                .as_ref()
                .map(|binding| binding.effective_inference())
                .transpose()
                .map_err(|error| error.to_string())?
                .flatten();
            let (model, reasoning) = effective.unwrap_or_else(|| (model.clone(), reasoning));
            Ok((inherited_model(&model)?, inherited_reasoning(&reasoning)?))
        });
        Ok(ModelPolicy {
            inherit_selection,
            resolve_model,
            resolve_reasoning,
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
        context: Option<Arc<dyn exomonad_actor::HostedContextBinding>>,
    ) -> Arc<dyn CellModelBinding> {
        let policy = match self.policy(descriptor, context) {
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
    #[cfg(test)]
    if let Some(transport) = service.test_transport() {
        return Arc::new(
            EmbeddedCellModelFactory::<RunAuth, _>::new(
                service.runtime.store(),
                service.runtime.scheduler(),
                config.model.clone(),
                config.reasoning,
                Arc::new(move || transport.clone()),
            )
            .with_launch_config(config),
        );
    }
    admitted_runtime_factory(&service.runtime, settings, config)
}

pub(super) fn admitted_runtime_factory(
    runtime: &super::embedded_harness::EmbeddedHarnessRuntime,
    settings: &EmbeddedLaunchConfig,
    config: &ActorHostConfig,
) -> Arc<dyn CellModelFactory> {
    let _ = settings;
    let auth = runtime.inference_auth().clone();
    Arc::new(
        EmbeddedCellModelFactory::<RunAuth, _>::new(
            runtime.store(),
            runtime.scheduler(),
            config.model.clone(),
            config.reasoning,
            Arc::new(move || harness::transport::ResponsesClient::new(auth.clone())),
        )
        .with_launch_config(config),
    )
}

#[cfg(test)]
mod tests;
