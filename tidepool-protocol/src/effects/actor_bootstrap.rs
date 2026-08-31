//! Kernel-private actor installation boundary.
//!
//! The trusted `Tidepool.Actor` entry wrapper raises authored initialization
//! and behavior into a row containing this effect, then parks once on
//! `ActorReadyWith`. Authored actor rows and model workbenches never contain
//! `ActorBootstrap`; Rust additionally checks the installed-program realm and
//! initialization phase before publishing the actor.

use crate::schema::{Effect, HandlingClass, Polymorphism, Verb};
use crate::HsType;

/// The private readiness effect used by the trusted actor entry wrapper.
#[must_use]
pub fn actor_bootstrap() -> Effect {
    Effect {
        name: "ActorBootstrap",
        authored_surface: crate::schema::AuthoredSurface::OPAQUE,
        handler: "ActorBootstrapDecodeHandler",
        handler_module: "actor_bootstrap",
        req_enum: "ActorBootstrapReq",
        decl_fn: "actor_bootstrap_decl",
        description: &[
            "Kernel-private actor installation boundary. The trusted Tidepool.Actor wrapper uses ",
            "it to publish readiness; authored actor code has no operation in this effect.",
        ],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &["import Tidepool.Actor"],
        type_defs: Vec::new(),
        foreign_types: &[],
        errors: None,
        verbs: vec![Verb {
            ctor: "ActorReadyWith",
            method: "actor_ready_with",
            args: Vec::new(),
            ret: HsType::Unit,
            errors: None,
            handling: HandlingClass::Actor,
            extract: None,
        }],
        helpers: Vec::new(),
        polymorphism: Polymorphism::None,
        dispatched: false,
    }
}
