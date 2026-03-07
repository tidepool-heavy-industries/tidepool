use crate::gc::trace::ForwardingTable;
use tidepool_eval::env::Env;
use tidepool_eval::heap::{Heap, ThunkState, VecHeap};
use tidepool_eval::value::{ThunkId, Value};
use tidepool_repr::{CoreFrame, RecursiveTree, VarId};

/// Compact the heap by moving reachable thunks to a new VecHeap.
pub fn compact(table: &ForwardingTable, old_heap: &dyn Heap) -> VecHeap {
    let reachable_count = table.mapping.iter().flatten().count();
    let mut inverse = vec![ThunkId(0); reachable_count];

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

fn rewrite_value(val: &Value, table: &ForwardingTable) -> Value {
    match val {
        Value::Lit(l) => Value::Lit(l.clone()),
        Value::Con(id, fields) => Value::Con(
            *id,
            fields.iter().map(|f| rewrite_value(f, table)).collect(),
        ),
        Value::ConFun(id, arity, args) => Value::ConFun(
            *id,
            *arity,
            args.iter().map(|a| rewrite_value(a, table)).collect(),
        ),
        Value::Closure(env, binder, expr) => {
            Value::Closure(rewrite_env(env, table), *binder, expr.clone())
        }
        Value::ThunkRef(id) => Value::ThunkRef(table.lookup(*id).unwrap_or_else(|_| {
            panic!(
                "GC compact: ThunkRef({}) not in forwarding table — GC trace bug",
                id.0
            );
        })),
        Value::JoinCont(binders, expr, env) => {
            Value::JoinCont(binders.clone(), expr.clone(), rewrite_env(env, table))
        }
        Value::ByteArray(ba) => Value::ByteArray(ba.clone()),
    }
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
        match new_heap.read(ThunkId(0)) {
            ThunkState::BlackHole => panic!("Expected something other than BlackHole"),
            _ => (),
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
}
