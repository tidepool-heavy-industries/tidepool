//! Dispatch logic for algebraic effects.

use crate::error::EffectError;
use frunk::{HCons, HNil};
use tidepool_bridge::error::BridgeError;
use tidepool_bridge::{FromCore, ToCore};
use tidepool_eval::value::Value;
use tidepool_repr::{DataConId, DataConTable};

/// A lazily-produced sequence of effect-result elements.
///
/// The element producer is *the cursor*: the JIT materializes list cells in
/// chunks as Haskell code forces successive tails, and thunk memoization
/// (force-once → indirection) guarantees each tail is pulled exactly once,
/// in order — so a plain iterator is exactly the right shape. An infinite
/// iterator is a legitimate infinite Haskell list.
///
/// Semantics note: the producer runs at *demand* time, interleaved with
/// later effects. A `Vec`-backed stream (data captured at dispatch) keeps
/// strict effect semantics — only conversion is deferred. A live-IO
/// iterator opts into lazy-IO semantics (`hGetContents`-style): it observes
/// world state from after its effect's sequence point. Capture first unless
/// that is what you want.
pub struct ValueStream {
    source: Box<dyn ValueSource>,
    cons_id: DataConId,
    nil_id: DataConId,
}

impl ValueStream {
    /// Build a stream from a custom source plus the list constructor ids
    /// (escape hatch for exotic producers; most callers want
    /// [`EffectContext::respond_stream`]).
    pub fn from_source(
        source: Box<dyn ValueSource>,
        cons_id: DataConId,
        nil_id: DataConId,
    ) -> Self {
        Self {
            source,
            cons_id,
            nil_id,
        }
    }

    /// Decompose into (source, cons id, nil id) — consumed by the machine
    /// at the dispatch site.
    pub fn into_parts(self) -> (Box<dyn ValueSource>, DataConId, DataConId) {
        (self.source, self.cons_id, self.nil_id)
    }
}

impl std::fmt::Debug for ValueStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<value stream>")
    }
}

/// Element producer for [`ValueStream`].
///
/// The [`DataConTable`] is an *argument* to production rather than captured
/// state, so sources need no `'static` table access and the machine
/// provides its own table at chunk-materialization time.
pub trait ValueSource {
    /// Produce the next element, or `None` when exhausted.
    fn next_value(&mut self, table: &DataConTable) -> Option<Result<Value, BridgeError>>;
}

/// Adapts any iterator of `ToCore` items into a [`ValueSource`]: elements
/// convert one at a time, at pull time.
struct IterSource<I>(I);

impl<I> ValueSource for IterSource<I>
where
    I: Iterator,
    I::Item: ToCore,
{
    fn next_value(&mut self, table: &DataConTable) -> Option<Result<Value, BridgeError>> {
        self.0.next().map(|x| x.to_value(table))
    }
}

/// A handler's answer to an effect request.
#[derive(Debug)]
pub enum Response {
    /// Fully materialized value (the classic path).
    Complete(Value),
    /// Lazily-produced list elements, materialized in chunks on demand.
    Stream(ValueStream),
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

    /// Respond with a lazily-streamed list: elements convert and materialize
    /// chunk-by-chunk as the Haskell program demands them. `take k` of a huge
    /// listing only ever converts ~one chunk; an infinite iterator is a
    /// legitimate infinite list. See [`ValueStream`] for the semantics note
    /// on live-IO iterators.
    pub fn respond_stream<I>(&self, items: I) -> Result<Response, EffectError>
    where
        I: IntoIterator,
        I::IntoIter: 'static,
        I::Item: ToCore,
    {
        let cons_id = tidepool_bridge::get_resilient(self.table, ":", 2)
            .ok_or_else(|| EffectError::Bridge(BridgeError::UnknownDataConName(":".into())))?;
        let nil_id = tidepool_bridge::get_resilient(self.table, "[]", 0)
            .ok_or_else(|| EffectError::Bridge(BridgeError::UnknownDataConName("[]".into())))?;
        Ok(Response::Stream(ValueStream {
            source: Box::new(IterSource(items.into_iter())),
            cons_id,
            nil_id,
        }))
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
    type Request: FromCore;
    fn handle(
        &mut self,
        req: Self::Request,
        cx: &EffectContext<'_, U>,
    ) -> Result<Response, EffectError>;
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
    ) -> Result<Response, EffectError>;
}

impl<U> DispatchEffect<U> for HNil {
    fn dispatch(
        &mut self,
        tag: u64,
        _request: &Value,
        _cx: &EffectContext<'_, U>,
    ) -> Result<Response, EffectError> {
        Err(EffectError::UnhandledEffect { tag })
    }
}

