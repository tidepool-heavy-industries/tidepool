//! Host backend for the `Jev` effect: one judgment request (JSON text) in,
//! the response body (JSON text) or a typed call failure out.

use std::sync::Arc;

use futures_util::future::BoxFuture;

/// `Tidepool.Effects.Core.JevCallError`, constructor for constructor.
#[derive(Debug, Clone, PartialEq, Eq, tidepool_bridge_derive::ToCore)]
pub enum JevCallFailure {
    #[core(module = "Tidepool.Effects.Core", name = "JevUnconfigured")]
    Unconfigured,
    #[core(module = "Tidepool.Effects.Core", name = "JevCallCap")]
    CallCap,
    #[core(module = "Tidepool.Effects.Core", name = "JevTransport")]
    Transport(String),
    #[core(module = "Tidepool.Effects.Core", name = "JevTimeout")]
    Timeout,
    #[core(module = "Tidepool.Effects.Core", name = "JevHttp")]
    Http(i64, String),
    #[core(module = "Tidepool.Effects.Core", name = "JevBodyLimit")]
    BodyLimit,
    #[core(module = "Tidepool.Effects.Core", name = "JevMalformed")]
    Malformed(String),
}

/// Answers `Jev` requests for every actor of a forest.
pub trait JevBackend: Send + Sync {
    fn ask(&self, request: String) -> BoxFuture<'_, Result<String, JevCallFailure>>;
}

/// Shared backend handle; the default answers every request as unconfigured.
pub type JevBackendHandle = Arc<dyn JevBackend>;

pub(crate) struct UnconfiguredJev;

/// A backend that answers every request as unconfigured.
#[must_use]
pub fn unconfigured_jev() -> JevBackendHandle {
    Arc::new(UnconfiguredJev)
}

impl JevBackend for UnconfiguredJev {
    fn ask(&self, _request: String) -> BoxFuture<'_, Result<String, JevCallFailure>> {
        Box::pin(async { Err(JevCallFailure::Unconfigured) })
    }
}
