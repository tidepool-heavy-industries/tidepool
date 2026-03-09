use crate::dispatch::{DispatchEffect, EffectContext};
use crate::error::EffectError;
use tidepool_eval::heap::Heap;
use tidepool_eval::value::Value;
use tidepool_repr::{CoreExpr, DataConId, DataConTable};

pub struct EffectMachine<'a> {
    table: &'a DataConTable,
    heap: &'a mut dyn Heap,
    val_id: DataConId,
    e_id: DataConId,
    leaf_id: DataConId,
    node_id: DataConId,
    union_id: DataConId,
}

impl<'a> EffectMachine<'a> {
    pub fn new(table: &'a DataConTable, heap: &'a mut dyn Heap) -> Result<Self, EffectError> {
        let val_id = table
            .get_by_name("Val")
            .ok_or(EffectError::MissingConstructor { name: "Val" })?;
        let e_id = table
            .get_by_name("E")
            .ok_or(EffectError::MissingConstructor { name: "E" })?;
        let leaf_id = table
            .get_by_name("Leaf")
            .ok_or(EffectError::MissingConstructor { name: "Leaf" })?;
        let node_id = table
            .get_by_name("Node")
            .ok_or(EffectError::MissingConstructor { name: "Node" })?;
        let union_id = table
            .get_by_name("Union")
            .ok_or(EffectError::MissingConstructor { name: "Union" })?;
        Ok(Self {
            table,
            heap,
            val_id,
            e_id,
            leaf_id,
            node_id,
            union_id,
        })
    }

    /// Run an Eff expression to completion with the given handler HList.
    /// Backward-compatible: uses U=() (no user data).
    pub fn run<H: DispatchEffect>(
        &mut self,
        expr: &CoreExpr,
        handlers: &mut H,
    ) -> Result<Value, EffectError> {
        self.run_with_user(expr, handlers, &())
    }

    /// Run an Eff expression with user data threaded through to handlers.
    pub fn run_with_user<U, H: DispatchEffect<U>>(
        &mut self,
        expr: &CoreExpr,
        handlers: &mut H,
        user: &U,
    ) -> Result<Value, EffectError> {
        let env = tidepool_eval::eval::env_from_datacon_table(self.table);
        let mut current = tidepool_eval::eval::eval(expr, &env, self.heap)?;

        loop {
            let forced = tidepool_eval::eval::force(current, self.heap)?;
            match forced {
                Value::Con(id, ref fields) if id == self.val_id => {
                    // Val x — pure result, done. Deep force to eliminate any ThunkRefs.
                    let result = fields
                        .first()
                        .cloned()
                        .unwrap_or(Value::Lit(tidepool_repr::Literal::LitInt(0)));
                    return Ok(tidepool_eval::eval::deep_force(result, self.heap)?);
                }
                Value::Con(id, ref fields) if id == self.e_id => {
                    // E (Union tag# req) k
                    if fields.len() != 2 {
                        return Err(EffectError::FieldCountMismatch {
                            constructor: "E",
                            expected: 2,
                            got: fields.len(),
                        });
                    }
                    let union_val = tidepool_eval::eval::deep_force(fields[0].clone(), self.heap)?;
                    let k = tidepool_eval::eval::force(fields[1].clone(), self.heap)?;

                    // Destructure Union(tag, req)
                    let (tag, request) = match union_val {
                        Value::Con(uid, ref ufields) if uid == self.union_id => {
                            if ufields.len() != 2 {
                                return Err(EffectError::FieldCountMismatch {
                                    constructor: "Union",
                                    expected: 2,
                                    got: ufields.len(),
                                });
                            }
                            let tag = match &ufields[0] {
                                Value::Lit(tidepool_repr::Literal::LitWord(w)) => *w,
                                Value::Lit(tidepool_repr::Literal::LitInt(i)) => *i as u64,
                                other => {
                                    return Err(EffectError::UnexpectedValue {
                                        context: "Union tag (Word#/Int#)",
                                        got: format!("{:?}", other),
                                    })
                                }
                            };
                            // deep_force the request so FromCore never sees ThunkRef
                            let req = ufields[1].clone();
                            (tag, req)
                        }
                        other => {
                            return Err(EffectError::UnexpectedValue {
                                context: "Union constructor",
                                got: format!("{:?}", other),
                            })
                        }
                    };

                    // Dispatch to handler
                    let cx = EffectContext::with_user(self.table, user);
                    let response = handlers.dispatch(tag, &request, &cx)?;

                    // Apply continuation
                    current = self.apply_cont(k, response)?;
                }
                other => {
                    return Err(EffectError::UnexpectedValue {
                        context: "Val or E constructor",
                        got: format!("{:?}", other),
                    });
                }
            }
        }
    }

