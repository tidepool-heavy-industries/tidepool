//! Evaluation environment and variable bindings.

use crate::value::Value;
use im::HashMap as ImHashMap;
use tidepool_repr::{JoinId, VarId};

/// A key into the [`Env`] binding map: either an ordinary Core variable or a
/// join-point label.
///
/// Join continuations and ordinary variables live in the SAME map (a join
/// must be visible to the same lexical lookups as a variable — see
/// [`Env::update_join`]), but `VarId` and `JoinId` are unrelated Core
/// identifier families (real `VarId`s already use high-byte tags, e.g.
/// `0xFE` for externals). Wrapping them in a sum type — rather than
/// manufacturing a `VarId` with a high bit set for join labels, as this
/// crate used to — makes it a compile error to alias one namespace into the
/// other, so a join label can never collide with a real variable id and
/// silently resolve the wrong binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum EnvKey {
    Var(VarId),
    Join(JoinId),
}

/// Evaluation environment: mapping from [`EnvKey`] to [`Value`].
///
/// Uses an `im::HashMap` for efficient structural sharing, allowing
/// closures to capture their environment with minimal overhead.
#[derive(Debug, Clone, Default)]
pub struct Env {
    bindings: ImHashMap<EnvKey, Value>,
}

impl Env {
    /// An empty environment.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the binding map is empty.
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
