use frunk::hlist;
use proptest::prelude::*;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext, EffectHandler};
use tidepool_effect::error::EffectError;
use tidepool_eval::value::Value;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::Literal;

/// Mock handler that returns its own ID as a response.
struct MockHandler {
    id: u64,
}

impl EffectHandler<()> for MockHandler {
    type Request = Value;
    fn handle(&mut self, _req: Self::Request, _cx: &EffectContext<'_, ()>) -> Result<Value, EffectError> {
        Ok(Value::Lit(Literal::LitInt(self.id as i64)))
    }
}

/// Helper to create a dummy DataConTable.
fn empty_table() -> DataConTable {
    DataConTable::new()
}

proptest! {
    /// Test that dispatching with tag K routes to the K-th handler in the HList.
    #[test]
    fn dispatch_routes_by_tag(tag in 0u64..3u64) {
        let mut h3 = hlist![
            MockHandler { id: 0 },
            MockHandler { id: 1 },
            MockHandler { id: 2 }
        ];
        
        let table = empty_table();
        let cx = EffectContext::with_user(&table, &());
        let req = Value::Lit(Literal::LitInt(42));
        
        let res = h3.dispatch(tag, &req, &cx).unwrap();
        if let Value::Lit(Literal::LitInt(id)) = res {
            prop_assert_eq!(id, tag as i64);
        } else {
            panic!("Unexpected response: {:?}", res);
        }
    }

    /// Test that dispatching with a tag beyond the HList length returns an error.
    /// The error tag should be relative to the point of failure (HNil).
    #[test]
    fn unknown_tag_returns_error(tag in 3u64..100u64) {
        let mut h3 = hlist![
            MockHandler { id: 0 },
            MockHandler { id: 1 },
            MockHandler { id: 2 }
        ];
        
        let table = empty_table();
        let cx = EffectContext::with_user(&table, &());
        let req = Value::Lit(Literal::LitInt(42));
        
        let res = h3.dispatch(tag, &req, &cx);
        prop_assert!(res.is_err());
        match res {
            Err(EffectError::UnhandledEffect { tag: actual_tag }) => {
                // Each HCons decrements the tag. After 3 handlers, tag becomes tag - 3.
                prop_assert_eq!(actual_tag, tag - 3);
            }
            other => panic!("Expected UnhandledEffect, got {:?}", other),
        }
    }

    /// Test that the handler receives the exact request Value passed to dispatch.
    #[test]
    fn handler_receives_correct_request(req_val in any::<i64>()) {
        struct EchoHandler;
        impl EffectHandler<()> for EchoHandler {
            type Request = Value;
            fn handle(&mut self, req: Self::Request, _cx: &EffectContext<'_, ()>) -> Result<Value, EffectError> {
                Ok(req)
            }
        }
        
        let mut handlers = hlist![EchoHandler];
        let table = empty_table();
        let cx = EffectContext::with_user(&table, &());
        let req = Value::Lit(Literal::LitInt(req_val));
        
        let res = handlers.dispatch(0, &req, &cx).unwrap();
        // Compare using Debug representation as Value doesn't implement PartialEq
        prop_assert_eq!(format!("{:?}", res), format!("{:?}", req));
    }

    /// Test that dispatch routing is consistent even if we change handler order and tags.
    #[test]
    fn hlist_order_consistent(val in any::<i64>()) {
        let mut h_ab = hlist![
            MockHandler { id: 10 },
            MockHandler { id: 20 }
        ];
        let mut h_ba = hlist![
            MockHandler { id: 20 },
            MockHandler { id: 10 }
        ];
        
        let table = empty_table();
        let cx = EffectContext::with_user(&table, &());
        let req = Value::Lit(Literal::LitInt(val));
        
        // Handler with id 10 is at tag 0 in h_ab, and tag 1 in h_ba.
        let res_a0 = h_ab.dispatch(0, &req, &cx).unwrap();
        let res_a1 = h_ba.dispatch(1, &req, &cx).unwrap();
        prop_assert_eq!(format!("{:?}", res_a0), format!("{:?}", res_a1));
        
        // Handler with id 20 is at tag 1 in h_ab, and tag 0 in h_ba.
        let res_b1 = h_ab.dispatch(1, &req, &cx).unwrap();
        let res_b0 = h_ba.dispatch(0, &req, &cx).unwrap();
        prop_assert_eq!(format!("{:?}", res_b1), format!("{:?}", res_b0));
    }
}