//! Dispatch logic for algebraic effects.

use crate::error::EffectError;
use frunk::{HCons, HNil};
use tidepool_bridge::{FromCore, ToCore};
use tidepool_eval::value::Value;
use tidepool_repr::DataConTable;

/// Shared context passed to effect handlers during dispatch.
///
/// Carries the [`DataConTable`] (needed for `FromCore`/`ToCore` conversions) and
/// an optional user-defined state value `U` that handlers can read.
pub struct EffectContext<'a, U = ()> {
    table: &'a DataConTable,
    user: &'a U,
}

impl<'a, U> EffectContext<'a, U> {
    /// Create a new context with a user state value and data constructor table.
    pub fn with_user(table: &'a DataConTable, user: &'a U) -> Self {
        Self { table, user }
    }

    /// Convert a Rust value into a Core `Value` suitable for returning to the JIT.
    pub fn respond<T: ToCore>(&self, val: T) -> Result<Value, EffectError> {
        val.to_value(self.table).map_err(EffectError::Bridge)
    }

    /// Access the data constructor table (for manual `FromCore`/`ToCore` calls).
    pub fn table(&self) -> &DataConTable {
        self.table
    }

    /// Access the user-defined state.
    pub fn user(&self) -> &U {
        self.user
    }
}

/// Handler for a single effect type.
///
/// Implement this trait for each Rust struct that handles one Haskell effect.
/// `Request` is the `#[derive(FromCore)]` enum mirroring the Haskell GADT.
///
/// ```ignore
/// impl EffectHandler for ConsoleHandler {
///     type Request = ConsoleReq;
///     fn handle(&mut self, req: ConsoleReq, cx: &EffectContext) -> Result<Value, EffectError> {
///         match req {
///             ConsoleReq::Print(msg) => { println!("{msg}"); cx.respond(()) }
///         }
///     }
/// }
/// ```
pub trait EffectHandler<U = ()> {
    type Request: FromCore;
    fn handle(
        &mut self,
        req: Self::Request,
        cx: &EffectContext<'_, U>,
    ) -> Result<Value, EffectError>;
}

/// Tag-based effect dispatch over an HList of handlers.
///
/// The JIT yields `(tag, request)` pairs where `tag` identifies which effect
/// in the `Eff '[E0, E1, ...]` list fired. `DispatchEffect` peels one layer
/// per HCons: tag 0 → head handler, tag N → tail with tag N−1.
///
/// You don't implement this manually — it's derived for `frunk::HList![H0, H1, ...]`
/// when each `Hi: EffectHandler`.
pub trait DispatchEffect<U = ()> {
    fn dispatch(
        &mut self,
        tag: u64,
        request: &Value,
        cx: &EffectContext<'_, U>,
    ) -> Result<Value, EffectError>;
}

impl<U> DispatchEffect<U> for HNil {
    fn dispatch(
        &mut self,
        tag: u64,
        _request: &Value,
        _cx: &EffectContext<'_, U>,
    ) -> Result<Value, EffectError> {
        Err(EffectError::UnhandledEffect { tag })
    }
}

impl<U, H: EffectHandler<U>, T: DispatchEffect<U>> DispatchEffect<U> for HCons<H, T> {
    fn dispatch(
        &mut self,
        tag: u64,
        request: &Value,
        cx: &EffectContext<'_, U>,
    ) -> Result<Value, EffectError> {
        if tag == 0 {
            let req = H::Request::from_value(request, cx.table())?;
            self.head.handle(req, cx)
        } else {
            self.tail.dispatch(tag - 1, request, cx)
        }
    }
}
