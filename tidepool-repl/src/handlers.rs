//! The minimal effect handler stack for the Wave-2 `tidepool-repl` server.
//!
//! Deliberately small — `[Console, Ask]`. `Ask` is intercepted by the worker's
//! [`crate::ask::ReplAskDispatcher`]; the only base handler is `Console`. Richer
//! effect parity with the `tidepool` eval server is a later concern (Wave 4+).

use tidepool_bridge_derive::FromCore;
use tidepool_effect::dispatch::{EffectContext, EffectHandler};
use tidepool_effect::error::EffectError;
use tidepool_mcp::{ask_decl, console_decl, CapturedOutput, DescribeEffect, EffectDecl};

/// The `Console` effect's request shape (tag 0).
#[derive(FromCore)]
pub enum ConsoleReq {
    #[core(name = "Print")]
    Print(String),
}

/// Captures `Print` output into the turn's [`CapturedOutput`].
#[derive(Clone)]
pub struct ConsoleHandler;

impl DescribeEffect for ConsoleHandler {
    fn effect_decl() -> EffectDecl {
        console_decl()
    }
}

impl EffectHandler<CapturedOutput> for ConsoleHandler {
    type Request = ConsoleReq;
    fn handle(
        &mut self,
        req: ConsoleReq,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        match req {
            ConsoleReq::Print(s) => {
                cx.user().push(s);
                cx.respond(())
            }
        }
    }
}

/// The Wave-2 effect decls (`[Console, Ask]`) and the `Ask` tag (its index).
pub fn default_decls() -> (Vec<EffectDecl>, u64) {
    let decls = vec![console_decl(), ask_decl()];
    let ask_tag = (decls.len() - 1) as u64;
    (decls, ask_tag)
}