    /// Apply a Leaf/Node continuation tree to a value.
    fn apply_cont(&mut self, k: Value, arg: Value) -> Result<Value, EffectError> {
        let k = tidepool_eval::eval::force(k, self.heap)?;
        match k {
            Value::Con(id, ref fields) if id == self.leaf_id => {
                // Leaf(f) — apply f to arg
                if fields.len() != 1 {
                    return Err(EffectError::FieldCountMismatch {
                        constructor: "Leaf",
                        expected: 1,
                        got: fields.len(),
                    });
                }
                let f = tidepool_eval::eval::force(fields[0].clone(), self.heap)?;
                Ok(self.apply_closure(f, arg)?)
            }
            Value::Con(id, ref fields) if id == self.node_id => {
                // Node(k1, k2) — apply k1, then compose with k2
                if fields.len() != 2 {
                    return Err(EffectError::FieldCountMismatch {
                        constructor: "Node",
                        expected: 2,
                        got: fields.len(),
                    });
                }
                let k1 = fields[0].clone();
                let k2 = fields[1].clone();
                let result = self.apply_cont(k1, arg)?;
                let forced = tidepool_eval::eval::force(result, self.heap)?;

                match forced {
                    Value::Con(vid, ref vfields) if vid == self.val_id => {
                        // k1 returned Val(y) — feed y to k2
                        let y = vfields
                            .first()
                            .cloned()
                            .unwrap_or(Value::Lit(tidepool_repr::Literal::LitInt(0)));
                        self.apply_cont(k2, y)
                    }
                    Value::Con(eid, ref efields) if eid == self.e_id => {
                        // k1 yielded E(union, k') — compose: E(union, Node(k', k2))
                        if efields.len() != 2 {
                            return Err(EffectError::FieldCountMismatch {
                                constructor: "E (continuation)",
                                expected: 2,
                                got: efields.len(),
                            });
                        }
                        let union_val = efields[0].clone();
                        let k_prime = efields[1].clone();
                        let new_k = Value::Con(self.node_id, vec![k_prime, k2]);
                        Ok(Value::Con(self.e_id, vec![union_val, new_k]))
                    }
                    other => Err(EffectError::UnexpectedValue {
                        context: "Val or E after applying k1",
                        got: format!("{:?}", other),
                    }),
                }
            }
            Value::Closure(..) => {
                // Raw closure (degenerate continuation)
                Ok(self.apply_closure(k, arg)?)
            }
            other => Err(EffectError::UnexpectedValue {
                context: "Leaf or Node continuation",
                got: format!("{:?}", other),
            }),
        }
    }

