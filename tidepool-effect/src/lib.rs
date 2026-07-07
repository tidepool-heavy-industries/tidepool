//! Effect handling for Tidepool's freer-simple effect system.
//!
//! Provides `EffectHandler` and `DispatchEffect` traits with HList-based
//! handler composition for dispatching algebraic effects at runtime.

pub mod dispatch;
pub mod error;
pub mod freer_names;
pub mod machine;
pub mod pause;

pub use dispatch::*;
pub use error::*;
pub use machine::*;
