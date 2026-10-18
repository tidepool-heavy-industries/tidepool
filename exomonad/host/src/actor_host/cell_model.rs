//! Compose the admitted cell's ModelCall service from the embedded run owners.
use std::{marker::PhantomData, sync::Arc};

use super::run_auth::RunAuth;
use exomonad_actor::{
    ActorDescriptor, AdmittedModelIdentity, CellModelBinding, CellModelFactory, ModelPermissions,
    ModelReq,
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
    model_turn::{CellModelService, ModelPolicy},
};

pub(super) struct EmbeddedCellModelFactory<A, C> {
    runtime: tokio::runtime::Handle,
    store: Arc<Store>,
    scheduler: Arc<JobScheduler>,
    launch_config: Option<Arc<ActorHostConfig>>,
    selection: Arc<crate::model_turn::SelectedTransport<C>>,
    auth: PhantomData<fn() -> A>,
}

impl<A: Auth + 'static, C: ResponsesTransport + 'static> EmbeddedCellModelFactory<A, C> {
    pub(super) fn new(
        store: Arc<Store>,
        scheduler: Arc<JobScheduler>,
        selection: Arc<crate::model_turn::SelectedTransport<C>>,
    ) -> Self {
        Self {
            runtime: tokio::runtime::Handle::current(),
            store,
            scheduler,
            launch_config: None,
            selection,
            auth: PhantomData,
        }
    }
    fn selected_transport(
        &self,
        descriptor: &ActorDescriptor,
    ) -> Result<Arc<crate::model_turn::SelectedTransport<C>>, String> {
        let selection = self.selection.clone();
        let catalog = self
            .launch_config
            .as_ref()
            .and_then(|config| config.workspace_inputs.as_ref())
            .and_then(|workspace| workspace.model_catalog.clone())
            .ok_or("no frozen model catalog is admitted")?;
        let permissions = descriptor.capabilities().model_permissions().clone();
        Ok(Arc::new(move |choice| {
            admitted_catalog_model(&catalog, &permissions, choice)?;
            selection(choice)
        }))
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
        _context: Option<Arc<dyn exomonad_actor::HostedContextBinding>>,
    ) -> Arc<dyn CellModelBinding> {
        let selection = match self.selected_transport(descriptor) {
            Ok(selection) => selection,
            Err(reason) => return Arc::new(RefusedCellModelBinding(reason)),
        };
        Arc::new(EmbeddedCellModelBinding(Arc::new(
            CellModelService::<A, C>::new(
                self.runtime.clone(),
                principal,
                execution.as_str().to_owned(),
                self.store.clone(),
                self.scheduler.clone(),
                ModelPolicy {
                    limits: Limits::default(),
                },
                selection,
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
                ModelReq::ModelStartWith(family, request) => {
                    Response::new(service.start(principal, family.into(), json(&request)))
                }
                ModelReq::ModelResumeWith(family, invocation, call, answer) => Response::new(
                    service.resume(principal, family.into(), &invocation, &call, json(&answer)),
                ),
                ModelReq::ModelAnnotateWith(family, invocation, operation, annotation) => {
                    Response::new(service.annotate(
                        principal,
                        family.into(),
                        &invocation,
                        &operation,
                        json(&annotation),
                    ))
                }
                ModelReq::ModelCloseWith(family, invocation) => {
                    Response::new(service.close(principal, family.into(), &invocation))
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
                ModelReq::ModelCloseWith(..) => Response::new(Err::<(), _>(error)),
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
        let catalog = config
            .workspace_inputs
            .as_ref()
            .and_then(|workspace| workspace.model_catalog.clone());
        return Arc::new(
            EmbeddedCellModelFactory::<RunAuth, _>::new(
                service.runtime.store(),
                service.runtime.scheduler(),
                Arc::new(move |choice| {
                    let selected = catalog_model(
                        catalog
                            .as_ref()
                            .ok_or("no frozen model catalog is admitted")?,
                        choice,
                    )?;
                    Ok((
                        transport.clone(),
                        selected.model.value().to_owned(),
                        super::reasoning_effort(&choice.reasoning),
                    ))
                }),
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
    let catalog = config
        .workspace_inputs
        .as_ref()
        .and_then(|workspace| workspace.model_catalog.clone());
    let providers = runtime.inference_providers().cloned();
    Arc::new(
        EmbeddedCellModelFactory::<RunAuth, _>::new(
            runtime.store(),
            runtime.scheduler(),
            Arc::new(move |choice| {
                catalog_transport(
                    catalog
                        .as_ref()
                        .ok_or("no frozen model catalog is admitted")?,
                    providers
                        .as_ref()
                        .ok_or("no provider registry is admitted")?,
                    choice,
                )
            }),
        )
        .with_launch_config(config),
    )
}

fn catalog_transport(
    catalog: &crate::exomonad::model_catalog::ResolvedCatalog,
    providers: &std::collections::BTreeMap<String, RunAuth>,
    selection: &tidepool_bridge_effects::ModelSelectionEnvelope,
) -> Result<(harness::transport::ResponsesClient<RunAuth>, String, Effort), String> {
    let selected = catalog_model(catalog, selection)?;
    let auth = providers
        .get(&selected.provider)
        .ok_or("model provider is unavailable")?
        .clone();
    Ok((
        harness::transport::ResponsesClient::new(auth),
        selected.model.value().to_owned(),
        super::reasoning_effort(&selection.reasoning),
    ))
}

fn catalog_model<'a>(
    catalog: &'a crate::exomonad::model_catalog::ResolvedCatalog,
    selection: &tidepool_bridge_effects::ModelSelectionEnvelope,
) -> Result<&'a crate::exomonad::model_catalog::ResolvedModel, String> {
    if selection.catalog_identity != catalog.identity {
        return Err("model choice belongs to another frozen catalog".into());
    }
    let selected = catalog
        .models
        .get(&selection.key)
        .ok_or("model is not admitted by this profile")?;
    if !selected.allowed_efforts.contains(&selection.reasoning) {
        return Err("model effort is not admitted by this profile".into());
    }
    Ok(selected)
}

fn admitted_catalog_model<'a>(
    catalog: &'a crate::exomonad::model_catalog::ResolvedCatalog,
    permissions: &ModelPermissions,
    selection: &tidepool_bridge_effects::ModelSelectionEnvelope,
) -> Result<&'a crate::exomonad::model_catalog::ResolvedModel, String> {
    let selected = catalog_model(catalog, selection)?;
    let identity = AdmittedModelIdentity::new(
        catalog.identity.clone(),
        selection.key.clone(),
        selected.provider.clone(),
        selected.model.clone(),
    )
    .map_err(|error| error.to_string())?;
    if !permissions.permits(
        exomonad_tool::ModelOperation::ModelCall,
        &identity,
        selection.reasoning,
    ) {
        return Err("model call is not permitted for this actor".into());
    }
    Ok(selected)
}

#[cfg(test)]
mod tests;
