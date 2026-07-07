//! Thin re-export of the freer-simple / open-union / FTCQueue constructor
//! names — canonically defined in `tidepool_repr::freer_names` (plan 05 F2
//! single-sourcing: `tidepool-repr`'s own `normalize.rs` needs these same
//! names for the production-path variant of this collision, and
//! `tidepool-effect` depends on `tidepool-repr`, not the other way around, so
//! the definitions had to move down there). Kept as a module here — instead
//! of removing it and updating call sites — so `crate::freer_names::…` in
//! `machine.rs`, and `tidepool_effect::freer_names::…` in `tidepool-codegen`'s
//! `effect_machine::ConTags::try_from`, keep compiling unchanged.
pub use tidepool_repr::freer_names::*;
