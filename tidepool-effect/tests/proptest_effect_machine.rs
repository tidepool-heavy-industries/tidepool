use frunk::{hlist, HNil};
use proptest::prelude::*;
use tidepool_bridge::{BridgeError, FromCore};
use tidepool_effect::dispatch::{EffectContext, EffectHandler, Response};
use tidepool_effect::error::EffectError;
use tidepool_effect::machine::EffectMachine;
use tidepool_eval::heap::VecHeap;
use tidepool_eval::value::Value;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::types::{DataConId, Literal, PrimOpKind, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, RecursiveTree};
use tidepool_testing::gen::freer_effect_test_table as make_test_table;

struct CountingHandler {
    count: u32,
    responses: Vec<i64>,
}

impl CountingHandler {
    fn new(responses: Vec<i64>) -> Self {
        Self {
            count: 0,
            responses,
        }
    }
}

impl EffectHandler<()> for CountingHandler {
    type Request = Value;
    fn handle(
        &mut self,
        _req: Self::Request,
        _cx: &EffectContext<'_, ()>,
    ) -> Result<Response, EffectError> {
        let idx = self.count as usize;
        self.count += 1;
        let resp = self.responses.get(idx).copied().unwrap_or(0);
        Ok(Value::Lit(Literal::LitInt(resp)).into())
    }
}

/// Build: E(Union(tag, Lit(request)), Leaf(\x -> Val(x)))
fn make_single_effect(tag: u64, request_val: i64) -> CoreExpr {
    RecursiveTree {
        nodes: vec![
            CoreFrame::Var(VarId(100)), // 0
            CoreFrame::Con {
                tag: DataConId(1),
                fields: vec![0],
            }, // 1: Val(x)
            CoreFrame::Lam {
                binder: VarId(100),
                body: 1,
            }, // 2: \x -> Val(x)
            CoreFrame::Con {
                tag: DataConId(3),
                fields: vec![2],
            }, // 3: Leaf(...)
            CoreFrame::Lit(Literal::LitInt(request_val)), // 4: request
            CoreFrame::Lit(Literal::LitWord(tag)), // 5: tag
            CoreFrame::Con {
                tag: DataConId(5),
                fields: vec![5, 4],
            }, // 6: Union(tag, req)
            CoreFrame::Con {
                tag: DataConId(2),
                fields: vec![6, 3],
            }, // 7: E(union, k)
        ],
    }
}

const ROUTE_A: DataConId = DataConId(10);
const ROUTE_B: DataConId = DataConId(11);
const ROUTE_C: DataConId = DataConId(12);

fn routing_table() -> DataConTable {
    let mut table = make_test_table();
    for (id, name) in [
        (ROUTE_A, "RouteA"),
        (ROUTE_B, "RouteB"),
        (ROUTE_C, "RouteC"),
    ] {
        table.insert(DataCon {
            id,
            name: name.into(),
            tag: id.0 as u32,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some(format!("Test.{name}")),
            type_name: "RouteRequest".into(),
        });
    }
    table
}

struct RouteRequest<const ID: u64>;
impl<const ID: u64> tidepool_bridge::sealed::FromCoreSealed for RouteRequest<ID> {}
impl<const ID: u64> FromCore for RouteRequest<ID> {
    fn from_value(value: &Value, _table: &DataConTable) -> Result<Self, BridgeError> {
        match value {
            Value::Con(id, fields) if *id == DataConId(ID) && fields.is_empty() => Ok(Self),
            Value::Con(id, fields) if *id == DataConId(ID) => Err(BridgeError::ArityMismatch {
                con: *id,
                expected: 0,
                got: fields.len(),
            }),
            Value::Con(id, _) => Err(BridgeError::UnknownDataCon(*id)),
            other => Err(BridgeError::TypeMismatch {
                expected: "request constructor".into(),
                got: format!("{other:?}"),
            }),
        }
    }
}

