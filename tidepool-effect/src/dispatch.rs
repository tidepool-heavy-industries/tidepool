//! Dispatch logic for algebraic effects.

use crate::error::EffectError;
use frunk::{HCons, HNil};
use tidepool_bridge::error::BridgeError;
use tidepool_bridge::HaskellValue;
use tidepool_bridge::{FromHaskell, ToHaskell};
use tidepool_repr::{DataConTable, PrincipalId};
/// A handler's answer to an effect request.
pub struct Response {
    source: Box<dyn ToHaskell>,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response").finish_non_exhaustive()
    }
}

impl Response {
    pub fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn tidepool_bridge::HaskellVisitor,
    ) -> Result<(), BridgeError> {
        self.source.visit(table, visitor)
    }

    /// Materialize this response at the dispatch/resume boundary. The owned
    /// source remains structural until a consumer actually needs a snapshot.
    pub fn to_value(&self, table: &DataConTable) -> Result<HaskellValue, BridgeError> {
        self.source.to_value(table)
    }
}

impl From<HaskellValue> for Response {
    fn from(v: HaskellValue) -> Self {
        Response {
            source: Box::new(v),
        }
    }
}

/// Shared context passed to effect handlers during dispatch.
///
/// Carries the [`DataConTable`] (needed for `FromHaskell`/`ToHaskell` conversions),
/// the exact runtime principal for this execution entry, and an optional
/// user-defined state value `U` that handlers can read.
pub struct EffectContext<'a, U = ()> {
    table: &'a DataConTable,
    principal: PrincipalId,
    user: &'a U,
}

impl<'a, U> EffectContext<'a, U> {
    /// Create a new context with a user state value and data constructor table.
    ///
    /// This compatibility constructor is for non-actor execution. Actor-aware
    /// runtimes must use [`Self::with_principal`].
    pub fn with_user(table: &'a DataConTable, user: &'a U) -> Self {
        Self::with_principal(table, PrincipalId::SYSTEM, user)
    }

    /// Create a context for one exact runtime principal.
    pub fn with_principal(table: &'a DataConTable, principal: PrincipalId, user: &'a U) -> Self {
        Self {
            table,
            principal,
            user,
        }
    }

    /// Convert a Rust value into a complete response for the JIT.
    pub fn respond<T: ToHaskell + 'static>(&self, val: T) -> Result<Response, EffectError> {
        Ok(Response {
            source: Box::new(val),
        })
    }

    /// Access the data constructor table (for manual `FromHaskell`/`ToHaskell` calls).
    pub fn table(&self) -> &DataConTable {
        self.table
    }

    /// Exact authority installed for this execution entry.
    pub fn principal(&self) -> PrincipalId {
        self.principal
    }

    /// Access the user-defined state.
    pub fn user(&self) -> &U {
        self.user
    }
}

/// Handler for a single effect type.
///
/// Implement this trait for each Rust struct that handles one Haskell effect.
/// `Request` is typically a `#[derive(FromHaskell)]` enum mirroring the Haskell GADT.
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
    /// the Core `HaskellValue` via [`FromHaskell`].
    type Request: FromHaskell;

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
        request: &HaskellValue,
        cx: &EffectContext<'_, U>,
    ) -> Result<Option<Response>, EffectError>;
}

/// Name an effect request for diagnostics without assigning routing meaning
/// to the freer-simple union tag that carried it.
#[must_use]
pub fn request_constructor(request: &HaskellValue, table: &DataConTable) -> String {
    match request {
        HaskellValue::Con(id, _) => table
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
        _request: &HaskellValue,
        _cx: &EffectContext<'_, U>,
    ) -> Result<Option<Response>, EffectError> {
        Ok(None)
    }
}

impl<U, H: EffectHandler<U>, T: DispatchEffect<U>> DispatchEffect<U> for HCons<H, T> {
    fn dispatch(
        &mut self,
        request: &HaskellValue,
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
        request: &HaskellValue,
        cx: &EffectContext<'_, U>,
    ) -> Result<Option<Response>, EffectError> {
        (**self).dispatch(request, cx)
    }
}

impl<U, H: DispatchEffect<U> + ?Sized> DispatchEffect<U> for Box<H> {
    fn dispatch(
        &mut self,
        request: &HaskellValue,
        cx: &EffectContext<'_, U>,
    ) -> Result<Option<Response>, EffectError> {
        (**self).dispatch(request, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tidepool_repr::types::Literal;

    struct CountedResponse(Arc<AtomicUsize>);

    impl tidepool_bridge::sealed::ToHaskellSealed for CountedResponse {}

    impl ToHaskell for CountedResponse {
        fn visit(
            &self,
            _table: &DataConTable,
            visitor: &mut dyn tidepool_bridge::HaskellVisitor,
        ) -> Result<(), BridgeError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            visitor.literal(Literal::LitInt(7))
        }
    }

    fn empty_table() -> DataConTable {
        DataConTable::new()
    }

    fn make_cx(table: &DataConTable) -> EffectContext<'_> {
        EffectContext::with_user(table, &())
    }

    fn lit_int(n: i64) -> HaskellValue {
        HaskellValue::Lit(Literal::LitInt(n))
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
        assert!(matches!(
            result.to_value(&table),
            Ok(HaskellValue::Lit(Literal::LitInt(42)))
        ));
    }

    #[test]
    fn response_keeps_its_source_until_materialization() {
        let table = empty_table();
        let cx = make_cx(&table);
        let visits = Arc::new(AtomicUsize::new(0));
        let response = cx.respond(CountedResponse(Arc::clone(&visits))).unwrap();
        assert_eq!(visits.load(Ordering::SeqCst), 0);
        assert!(matches!(
            response.to_value(&table),
            Ok(HaskellValue::Lit(Literal::LitInt(7)))
        ));
        assert_eq!(visits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn effect_context_distinguishes_system_and_explicit_principals() {
        let table = empty_table();
        let system = EffectContext::with_user(&table, &());
        assert_eq!(system.principal(), PrincipalId::SYSTEM);

        let expected = PrincipalId::new(17, 3);
        let actor = EffectContext::with_principal(&table, expected, &());
        assert_eq!(actor.principal(), expected);
    }
}
