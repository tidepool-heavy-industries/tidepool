use crate::dispatch::DispatchEffect;
use crate::error::EffectError;
use core_eval::heap::Heap;
use core_eval::value::Value;
use core_repr::{CoreExpr, DataConId, DataConTable};

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
            .ok_or_else(|| {
                EffectError::BadUnion("Val constructor not found in DataConTable".into())
            })?;
        let e_id = table
            .get_by_name("E")
            .ok_or_else(|| {
                EffectError::BadUnion("E constructor not found in DataConTable".into())
            })?;
        let leaf_id = table
            .get_by_name("Leaf")
            .ok_or_else(|| EffectError::BadContinuation("Leaf constructor not found".into()))?;
        let node_id = table
            .get_by_name("Node")
            .ok_or_else(|| EffectError::BadContinuation("Node constructor not found".into()))?;
        let union_id = table
            .get_by_name("Union")
            .ok_or_else(|| EffectError::BadUnion("Union constructor not found".into()))?;
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
    pub fn run<H: DispatchEffect>(
        &mut self,
        expr: &CoreExpr,
        handlers: &mut H,
    ) -> Result<Value, EffectError> {
        let env = core_eval::eval::env_from_datacon_table(self.table);
        let mut current = core_eval::eval::eval(expr, &env, self.heap)?;

        loop {
            let forced = core_eval::eval::force(current, self.heap)?;
            match forced {
                Value::Con(id, ref fields) if id == self.val_id => {
                    // Val x — pure result, done
                    return Ok(fields
                        .first()
                        .cloned()
                        .unwrap_or(Value::Lit(core_repr::Literal::LitInt(0))));
                }
                Value::Con(id, ref fields) if id == self.e_id => {
                    // E (Union tag# req) k
                    if fields.len() != 2 {
                        return Err(EffectError::BadUnion(format!(
                            "E expects 2 fields, got {}",
                            fields.len()
                        )));
                    }
                    let union_val = core_eval::eval::force(fields[0].clone(), self.heap)?;
                    let k = core_eval::eval::force(fields[1].clone(), self.heap)?;

                    // Destructure Union(tag, req)
                    let (tag, request) = match union_val {
                        Value::Con(uid, ref ufields) if uid == self.union_id => {
                            if ufields.len() != 2 {
                                return Err(EffectError::BadUnion(format!(
                                    "Union expects 2 fields, got {}",
                                    ufields.len()
                                )));
                            }
                            let tag =
                                match core_eval::eval::force(ufields[0].clone(), self.heap)? {
                                    Value::Lit(core_repr::Literal::LitWord(w)) => w,
                                    Value::Lit(core_repr::Literal::LitInt(i)) => i as u64,
                                    other => {
                                        return Err(EffectError::BadUnion(format!(
                                            "Union tag must be Word#/Int#, got {:?}",
                                            other
                                        )))
                                    }
                                };
                            let req =
                                core_eval::eval::force(ufields[1].clone(), self.heap)?;
                            (tag, req)
                        }
                        other => {
                            return Err(EffectError::BadUnion(format!(
                                "Expected Union constructor, got {:?}",
                                other
                            )))
                        }
                    };

                    // Dispatch to handler
                    let response = handlers.dispatch(tag, &request, self.table)?;

                    // Apply continuation
                    current = self.apply_cont(k, response)?;
                }
                other => {
                    return Err(EffectError::BadUnion(format!(
                        "Expected Val or E constructor, got {:?}",
                        other
                    )));
                }
            }
        }
    }

    /// Apply a Leaf/Node continuation tree to a value.
    fn apply_cont(&mut self, k: Value, arg: Value) -> Result<Value, EffectError> {
        let k = core_eval::eval::force(k, self.heap)?;
        match k {
            Value::Con(id, ref fields) if id == self.leaf_id => {
                // Leaf(f) — apply f to arg
                if fields.len() != 1 {
                    return Err(EffectError::BadContinuation(format!(
                        "Leaf expects 1 field, got {}",
                        fields.len()
                    )));
                }
                let f = core_eval::eval::force(fields[0].clone(), self.heap)?;
                Ok(self.apply_closure(f, arg)?)
            }
            Value::Con(id, ref fields) if id == self.node_id => {
                // Node(k1, k2) — apply k1, then compose with k2
                if fields.len() != 2 {
                    return Err(EffectError::BadContinuation(format!(
                        "Node expects 2 fields, got {}",
                        fields.len()
                    )));
                }
                let k1 = fields[0].clone();
                let k2 = fields[1].clone();
                let result = self.apply_cont(k1, arg)?;
                let forced = core_eval::eval::force(result, self.heap)?;

                match forced {
                    Value::Con(vid, ref vfields) if vid == self.val_id => {
                        // k1 returned Val(y) — feed y to k2
                        let y = vfields
                            .first()
                            .cloned()
                            .unwrap_or(Value::Lit(core_repr::Literal::LitInt(0)));
                        self.apply_cont(k2, y)
                    }
                    Value::Con(eid, ref efields) if eid == self.e_id => {
                        // k1 yielded E(union, k') — compose: E(union, Node(k', k2))
                        if efields.len() != 2 {
                            return Err(EffectError::BadContinuation(
                                "E in continuation expects 2 fields".into(),
                            ));
                        }
                        let union_val = efields[0].clone();
                        let k_prime = efields[1].clone();
                        let new_k = Value::Con(self.node_id, vec![k_prime, k2]);
                        Ok(Value::Con(self.e_id, vec![union_val, new_k]))
                    }
                    other => Err(EffectError::BadContinuation(format!(
                        "Expected Val or E after applying k1, got {:?}",
                        other
                    ))),
                }
            }
            Value::Closure(..) => {
                // Raw closure (degenerate continuation)
                Ok(self.apply_closure(k, arg)?)
            }
            other => Err(EffectError::BadContinuation(format!(
                "Expected Leaf or Node, got {:?}",
                other
            ))),
        }
    }

    /// Apply a single closure to a value.
    fn apply_closure(&mut self, closure: Value, arg: Value) -> Result<Value, EffectError> {
        match closure {
            Value::Closure(env, binder, body) => {
                let new_env = env.update(binder, arg);
                Ok(core_eval::eval::eval(&body, &new_env, self.heap)?)
            }
            Value::ConFun(tag, arity, mut args) => {
                args.push(arg);
                if args.len() == arity {
                    Ok(Value::Con(tag, args))
                } else {
                    Ok(Value::ConFun(tag, arity, args))
                }
            }
            other => Err(EffectError::BadContinuation(format!(
                "Expected closure, got {:?}",
                other
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_eval::heap::VecHeap;
    use core_repr::datacon::DataCon;
    use core_repr::datacon_table::DataConTable;
    use core_repr::types::{DataConId, Literal, VarId};
    use core_repr::{CoreExpr, CoreFrame, RecursiveTree};

    fn make_test_table() -> DataConTable {
        let mut table = DataConTable::new();
        table.insert(DataCon {
            id: DataConId(1),
            name: "Val".to_string(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![],
        });
        table.insert(DataCon {
            id: DataConId(2),
            name: "E".to_string(),
            tag: 2,
            rep_arity: 2,
            field_bangs: vec![],
        });
        table.insert(DataCon {
            id: DataConId(3),
            name: "Leaf".to_string(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![],
        });
        table.insert(DataCon {
            id: DataConId(4),
            name: "Node".to_string(),
            tag: 2,
            rep_arity: 2,
            field_bangs: vec![],
        });
        table.insert(DataCon {
            id: DataConId(5),
            name: "Union".to_string(),
            tag: 1,
            rep_arity: 2,
            field_bangs: vec![],
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

        // Build the continuation: Leaf(\x -> Val(x))
        // \x -> Val(x) is: Lam(x, Con(Val, [Var(x)]))
        // Leaf(f) is: Con(Leaf, [f])
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
        use crate::dispatch::{DispatchEffect, EffectHandler};
        use core_bridge::FromCore;

        struct TestReq(i64);
        impl FromCore for TestReq {
            fn from_value(
                value: &Value,
                _table: &DataConTable,
            ) -> Result<Self, core_bridge::BridgeError> {
                match value {
                    Value::Lit(Literal::LitInt(n)) => Ok(TestReq(*n)),
                    _ => Err(core_bridge::BridgeError::TypeMismatch {
                        expected: "LitInt".into(),
                        got: format!("{:?}", value),
                    }),
                }
            }
        }

        struct TestHandler;
        impl EffectHandler for TestHandler {
            type Request = TestReq;
            fn handle(
                &mut self,
                req: TestReq,
                _table: &DataConTable,
            ) -> Result<Value, EffectError> {
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
}
