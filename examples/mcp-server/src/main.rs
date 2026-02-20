//! Example MCP server demonstrating Tidepool with custom effect handlers.

use core_effect::dispatch::{EffectContext, EffectHandler};
use core_effect::error::EffectError;
use core_eval::value::Value;
use tidepool_mcp::TidepoolMcpServer;

/// Console effect handler — handles print-like effects (tag 0).
#[derive(Clone)]
struct ConsoleHandler;

impl EffectHandler<()> for ConsoleHandler {
    type Request = Value;

    fn handle(
        &mut self,
        req: Self::Request,
        _cx: &EffectContext<'_, ()>,
    ) -> Result<Value, EffectError> {
        eprintln!("[console] {:?}", req);
        Ok(Value::Lit(core_repr::Literal::LitInt(0)))
    }
}

/// Environment effect handler — handles env-lookup effects (tag 1).
#[derive(Clone)]
struct EnvHandler;

impl EffectHandler<()> for EnvHandler {
    type Request = Value;

    fn handle(
        &mut self,
        req: Self::Request,
        _cx: &EffectContext<'_, ()>,
    ) -> Result<Value, EffectError> {
        eprintln!("[env] lookup {:?}", req);
        Ok(Value::Lit(core_repr::Literal::LitString("example_value".into())))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let handlers = frunk::hlist![ConsoleHandler, EnvHandler];
    let server = TidepoolMcpServer::new(handlers);
    server.serve_stdio().await
}
