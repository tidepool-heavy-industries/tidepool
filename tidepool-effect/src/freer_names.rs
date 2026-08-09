//! Re-export of the freer-simple / open-union / FTCQueue constructor names.
//!
//! Canonically defined in `tidepool_repr::freer_names`: `tidepool-repr`'s own
//! `normalize.rs` needs these same names, and `tidepool-effect` depends on
//! `tidepool-repr` (not the reverse), so the single source lives there. This
//! module exists so `crate::freer_names::…` (used in `machine.rs`) and
//! `tidepool_effect::freer_names::…` (used in `tidepool-codegen`'s
//! `effect_machine::ConTags::try_from`) both resolve to the same names.
pub use tidepool_repr::freer_names::*;
