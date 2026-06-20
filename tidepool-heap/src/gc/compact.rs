use crate::gc::trace::ForwardingTable;
use tidepool_eval::env::Env;
use tidepool_eval::heap::{Heap, ThunkState, VecHeap};
use tidepool_eval::value::{ThunkId, Value};
use tidepool_repr::{CoreExpr, CoreFrame, DataConId, RecursiveTree, VarId};

/// Compact the heap by moving reachable thunks to a new VecHeap.
pub fn compact(table: &ForwardingTable, old_heap: &dyn Heap) -> VecHeap {
    let mut inverse = vec![ThunkId(0); table.reachable_count()];

    for (old_idx, maybe_new_id) in table.mapping.iter().enumerate() {
        if let Some(new_id) = maybe_new_id {
            inverse[new_id.0 as usize] = ThunkId(old_idx as u32);
        }
    }

    let mut new_heap = VecHeap::new();
    let dummy_expr = RecursiveTree {
        nodes: vec![CoreFrame::Var(VarId(0))],
    };

    for &old_id in &inverse {
        let old_state = old_heap.read(old_id);

        // We can't easily construct a ThunkState::Unevaluated if it's already something else
        // via alloc, so we always alloc and then potentially overwrite if needed,
        // but we can optimize if it's already Unevaluated.
        match old_state {
            ThunkState::Unevaluated(env, expr) => {
                new_heap.alloc(rewrite_env(env, table), expr.clone());
            }
            _ => {
                let id = new_heap.alloc(Env::new(), dummy_expr.clone());
                new_heap.write(id, rewrite_state(old_state, table));
            }
        }
    }

    new_heap
}

fn rewrite_state(state: &ThunkState, table: &ForwardingTable) -> ThunkState {
    match state {
        ThunkState::Unevaluated(env, expr) => {
            ThunkState::Unevaluated(rewrite_env(env, table), expr.clone())
        }
        ThunkState::BlackHole => ThunkState::BlackHole,
        ThunkState::Evaluated(val) => ThunkState::Evaluated(rewrite_value(val, table)),
    }
}

fn rewrite_env(env: &Env, table: &ForwardingTable) -> Env {
    env.iter()
        .map(|(k, v)| (*k, rewrite_value(v, table)))
        .collect()
}

