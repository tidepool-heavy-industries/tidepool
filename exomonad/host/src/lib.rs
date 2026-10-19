//! Exomonad host composition, provider integration, and operator interfaces.

// Embedded actor admission retains nested persistent compiler-world facts.
// Keep their Send/Sync proofs within the compiler trait-solver recursion budget.
#![recursion_limit = "256"]

pub mod actor_host;
pub mod exomonad;
pub mod haskell_sources;
pub mod model_turn;
pub mod operator;
pub mod run_map;

mod generated;

#[cfg(test)]
mod transport_test_support {
    use exomonad_actor::{
        ResidentToolDispatchFuture, ResidentToolEndpoint, ResidentToolError, ResidentToolFuture,
    };

    pub(crate) trait ResidentToolEndpointTestExt {
        fn dispatch_json_boxed(
            &self,
            invocation: exomonad_tool::ToolInvocation,
        ) -> ResidentToolFuture;
    }

    impl<T: ResidentToolEndpoint + ?Sized> ResidentToolEndpointTestExt for T {
        fn dispatch_json_boxed(
            &self,
            invocation: exomonad_tool::ToolInvocation,
        ) -> ResidentToolFuture {
            let future: ResidentToolDispatchFuture = self.dispatch_boxed(invocation);
            Box::pin(async move {
                future
                    .await?
                    .into_json()
                    .map_err(ResidentToolError::Encoding)
            })
        }
    }
}
