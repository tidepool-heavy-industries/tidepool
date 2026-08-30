//! Dispatch logic for algebraic effects.

use crate::error::EffectError;
use frunk::{HCons, HNil};
use tidepool_bridge::error::BridgeError;
use tidepool_bridge::{FromCore, ToCore};
use tidepool_eval::value::Value;
use tidepool_repr::{DataConId, DataConTable};
/// A handler's answer to an effect request.
#[derive(Debug)]
pub enum Response {
    /// Fully materialized value (the classic path).
    Complete(Value),
    /// A list response with every element already converted. Carried as a
    /// flat `Vec` (plus the list constructor ids) rather than a pre-built
    /// cons `Value` so the machine can build the spine ITERATIVELY at the
    /// heap boundary — a deep recursive `Value` spine must never exist,
    /// neither at construction nor at `Drop` (~3 stack frames per cell
    /// overflow the eval thread; see `materialize_cons_list`).
    List {
        items: Vec<Value>,
        cons_id: DataConId,
        nil_id: DataConId,
    },
}

impl From<Value> for Response {
    fn from(v: Value) -> Self {
        Response::Complete(v)
    }
}

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

    /// Convert a Rust value into a complete response for the JIT.
    pub fn respond<T: ToCore>(&self, val: T) -> Result<Response, EffectError> {
        val.to_value(self.table)
            .map(Response::Complete)
            .map_err(EffectError::Bridge)
    }

    /// Respond with an owned `Vec` as a Haskell list. Every element converts
    /// EAGERLY, here, at dispatch time — there is no deferred conversion and
    /// no stream machinery; what stays special about a list response is only
    /// that the machine builds its heap spine iteratively (stack safety on
    /// long lists), which is why this is not just `respond(items)`.
    pub fn respond_list<T>(&self, items: Vec<T>) -> Result<Response, EffectError>
    where
        T: ToCore,
    {
        let cons_id = tidepool_bridge::get_resilient(self.table, ":", 2)
            .ok_or_else(|| EffectError::Bridge(BridgeError::UnknownDataConName(":".into())))?;
        let nil_id = tidepool_bridge::get_resilient(self.table, "[]", 0)
            .ok_or_else(|| EffectError::Bridge(BridgeError::UnknownDataConName("[]".into())))?;
        let items = items
            .into_iter()
            .map(|x| x.to_value(self.table))
            .collect::<Result<Vec<_>, _>>()
            .map_err(EffectError::Bridge)?;
        Ok(Response::List {
            items,
            cons_id,
            nil_id,
        })
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
/// `Request` is typically a `#[derive(FromCore)]` enum mirroring the Haskell GADT.
///
/// ```no_run
/// use tidepool_effect::{EffectHandler, EffectContext, EffectError, Response};
///
/// struct UnitHandler;
///
/// impl EffectHandler for UnitHandler {
///     type Request = ();
///     fn handle(&mut self, _req: (), cx: &EffectContext) -> Result<Response, EffectError> {
///         cx.respond(())
///     }
/// }
/// ```
pub trait EffectHandler<U = ()> {
    /// The Haskell-side effect request this handler consumes, decoded from
    /// the Core `Value` via [`FromCore`].
    type Request: FromCore;

    /// Handle one decoded request and produce a response.
    fn handle(
        &mut self,
        req: Self::Request,
        cx: &EffectContext<'_, U>,
    ) -> Result<Response, EffectError>;
}

/// Constructor-based effect routing over an HList of handlers.
///
/// The freer-simple union tag is deliberately absent from this interface.
/// Each handler declares the nominal request constructors it owns; routing
/// returns `None` when no installed handler owns the request. The execution
/// layer then decides whether an unhandled request suspends or is an error.
pub trait DispatchEffect<U = ()> {
    /// Route `request` by its nominal constructor.
    fn dispatch(
        &mut self,
        request: &Value,
        cx: &EffectContext<'_, U>,
    ) -> Result<Option<Response>, EffectError>;
}

/// Name an effect request for diagnostics without assigning routing meaning
/// to the freer-simple union tag that carried it.
#[must_use]
pub fn request_constructor(request: &Value, table: &DataConTable) -> String {
    match request {
        Value::Con(id, _) => table
            .get(*id)
            .and_then(|con| con.qualified_name.as_deref().or(Some(con.name.as_str())))
            .map(str::to_owned)
            .unwrap_or_else(|| format!("constructor {id:?}")),
        other => format!("non-constructor value {other:?}"),
    }
}

impl<U> DispatchEffect<U> for HNil {
    fn dispatch(
        &mut self,
        _request: &Value,
        _cx: &EffectContext<'_, U>,
    ) -> Result<Option<Response>, EffectError> {
        Ok(None)
    }
}

impl<U, H: EffectHandler<U>, T: DispatchEffect<U>> DispatchEffect<U> for HCons<H, T> {
    fn dispatch(
        &mut self,
        request: &Value,
        cx: &EffectContext<'_, U>,
    ) -> Result<Option<Response>, EffectError> {
        match H::Request::from_value(request, cx.table()) {
            Ok(req) => self.head.handle(req, cx).map(Some),
            // Derived request enums use UnknownDataCon only after checking
            // every constructor they own. At this layer that means "not my
            // effect", not malformed input; continue through the installed
            // handler set. Arity/field/type errors from a recognized
            // constructor remain loud and never fall through to a sibling.
            Err(BridgeError::UnknownDataCon(_)) => self.tail.dispatch(request, cx),
            Err(error) => Err(EffectError::Bridge(error)),
        }
    }
}

// Forwarding impls: a `&mut H` or a boxed handler dispatches through its inner
// handler. These let a generic `H: DispatchEffect<U>` bound accept a type-erased
// `Box<dyn _>` handler stack (e.g. the MCP server's `Box<dyn McpEffectHandler>`),
// so a driver that is generic over the handler can be fed a boxed one without a
// bespoke wrapper.
impl<U, H: DispatchEffect<U> + ?Sized> DispatchEffect<U> for &mut H {
    fn dispatch(
        &mut self,
        request: &Value,
        cx: &EffectContext<'_, U>,
    ) -> Result<Option<Response>, EffectError> {
        (**self).dispatch(request, cx)
    }
}

impl<U, H: DispatchEffect<U> + ?Sized> DispatchEffect<U> for Box<H> {
    fn dispatch(
        &mut self,
        request: &Value,
        cx: &EffectContext<'_, U>,
    ) -> Result<Option<Response>, EffectError> {
        (**self).dispatch(request, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::types::Literal;

    fn empty_table() -> DataConTable {
        DataConTable::new()
    }

    fn make_cx(table: &DataConTable) -> EffectContext<'_> {
        EffectContext::with_user(table, &())
    }

    fn lit_int(n: i64) -> Value {
        Value::Lit(Literal::LitInt(n))
    }

    #[test]
    fn hnil_leaves_every_request_unhandled() {
        let table = empty_table();
        let cx = make_cx(&table);
        assert!(HNil.dispatch(&lit_int(5), &cx).unwrap().is_none());
    }

    #[test]
    fn effect_context_respond_round_trips_value() {
        let table = empty_table();
        let cx = make_cx(&table);
        let result = cx.respond(lit_int(42)).unwrap();
        match result {
            Response::Complete(Value::Lit(Literal::LitInt(42))) => {}
            other => panic!("expected LitInt(42), got {other:?}"),
        }
    }
}