impl<U, H: EffectHandler<U>, T: DispatchEffect<U>> DispatchEffect<U> for HCons<H, T> {
    fn dispatch(
        &mut self,
        tag: u64,
        request: &Value,
        cx: &EffectContext<'_, U>,
    ) -> Result<Response, EffectError> {
        if tag == 0 {
            let req = H::Request::from_value(request, cx.table())?;
            self.head.handle(req, cx)
        } else {
            self.tail.dispatch(tag - 1, request, cx)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frunk::hlist;
    use tidepool_repr::types::Literal;

    /// Handler that adds 1 to a LitInt request.
    struct AddOneHandler;
    impl EffectHandler for AddOneHandler {
        type Request = Value;
        fn handle(&mut self, req: Value, _cx: &EffectContext) -> Result<Response, EffectError> {
            match req {
                Value::Lit(Literal::LitInt(n)) => Ok(Value::Lit(Literal::LitInt(n + 1)).into()),
                other => Err(EffectError::UnexpectedValue {
                    context: "LitInt",
                    got: format!("{other:?}"),
                }),
            }
        }
    }

    /// Handler that doubles a LitInt request.
    struct DoubleHandler;
    impl EffectHandler for DoubleHandler {
        type Request = Value;
        fn handle(&mut self, req: Value, _cx: &EffectContext) -> Result<Response, EffectError> {
            match req {
                Value::Lit(Literal::LitInt(n)) => Ok(Value::Lit(Literal::LitInt(n * 2)).into()),
                other => Err(EffectError::UnexpectedValue {
                    context: "LitInt",
                    got: format!("{other:?}"),
                }),
            }
        }
    }

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
    fn hnil_rejects_all_tags() {
        let table = empty_table();
        let cx = make_cx(&table);
        let result = HNil.dispatch(0, &lit_int(5), &cx);
        match result {
            Err(EffectError::UnhandledEffect { tag: 0 }) => {}
            other => panic!("expected UnhandledEffect {{ tag: 0 }}, got {other:?}"),
        }
    }

    #[test]
    fn single_handler_routes_tag_0() {
        let table = empty_table();
        let cx = make_cx(&table);
        let mut handlers = hlist![AddOneHandler];
        let result = handlers.dispatch(0, &lit_int(10), &cx).unwrap();
        match result {
            Response::Complete(Value::Lit(Literal::LitInt(11))) => {}
            other => panic!("expected LitInt(11), got {other:?}"),
        }
    }

    #[test]
    fn single_handler_rejects_tag_1() {
        let table = empty_table();
        let cx = make_cx(&table);
        let mut handlers = hlist![AddOneHandler];
        let result = handlers.dispatch(1, &lit_int(10), &cx);
        match result {
            Err(EffectError::UnhandledEffect { tag: 0 }) => {}
            // tag is decremented per HCons layer, so HNil sees 0
            other => panic!("expected UnhandledEffect {{ tag: 0 }}, got {other:?}"),
        }
    }

    #[test]
    fn two_handlers_route_tag_0_to_head() {
        let table = empty_table();
        let cx = make_cx(&table);
        let mut handlers = hlist![AddOneHandler, DoubleHandler];
        let result = handlers.dispatch(0, &lit_int(5), &cx).unwrap();
        match result {
            Response::Complete(Value::Lit(Literal::LitInt(6))) => {} // 5 + 1
            other => panic!("expected LitInt(6), got {other:?}"),
        }
    }

    #[test]
    fn two_handlers_route_tag_1_to_tail() {
        let table = empty_table();
        let cx = make_cx(&table);
        let mut handlers = hlist![AddOneHandler, DoubleHandler];
        let result = handlers.dispatch(1, &lit_int(5), &cx).unwrap();
        match result {
            Response::Complete(Value::Lit(Literal::LitInt(10))) => {} // 5 * 2
            other => panic!("expected LitInt(10), got {other:?}"),
        }
    }

    #[test]
    fn two_handlers_reject_tag_2() {
        let table = empty_table();
        let cx = make_cx(&table);
        let mut handlers = hlist![AddOneHandler, DoubleHandler];
        let result = handlers.dispatch(2, &lit_int(5), &cx);
        match result {
            Err(EffectError::UnhandledEffect { tag: 0 }) => {}
            // tag decremented by 2 (one per HCons), so HNil sees 0
            other => panic!("expected UnhandledEffect {{ tag: 0 }}, got {other:?}"),
        }
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
