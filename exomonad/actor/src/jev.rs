//! Host backend for the `Jev` effect: one judgment request (JSON text) in,
//! the response body (JSON text) or a typed call failure out.

use std::sync::Arc;

use futures_util::future::BoxFuture;

/// `Tidepool.Effects.Core.JevCallError`, constructor for constructor.
#[derive(Debug, Clone, PartialEq, Eq, tidepool_bridge_derive::ToHaskell)]
pub enum JevCallFailure {
    #[haskell(module = "Tidepool.Effects.Core", name = "JevUnconfigured")]
    Unconfigured,
    #[haskell(module = "Tidepool.Effects.Core", name = "JevCallCap")]
    CallCap,
    #[haskell(module = "Tidepool.Effects.Core", name = "JevTransport")]
    Transport(String),
    #[haskell(module = "Tidepool.Effects.Core", name = "JevTimeout")]
    Timeout,
    #[haskell(module = "Tidepool.Effects.Core", name = "JevHttp")]
    Http(i64, String),
    #[haskell(module = "Tidepool.Effects.Core", name = "JevCircuitOpen")]
    CircuitOpen(i64, i64),
    #[haskell(module = "Tidepool.Effects.Core", name = "JevClientSetup")]
    ClientSetup(String),
    #[haskell(module = "Tidepool.Effects.Core", name = "JevBodyLimit")]
    BodyLimit,
    #[haskell(module = "Tidepool.Effects.Core", name = "JevMalformed")]
    Malformed(String),
}

/// Answers `Jev` requests for every actor of a forest.
pub trait JevBackend: Send + Sync {
    /// Whether this instance has a configured judgment service. Transient
    /// request failures do not change installation support.
    fn is_installed(&self) -> bool {
        true
    }

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

/// A backend that retains a client-construction failure for each request.
#[must_use]
pub fn failed_jev_client_setup(detail: impl Into<String>) -> JevBackendHandle {
    Arc::new(FailedJevClientSetup(detail.into()))
}

struct FailedJevClientSetup(String);

impl JevBackend for FailedJevClientSetup {
    fn ask(&self, _request: String) -> BoxFuture<'_, Result<String, JevCallFailure>> {
        let detail = self.0.clone();
        Box::pin(async move { Err(JevCallFailure::ClientSetup(detail)) })
    }
}

impl JevBackend for UnconfiguredJev {
    fn is_installed(&self) -> bool {
        false
    }

    fn ask(&self, _request: String) -> BoxFuture<'_, Result<String, JevCallFailure>> {
        Box::pin(async { Err(JevCallFailure::Unconfigured) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn setup_failure_is_retained_as_its_typed_cause() {
        let backend = failed_jev_client_setup("client builder rejected configuration");
        assert_eq!(
            backend.ask("{}".into()).await,
            Err(JevCallFailure::ClientSetup(
                "client builder rejected configuration".into()
            ))
        );
    }
}
