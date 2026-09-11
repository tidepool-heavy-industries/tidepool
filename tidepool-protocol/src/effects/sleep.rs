//! Deliberate actor-local suspension on a monotonic timer.
use crate::hs::HsType;
use crate::schema::{
    Arg, AuthoredSurface, Effect, HandlingClass, Helper, HelperBody, Polymorphism, RustBinding,
    Verb,
};

/// The resident sleep effect.
#[must_use]
pub fn sleep() -> Effect {
    Effect {
        name: "Sleep",
        authored_surface: AuthoredSurface::All,
        handler: "SleepDecodeHandler",
        handler_module: "sleep",
        req_enum: "SleepReq",
        decl_fn: "sleep_decl",
        description: &[
            "Suspend the current actor evaluation on a monotonic timer without occupying ",
            "command resources or waking the model before completion.",
        ],
        prompt_card: Some(&[
            "`sleep (minutes 15)` suspends this evaluation once and returns `()` after the delay.",
        ]),
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &["import Tidepool.Duration (Duration, milliseconds, seconds, minutes)"],
        type_defs: vec![],
        foreign_types: &[("Duration", "crate::request_effect::RequestDuration")],
        errors: None,
        verbs: vec![Verb {
            ctor: "SleepWith",
            method: "sleep_with",
            args: vec![Arg {
                name: "duration",
                ty: HsType::Named("Duration"),
                rust: RustBinding::Path("crate::request_effect::RequestDuration"),
            }],
            ret: HsType::Unit,
            errors: None,
            handling: HandlingClass::Actor,
            extract: None,
        }],
        helpers: vec![Helper {
            name: "sleep",
            ctor: Some("SleepWith"),
            substrate: false,
            doc: &["Suspend this evaluation for at least the requested monotonic duration."],
            body: HelperBody::Pointfree,
        }],
        polymorphism: Polymorphism::None,
        dispatched: false,
    }
}
