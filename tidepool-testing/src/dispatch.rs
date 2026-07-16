//! Shared effect dispatchers for tests.

use tidepool_effect::{DispatchEffect, EffectContext, EffectError, Response};
use tidepool_eval::Value;

/// No-op dispatcher for tests whose evaluated code never dispatches an effect
/// (pure `result` bindings, compile-error assertions): responds `0` to any
/// request. Generic over the user-data slot so it slots into any
/// `EffectContext<'_, U>` stack.
pub struct NullDispatcher;

impl<U> DispatchEffect<U> for NullDispatcher {
    fn dispatch(
        &mut self,
        _tag: u64,
        _request: &Value,
        cx: &EffectContext<'_, U>,
    ) -> Result<Response, EffectError> {
        cx.respond(serde_json::json!(0))
    }
}
