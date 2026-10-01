//! Per-cell model invocation boundary. The host owns the provider, durable
//! evidence, cancellation and shared budget; this handler owns none of them.
use crate::effect_glue::JsonArg;
pub use crate::generated::model_call::{ModelBoundaryError, ModelReq};
use serde_json::Value;
use std::sync::Arc;
use tidepool_effect::{
    dispatch::{EffectContext, Response},
    error::EffectError,
};
use tidepool_mcp::CapturedOutput;
use tidepool_repr::PrincipalId;

/// Installed once for an admitted cell and shared by all of its continuations.
/// Methods run in the generated deferred handler, outside machine checkout.
/// Implementations must bind opaque invocation identities to this cell and its
/// principal, and retain a callback result before offering its after-tool hook.
pub trait ModelService: Send + Sync {
    fn start(&self, caller: PrincipalId, request: Value) -> Result<Value, ModelBoundaryError>;
    fn resume(
        &self,
        caller: PrincipalId,
        invocation: &str,
        call_id: &str,
        answer: Value,
    ) -> Result<Value, ModelBoundaryError>;
    fn annotate(
        &self,
        caller: PrincipalId,
        invocation: &str,
        operation: &str,
        annotation: Value,
    ) -> Result<Value, ModelBoundaryError>;
    fn close(&self, caller: PrincipalId, invocation: &str) -> Result<(), ModelBoundaryError>;
}

/// Cloning preserves the cell service, including its aggregate budget.
#[derive(Clone, Default)]
pub struct ModelHandler {
    service: Option<Arc<dyn ModelService>>,
}
impl ModelHandler {
    #[must_use]
    pub fn new(service: Arc<dyn ModelService>) -> Self {
        Self {
            service: Some(service),
        }
    }
    fn service(&self) -> Result<&dyn ModelService, ModelBoundaryError> {
        self.service.as_deref().ok_or_else(|| {
            ModelBoundaryError::ModelUnavailable("this cell has no admitted model service".into())
        })
    }
    pub(crate) fn model_start(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        request: JsonArg,
    ) -> Result<Response, EffectError> {
        cx.respond(
            self.service()
                .and_then(|service| service.start(cx.principal(), request.0)),
        )
    }
    pub(crate) fn model_resume(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        invocation: String,
        call_id: String,
        answer: JsonArg,
    ) -> Result<Response, EffectError> {
        cx.respond(
            self.service().and_then(|service| {
                service.resume(cx.principal(), &invocation, &call_id, answer.0)
            }),
        )
    }
    pub(crate) fn model_annotate(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        invocation: String,
        operation: String,
        annotation: JsonArg,
    ) -> Result<Response, EffectError> {
        cx.respond(self.service().and_then(|service| {
            service.annotate(cx.principal(), &invocation, &operation, annotation.0)
        }))
    }
    pub(crate) fn model_close(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        invocation: String,
    ) -> Result<Response, EffectError> {
        cx.respond(
            self.service()
                .and_then(|service| service.close(cx.principal(), &invocation)),
        )
    }
}