    /// Apply a single closure to a value.
    fn apply_closure(&mut self, closure: Value, arg: Value) -> Result<Value, EffectError> {
        match closure {
            Value::Closure(env, binder, body) => {
                let new_env = env.update(binder, arg);
                Ok(tidepool_eval::eval::eval(&body, &new_env, self.heap)?)
            }
            Value::ConFun(tag, arity, mut args) => {
                args.push(arg);
                if args.len() == arity {
                    Ok(Value::Con(tag, args))
                } else {
                    Ok(Value::ConFun(tag, arity, args))
                }
            }
            other => Err(EffectError::UnexpectedValue {
                context: "closure",
                got: format!("{:?}", other),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_eval::heap::VecHeap;
    use tidepool_repr::datacon::DataCon;
    use tidepool_repr::datacon_table::DataConTable;
    use tidepool_repr::types::{DataConId, Literal, VarId};
    use tidepool_repr::{CoreExpr, CoreFrame, RecursiveTree};

    fn make_test_table() -> DataConTable {
        let mut table = DataConTable::new();
        table.insert(DataCon {
            id: DataConId(1),
            name: "Val".to_string(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![],
            qualified_name: None,
        });
        table.insert(DataCon {
            id: DataConId(2),
            name: "E".to_string(),
            tag: 2,
            rep_arity: 2,
            field_bangs: vec![],
            qualified_name: None,
        });
        table.insert(DataCon {
            id: DataConId(3),
            name: "Leaf".to_string(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![],
            qualified_name: None,
        });
        table.insert(DataCon {
            id: DataConId(4),
            name: "Node".to_string(),
            tag: 2,
            rep_arity: 2,
            field_bangs: vec![],
            qualified_name: None,
        });
        table.insert(DataCon {
            id: DataConId(5),
            name: "Union".to_string(),
            tag: 1,
            rep_arity: 2,
            field_bangs: vec![],
            qualified_name: None,
        });
        table
    }

    #[test]
    fn test_effect_machine_pure_val() {
        // Eff expression that is just Val(42)
        let table = make_test_table();
        let mut heap = VecHeap::new();

        // Build: Con(Val, [Lit(42)])
        let expr: CoreExpr = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitInt(42)),
                CoreFrame::Con {
                    tag: DataConId(1), // Val
                    fields: vec![0],
                },
            ],
        };

        let mut handlers = frunk::HNil;
        let mut machine = EffectMachine::new(&table, &mut heap).unwrap();
        let result = machine.run(&expr, &mut handlers).unwrap();

        match result {
            Value::Lit(Literal::LitInt(n)) => assert_eq!(n, 42),
            other => panic!("Expected Lit(42), got {:?}", other),
        }
    }

    #[test]
    fn test_effect_machine_single_effect() {
        // Build: E(Union(0, Lit(99)), Leaf(\x -> Val(x)))
        // Handler at tag 0 receives Lit(99), returns Lit(100)
        let table = make_test_table();
        let mut heap = VecHeap::new();

        let expr: CoreExpr = RecursiveTree {
            nodes: vec![
                // 0: Var(x) -- will be the Val payload
                CoreFrame::Var(VarId(100)),
                // 1: Con(Val, [Var(x)]) -- Val(x)
                CoreFrame::Con {
                    tag: DataConId(1), // Val
                    fields: vec![0],
                },
                // 2: Lam(x, Con(Val, [Var(x)])) -- \x -> Val(x)
                CoreFrame::Lam {
                    binder: VarId(100),
                    body: 1,
                },
                // 3: Con(Leaf, [lam]) -- Leaf(\x -> Val(x))
                CoreFrame::Con {
                    tag: DataConId(3), // Leaf
                    fields: vec![2],
                },
                // 4: Lit(99) -- the request
                CoreFrame::Lit(Literal::LitInt(99)),
                // 5: Lit(0) -- tag Word# 0
                CoreFrame::Lit(Literal::LitWord(0)),
                // 6: Con(Union, [tag, req]) -- Union(0, 99)
                CoreFrame::Con {
                    tag: DataConId(5), // Union
                    fields: vec![5, 4],
                },
                // 7: Con(E, [union, k]) -- E(Union(0, 99), Leaf(\x -> Val(x)))
                CoreFrame::Con {
                    tag: DataConId(2), // E
                    fields: vec![6, 3],
                },
            ],
        };

        // Simple handler: receives any value, returns Lit(100)
        use crate::dispatch::{EffectContext, EffectHandler};
        use tidepool_bridge::FromCore;

        struct TestReq(i64);
        impl FromCore for TestReq {
            fn from_value(
                value: &Value,
                _table: &DataConTable,
            ) -> Result<Self, tidepool_bridge::BridgeError> {
                match value {
                    Value::Lit(Literal::LitInt(n)) => Ok(TestReq(*n)),
                    _ => Err(tidepool_bridge::BridgeError::TypeMismatch {
                        expected: "LitInt".into(),
                        got: format!("{:?}", value),
                    }),
                }
            }
        }

        struct TestHandler;
        impl EffectHandler for TestHandler {
            type Request = TestReq;
            fn handle(&mut self, req: TestReq, _cx: &EffectContext) -> Result<Value, EffectError> {
                // Echo back the request + 1
                Ok(Value::Lit(Literal::LitInt(req.0 + 1)))
            }
        }

        let mut handlers = frunk::hlist![TestHandler];
        let mut machine = EffectMachine::new(&table, &mut heap).unwrap();
        let result = machine.run(&expr, &mut handlers).unwrap();

        match result {
            Value::Lit(Literal::LitInt(n)) => assert_eq!(n, 100),
            other => panic!("Expected Lit(100), got {:?}", other),
        }
    }

    #[test]
    fn test_run_with_user_data() {
        // Same as single_effect but handler reads user data to compute response
        let table = make_test_table();
        let mut heap = VecHeap::new();

        let expr: CoreExpr = RecursiveTree {
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
                CoreFrame::Lit(Literal::LitInt(10)),
                CoreFrame::Lit(Literal::LitWord(0)),
                CoreFrame::Con {
                    tag: DataConId(5),
                    fields: vec![5, 4],
                },
                CoreFrame::Con {
                    tag: DataConId(2),
                    fields: vec![6, 3],
                },
            ],
        };

        use crate::dispatch::{EffectContext, EffectHandler};
        use tidepool_bridge::FromCore;

        struct TestReq(i64);
        impl FromCore for TestReq {
            fn from_value(
                value: &Value,
                _table: &DataConTable,
            ) -> Result<Self, tidepool_bridge::BridgeError> {
                match value {
                    Value::Lit(Literal::LitInt(n)) => Ok(TestReq(*n)),
                    _ => Err(tidepool_bridge::BridgeError::TypeMismatch {
                        expected: "LitInt".into(),
                        got: format!("{:?}", value),
                    }),
                }
            }
        }

        struct UserData {
            multiplier: i64,
        }

        struct UserHandler;
        impl EffectHandler<UserData> for UserHandler {
            type Request = TestReq;
            fn handle(
                &mut self,
                req: TestReq,
                cx: &EffectContext<'_, UserData>,
            ) -> Result<Value, EffectError> {
                Ok(Value::Lit(Literal::LitInt(req.0 * cx.user().multiplier)))
            }
        }

        let user = UserData { multiplier: 5 };
        let mut handlers = frunk::hlist![UserHandler];
        let mut machine = EffectMachine::new(&table, &mut heap).unwrap();
        let result = machine.run_with_user(&expr, &mut handlers, &user).unwrap();

        match result {
            Value::Lit(Literal::LitInt(n)) => assert_eq!(n, 50), // 10 * 5
            other => panic!("Expected Lit(50), got {:?}", other),
        }
    }
}
