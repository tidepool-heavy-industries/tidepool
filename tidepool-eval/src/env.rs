//! Evaluation environment and variable bindings.

use crate::json::JsonConIds;
use crate::time::TimeConIds;
use crate::value::Value;
use im::HashMap as ImHashMap;
use std::sync::Arc;
use tidepool_repr::{JoinId, VarId};

/// A key into the [`Env`] binding map: either an ordinary Core variable or a
/// join-point label.
///
/// Join continuations and ordinary variables live in the SAME map (a join
/// must be visible to the same lexical lookups as a variable — see
/// [`Env::update_join`]), but `VarId` and `JoinId` are unrelated Core
/// identifier families (real `VarId`s already use high-byte tags, e.g.
/// `0xFE` for externals). Wrapping them in a sum type makes it a compile
/// error to alias one namespace into the other, so a join label can never
/// collide with a real variable id and silently resolve the wrong binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum EnvKey {
    Var(VarId),
    Join(JoinId),
}

/// The constructor ids `JsonDecode`/`ParseISO8601` need to build ADT values
/// from Rust, resolved once from the `DataConTable` that produced an [`Env`].
///
/// Carried directly on the `Env` (see [`Env::ids`]) rather than in a
/// thread-local cache keyed by nothing: a thread-local is set by whichever
/// `Env` was built LAST, so evaluating an older `Env` after a newer one was
/// built on the same thread would silently resolve the NEWER table's ids.
/// Baking the ids into the `Env` value itself makes every evaluation see the
/// ids for the table that actually produced the environment it is running
/// under, regardless of what else has been built on the thread since.
///
/// Each id set is `Arc`-wrapped rather than inlined by value: `JsonConIds`
/// alone is ~20 `DataConId` fields, and `Env` is cloned on every scope entry
/// (closures capture it, `update`/`update_join` return a fresh copy) — an
/// `Option<Arc<_>>` keeps that clone a pointer bump (and the common
/// `Env::new()` case, with no table resolved, allocation-free) instead of
/// copying the whole struct every time.
#[derive(Debug, Clone, Default)]
pub struct EvalIds {
    /// The aeson-`Value`/`Either`/`Data.Map` ids for `JsonDecode`. `None`
    /// when `Either` (or another required constructor) is not in scope.
    pub json: Option<Arc<JsonConIds>>,
    /// The `Either`/`I#`/`Text` ids for `ParseISO8601`. `None` when a
    /// required constructor is not in scope.
    pub time: Option<Arc<TimeConIds>>,
}

/// Evaluation environment: variable/join bindings plus the [`EvalIds`]
/// resolved from the `DataConTable` that built it.
///
/// The binding map uses an `im::HashMap` for efficient structural sharing,
/// allowing closures to capture their environment with minimal overhead.
/// `Env` is embedded BY VALUE in `Value::Closure`/`Value::JoinCont`, so its
/// own size feeds directly into `Value`'s size; `ids` is one more
/// `Option<Arc<_>>` behind that (rather than `EvalIds`'s two fields inlined)
/// to keep that footprint a single pointer.
#[derive(Debug, Clone, Default)]
pub struct Env {
    bindings: ImHashMap<EnvKey, Value>,
    ids: Option<Arc<EvalIds>>,
}

impl Env {
    /// An empty environment (no bindings, no ids).
    pub fn new() -> Self {
        Self::default()
    }

    /// The [`EvalIds`] resolved for this environment (empty/default if none
    /// were ever set — e.g. an `Env::new()` built without a `DataConTable`).
    pub fn ids(&self) -> EvalIds {
        self.ids.as_deref().cloned().unwrap_or_default()
    }

    /// Set the [`EvalIds`] for this environment. The one caller is
    /// `env_from_datacon_table`.
    pub fn set_ids(&mut self, ids: EvalIds) {
        self.ids = Some(Arc::new(ids));
    }

    /// Whether the binding map is empty. Ids never carry a `Value`, so they
    /// have no bearing on this — it exists for the iterative `Value`
    /// destructor to skip environments with nothing to drop.
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Number of bindings (vars + joins).
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    /// All bound values, var and join alike — used by the heap's
    /// reachability walk (GC-ish thunk-ref collection), which does not care
    /// which namespace a binding came from.
    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.bindings.values()
    }

    /// Look up an ordinary variable.
    pub fn get(&self, var: &VarId) -> Option<&Value> {
        self.bindings.get(&EnvKey::Var(*var))
    }

    /// Bind an ordinary variable in place.
    pub fn insert(&mut self, var: VarId, val: Value) -> Option<Value> {
        self.bindings.insert(EnvKey::Var(var), val)
    }

    /// Bind an ordinary variable, returning the updated environment
    /// (persistent update via structural sharing; `self` is unaffected).
    pub fn update(&self, var: VarId, val: Value) -> Self {
        Self {
            bindings: self.bindings.update(EnvKey::Var(var), val),
            ids: self.ids.clone(),
        }
    }

    /// Look up a join-point continuation.
    pub fn get_join(&self, join: &JoinId) -> Option<&Value> {
        self.bindings.get(&EnvKey::Join(*join))
    }

    /// Bind a join-point continuation, returning the updated environment
    /// (persistent update; `self` is unaffected).
    pub fn update_join(&self, join: JoinId, val: Value) -> Self {
        Self {
            bindings: self.bindings.update(EnvKey::Join(join), val),
            ids: self.ids.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::Literal;

    #[test]
    fn test_env_sharing() {
        let env1 = Env::new();
        let var1 = VarId(1);
        let val1 = Value::Lit(Literal::LitInt(10));

        let mut env2 = env1.clone();
        env2.insert(var1, val1.clone());

        assert!(env1.get(&var1).is_none());
        assert_eq!(
            match env2.get(&var1) {
                Some(Value::Lit(Literal::LitInt(n))) => *n,
                _ => 0,
            },
            10
        );

        let mut env3 = env2.clone();
        let var2 = VarId(2);
        let val2 = Value::Lit(Literal::LitInt(20));
        env3.insert(var2, val2);

        assert_eq!(env2.len(), 1);
        assert_eq!(env3.len(), 2);
    }

    /// F1: a join label and a variable with the SAME numeric id must resolve
    /// independently — the whole point of splitting the env key into
    /// `EnvKey::Var`/`EnvKey::Join` instead of manufacturing a tagged `VarId`
    /// for join labels.
    #[test]
    fn var_and_join_with_same_numeric_id_do_not_alias() {
        let var = VarId(42);
        let join = JoinId(42);
        let env = Env::new()
            .update(var, Value::Lit(Literal::LitInt(1)))
            .update_join(join, Value::Lit(Literal::LitInt(2)));

        assert!(matches!(
            env.get(&var),
            Some(Value::Lit(Literal::LitInt(1)))
        ));
        assert!(matches!(
            env.get_join(&join),
            Some(Value::Lit(Literal::LitInt(2)))
        ));
    }
}