/// Post-order work item for the iterative [`rewrite_value`] fold. A `Rewrite`
/// projects one node's children onto the stack; each `Build*` pops its already-
/// rewritten children off the results stack and reassembles the node.
///
/// `Closure`/`JoinCont` carry their environment's keys so the rewritten env
/// values (pushed as `Rewrite`s, popped here) can be zipped back into a fresh
/// `Env` — turning a nested-closure chain (the spine that overflowed
/// `Value::drop`) into queue length rather than call depth.
enum RewriteWork<'a> {
    Rewrite(&'a Value),
    BuildCon(DataConId, usize),
    BuildConFun(DataConId, usize, usize),
    BuildClosure(Vec<VarId>, VarId, &'a CoreExpr),
    BuildJoinCont(Vec<VarId>, Vec<VarId>, &'a CoreExpr),
}

// The per-item cost of the explicit work list is a visible, testable property
// — the point of replacing the compiler-hidden recursive fold over Value spines.
const _: () = assert!(std::mem::size_of::<RewriteWork<'static>>() <= 64);

/// Split an env into parallel `(keys, values)` vectors in iteration order. The
/// matching `Build*` holds `keys`; the caller pushes `values` in REVERSE after
/// the `Build*` so they pop (and land in `results`) in forward key order.
fn env_keys_vals(env: &Env) -> (Vec<VarId>, Vec<&Value>) {
    let mut keys = Vec::with_capacity(env.len());
    let mut vals = Vec::with_capacity(env.len());
    for (k, v) in env.iter() {
        keys.push(*k);
        vals.push(v);
    }
    (keys, vals)
}

/// Rewrite every `ThunkRef` in `val` through the forwarding `table`, returning a
/// fresh `Value`.
///
/// Iterative (explicit post-order work stack, mirroring `deep_force`): a GC
/// rewrite must not recurse the host stack over `Con`/`ConFun` field spines or
/// `Closure`/`JoinCont` environment chains — a deep value would overflow the
/// host thread (the "host stack-overflow" class: a silent thread death outside
/// the JIT signal handler). Sibling rewrite ORDER is unobservable.
fn rewrite_value(val: &Value, table: &ForwardingTable) -> Value {
    let mut stack: Vec<RewriteWork> = vec![RewriteWork::Rewrite(val)];
    let mut results: Vec<Value> = Vec::new();

    while let Some(work) = stack.pop() {
        match work {
            RewriteWork::Rewrite(v) => match v {
                Value::Lit(l) => results.push(Value::Lit(l.clone())),
                Value::ByteArray(ba) => results.push(Value::ByteArray(ba.clone())),
                Value::ThunkRef(id) => {
                    results.push(Value::ThunkRef(table.lookup(*id).unwrap_or_else(|_| {
                        panic!(
                            "GC compact: ThunkRef({}) not in forwarding table — GC trace bug",
                            id.0
                        );
                    })));
                }
                Value::Con(id, fields) => {
                    stack.push(RewriteWork::BuildCon(*id, fields.len()));
                    for f in fields.iter().rev() {
                        stack.push(RewriteWork::Rewrite(f));
                    }
                }
                Value::ConFun(id, arity, args) => {
                    stack.push(RewriteWork::BuildConFun(*id, *arity, args.len()));
                    for a in args.iter().rev() {
                        stack.push(RewriteWork::Rewrite(a));
                    }
                }
                Value::Closure(env, binder, expr) => {
                    let (keys, vals) = env_keys_vals(env);
                    // `Build*` first (bottom), env values reversed on top: they
                    // pop in forward key order and the build runs after them.
                    stack.push(RewriteWork::BuildClosure(keys, *binder, expr));
                    for v in vals.into_iter().rev() {
                        stack.push(RewriteWork::Rewrite(v));
                    }
                }
                Value::JoinCont(binders, expr, env) => {
                    let (keys, vals) = env_keys_vals(env);
                    stack.push(RewriteWork::BuildJoinCont(keys, binders.clone(), expr));
                    for v in vals.into_iter().rev() {
                        stack.push(RewriteWork::Rewrite(v));
                    }
                }
            },
            RewriteWork::BuildCon(id, n) => {
                let fields = results.split_off(results.len() - n);
                results.push(Value::Con(id, fields));
            }
            RewriteWork::BuildConFun(id, arity, n) => {
                let args = results.split_off(results.len() - n);
                results.push(Value::ConFun(id, arity, args));
            }
            RewriteWork::BuildClosure(keys, binder, expr) => {
                let vals = results.split_off(results.len() - keys.len());
                let env: Env = keys.into_iter().zip(vals).collect();
                results.push(Value::Closure(env, binder, expr.clone()));
            }
            RewriteWork::BuildJoinCont(keys, binders, expr) => {
                let vals = results.split_off(results.len() - keys.len());
                let env: Env = keys.into_iter().zip(vals).collect();
                results.push(Value::JoinCont(binders, expr.clone(), env));
            }
        }
    }

    results
        .pop()
        .expect("rewrite_value: work stack always leaves exactly one result")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gc::trace::trace;
    use tidepool_eval::heap::VecHeap;
    use tidepool_repr::DataConId;

    fn empty_expr() -> tidepool_repr::CoreExpr {
        RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(0))],
        }
    }

    #[test]
    fn test_compact_basic() {
        let mut heap = VecHeap::new();
        let _id1 = heap.alloc(Env::new(), empty_expr());
        let id2 = heap.alloc(Env::new(), empty_expr());

        // id2 is reachable, id1 is not
        let table = trace(&[id2], &heap);
        let new_heap = compact(&table, &heap);

        // Only one thunk in new heap
        if let ThunkState::BlackHole = new_heap.read(ThunkId(0)) {
            panic!("Expected something other than BlackHole")
        }
    }

    #[test]
    fn test_compact_rewriting() {
        let mut heap = VecHeap::new();
        let id_target = heap.alloc(Env::new(), empty_expr());
        let mut env = Env::new();
        env.insert(VarId(1), Value::ThunkRef(id_target));
        let id_root = heap.alloc(env, empty_expr());

        let table = trace(&[id_root], &heap);
        let new_heap = compact(&table, &heap);

        // id_root should be ThunkId(0), id_target should be ThunkId(1)
        let new_id_root = ThunkId(0);
        let new_id_target = ThunkId(1);

        match new_heap.read(new_id_root) {
            ThunkState::Unevaluated(env, _) => match env.get(&VarId(1)).unwrap() {
                Value::ThunkRef(id) => assert_eq!(*id, new_id_target),
                _ => panic!("Expected ThunkRef"),
            },
            _ => panic!("Expected Unevaluated"),
        }
    }

    #[test]
    fn test_compact_blackhole_preserved() {
        let mut heap = VecHeap::new();
        let id = heap.alloc(Env::new(), empty_expr());
        heap.write(id, ThunkState::BlackHole);

        let table = trace(&[id], &heap);
        let new_heap = compact(&table, &heap);

        match new_heap.read(ThunkId(0)) {
            ThunkState::BlackHole => (),
            _ => panic!("Expected BlackHole"),
        }
    }

    #[test]
    fn test_compact_evaluated_rewriting() {
        let mut heap = VecHeap::new();
        let id2 = heap.alloc(Env::new(), empty_expr());
        let id1 = heap.alloc(Env::new(), empty_expr());
        heap.write(
            id1,
            ThunkState::Evaluated(Value::Con(DataConId(1), vec![Value::ThunkRef(id2)])),
        );

        let table = trace(&[id1], &heap);
        let new_heap = compact(&table, &heap);

        // id1 -> 0, id2 -> 1
        match new_heap.read(ThunkId(0)) {
            ThunkState::Evaluated(Value::Con(_, fields)) => match &fields[0] {
                Value::ThunkRef(id) => assert_eq!(*id, ThunkId(1)),
                _ => panic!("Expected ThunkRef"),
            },
            _ => panic!("Expected Evaluated"),
        }
    }

    /// Equivalence: the iterative `rewrite_value` reproduces the obvious
    /// recursive spec on a small mixed value (Con of ThunkRef + nested Con +
    /// closure capturing a ThunkRef). Guards the post-order reassembly.
    #[test]
    fn rewrite_value_matches_spec_on_mixed_value() {
        use tidepool_repr::Literal;
        // Forwarding table mapping old ThunkId(3) -> new ThunkId(0).
        let mut heap = VecHeap::new();
        let target = heap.alloc(Env::new(), empty_expr()); // ThunkId(0)
        let table = trace(&[target], &heap);

        let mut env = Env::new();
        env.insert(VarId(7), Value::ThunkRef(target));
        env.insert(VarId(8), Value::Lit(Literal::LitInt(9)));
        let val = Value::Con(
            DataConId(1),
            vec![
                Value::ThunkRef(target),
                Value::Con(DataConId(2), vec![Value::Lit(Literal::LitInt(5))]),
                Value::Closure(env, VarId(1), empty_expr()),
            ],
        );

        let got = rewrite_value(&val, &table);
        let Value::Con(id, fields) = &got else {
            panic!("expected Con")
        };
        assert_eq!(*id, DataConId(1));
        assert!(matches!(fields[0], Value::ThunkRef(ThunkId(0))));
        assert!(matches!(&fields[1], Value::Con(DataConId(2), inner)
            if matches!(inner[0], Value::Lit(Literal::LitInt(5)))));
        let Value::Closure(renv, b, _) = &fields[2] else {
            panic!("expected Closure")
        };
        assert_eq!(*b, VarId(1));
        assert!(matches!(
            renv.get(&VarId(7)),
            Some(Value::ThunkRef(ThunkId(0)))
        ));
        assert!(matches!(
            renv.get(&VarId(8)),
            Some(Value::Lit(Literal::LitInt(9)))
        ));
    }

    /// Run `f` on a deliberately small (512 KiB) thread and require it to
    /// finish — a stack overflow inside `f` aborts the process.
    fn on_small_stack<F: FnOnce() + Send + 'static>(f: F) {
        std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(f)
            .expect("spawn small-stack thread")
            .join()
            .expect("GC compact overflowed the host thread stack");
    }

    /// Deep `Con` field spine in an evaluated thunk: `rewrite_value` must fold it
    /// onto its work stack, not the host call stack. A recursive fold SIGSEGVs
    /// here (the "host stack-overflow" class).
    #[test]
    fn compact_deep_con_spine_is_stack_safe() {
        use tidepool_repr::Literal;
        let mut spine = Value::Lit(Literal::LitInt(0));
        for _ in 0..400_000 {
            spine = Value::Con(DataConId(1), vec![spine]);
        }
        let mut heap = VecHeap::new();
        let id = heap.alloc(Env::new(), empty_expr());
        heap.write(id, ThunkState::Evaluated(spine));
        let table = trace(&[id], &heap);
        on_small_stack(move || {
            let _ = compact(&table, &heap);
        });
    }

    /// Nested-closure chain (each closure captures the previous in its env) in
    /// an evaluated thunk — the env-chain spine that overflowed `Value::drop`.
    /// `rewrite_value` must descend into `Closure` envs iteratively.
    #[test]
    fn compact_deep_closure_env_chain_is_stack_safe() {
        use tidepool_repr::Literal;
        let mut v = Value::Lit(Literal::LitInt(0));
        for _ in 0..200_000 {
            let mut env = Env::new();
            env.insert(VarId(0), v);
            v = Value::Closure(env, VarId(1), empty_expr());
        }
        let mut heap = VecHeap::new();
        let id = heap.alloc(Env::new(), empty_expr());
        heap.write(id, ThunkState::Evaluated(v));
        let table = trace(&[id], &heap);
        on_small_stack(move || {
            let _ = compact(&table, &heap);
        });
    }
}