struct RouteHandler<const ID: u64>(i64);
impl<const ID: u64> EffectHandler<()> for RouteHandler<ID> {
    type Request = RouteRequest<ID>;
    fn handle(
        &mut self,
        _req: Self::Request,
        _cx: &EffectContext<'_, ()>,
    ) -> Result<Response, EffectError> {
        Ok(Value::Lit(Literal::LitInt(self.0)).into())
    }
}

/// Build `E (Union carrierTag RequestConstructor) (Leaf (\x -> Val x))`.
fn make_routed_effect(carrier_tag: u64, request_id: DataConId) -> CoreExpr {
    RecursiveTree {
        nodes: vec![
            CoreFrame::Var(VarId(100)),
            CoreFrame::Con {
                tag: DataConId(1),
                fields: vec![0],
            },
            CoreFrame::Lam {
                binder: VarId(100),
                body: 1,
            },
            CoreFrame::Con {
                tag: DataConId(3),
                fields: vec![2],
            },
            CoreFrame::Con {
                tag: request_id,
                fields: vec![],
            },
            CoreFrame::Lit(Literal::LitWord(carrier_tag)),
            CoreFrame::Con {
                tag: DataConId(5),
                fields: vec![5, 4],
            },
            CoreFrame::Con {
                tag: DataConId(2),
                fields: vec![6, 3],
            },
        ],
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]
    #[test]
    fn pure_val_random(val in any::<i64>()) {
        let table = make_test_table();
        let mut heap = VecHeap::new();
        let expr = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitInt(val)),
                CoreFrame::Con {
                    tag: DataConId(1), // Val
                    fields: vec![0],
                },
            ],
        };
        let mut handlers = HNil;
        let mut machine = EffectMachine::new(&table, &mut heap).unwrap();
        let res = machine.run(&expr, &mut handlers).unwrap();
        if let Value::Lit(Literal::LitInt(n)) = res {
            prop_assert_eq!(n, val);
        } else {
            panic!("Expected LitInt({}), got {:?}", val, res);
        }
    }

    #[test]
    fn single_effect_random_values(req in any::<i64>(), resp in any::<i64>()) {
        let table = make_test_table();
        let mut heap = VecHeap::new();
        let expr = make_single_effect(0, req);
        let handler = CountingHandler::new(vec![resp]);
        let mut handlers = hlist![handler];
        let mut machine = EffectMachine::new(&table, &mut heap).unwrap();
        let res = machine.run(&expr, &mut handlers).unwrap();
        if let Value::Lit(Literal::LitInt(n)) = res {
            prop_assert_eq!(n, resp);
        } else {
            panic!("Expected LitInt({}), got {:?}", resp, res);
        }
        prop_assert_eq!(handlers.head.count, 1);
    }

    #[test]
    fn multi_handler_routing(route in 0usize..3, carrier_tag in any::<u64>()) {
        let table = routing_table();
        let mut heap = VecHeap::new();
        let request_id = [ROUTE_A, ROUTE_B, ROUTE_C][route];
        let expr = make_routed_effect(carrier_tag, request_id);
        let mut handlers = hlist![
            RouteHandler::<10>(10),
            RouteHandler::<11>(20),
            RouteHandler::<12>(30)
        ];
        let mut machine = EffectMachine::new(&table, &mut heap).unwrap();
        let res = machine.run(&expr, &mut handlers).unwrap();
        let expected = (route as i64 + 1) * 10;
        if let Value::Lit(Literal::LitInt(n)) = res {
            prop_assert_eq!(n, expected);
        } else {
            panic!("Expected LitInt({}), got {:?}", expected, res);
        }
    }

    #[test]
    fn handler_state_accumulation(r1 in any::<i64>(), r2 in any::<i64>()) {
        let table = make_test_table();
        let mut heap = VecHeap::new();
        // E(Union(0, r1), Leaf(\_ -> E(Union(0, r2), Leaf(\_ -> Val(0)))))
        let expr = RecursiveTree { nodes: vec![
            CoreFrame::Lit(Literal::LitInt(0)),                              // 0
            CoreFrame::Con { tag: DataConId(1), fields: vec![0] },           // 1: Val(0)
            CoreFrame::Lam { binder: VarId(200), body: 1 },                 // 2: \_ -> Val(0)
            CoreFrame::Con { tag: DataConId(3), fields: vec![2] },           // 3: Leaf(...)
            CoreFrame::Lit(Literal::LitInt(r2)),                             // 4: req2
            CoreFrame::Lit(Literal::LitWord(0)),                             // 5: tag 0
            CoreFrame::Con { tag: DataConId(5), fields: vec![5, 4] },        // 6: Union(0, r2)
            CoreFrame::Con { tag: DataConId(2), fields: vec![6, 3] },        // 7: E(Union(0,r2), Leaf(\_->Val(0)))
            CoreFrame::Lam { binder: VarId(100), body: 7 },                 // 8: \_ -> inner_effect
            CoreFrame::Con { tag: DataConId(3), fields: vec![8] },           // 9: Leaf(\_ -> inner)
            CoreFrame::Lit(Literal::LitInt(r1)),                             // 10: req1
            CoreFrame::Lit(Literal::LitWord(0)),                             // 11: tag 0
            CoreFrame::Con { tag: DataConId(5), fields: vec![11, 10] },      // 12: Union(0, r1)
            CoreFrame::Con { tag: DataConId(2), fields: vec![12, 9] },       // 13: E(Union(0,r1), ...)
        ]};

        let handler = CountingHandler::new(vec![100, 200]);
        let mut handlers = hlist![handler];
        let mut machine = EffectMachine::new(&table, &mut heap).unwrap();
        let res = machine.run(&expr, &mut handlers).unwrap();

        if let Value::Lit(Literal::LitInt(n)) = res {
            prop_assert_eq!(n, 0);
        } else {
            panic!("Expected LitInt(0), got {:?}", res);
        }
        prop_assert_eq!(handlers.head.count, 2);
    }

    #[test]
    fn continuation_uses_response(req_val in any::<i64>(), resp in any::<i64>()) {
        let table = make_test_table();
        let mut heap = VecHeap::new();
        // E(Union(0, req), Leaf(\x -> Val(x +# 100#)))
        let expr = RecursiveTree { nodes: vec![
            CoreFrame::Var(VarId(100)),                                      // 0: x
            CoreFrame::Lit(Literal::LitInt(100)),                            // 1: 100#
            CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![0, 1] }, // 2: x + 100
            CoreFrame::Con { tag: DataConId(1), fields: vec![2] },           // 3: Val(x + 100)
            CoreFrame::Lam { binder: VarId(100), body: 3 },                 // 4: \x -> Val(x+100)
            CoreFrame::Con { tag: DataConId(3), fields: vec![4] },           // 5: Leaf(...)
            CoreFrame::Lit(Literal::LitInt(req_val)),                        // 6: request
            CoreFrame::Lit(Literal::LitWord(0)),                             // 7: tag 0
            CoreFrame::Con { tag: DataConId(5), fields: vec![7, 6] },        // 8: Union(0, req)
            CoreFrame::Con { tag: DataConId(2), fields: vec![8, 5] },        // 9: E(union, k)
        ]};

        let handler = CountingHandler::new(vec![resp]);
        let mut handlers = hlist![handler];
        let mut machine = EffectMachine::new(&table, &mut heap).unwrap();
        let res = machine.run(&expr, &mut handlers).unwrap();

        let expected = resp.wrapping_add(100);
        if let Value::Lit(Literal::LitInt(n)) = res {
            prop_assert_eq!(n, expected);
        } else {
            panic!("Expected LitInt({}), got {:?}", expected, res);
        }
    }
}
