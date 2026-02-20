use crate::value::Value;
use tidepool_repr::VarId;
use im::HashMap;

/// Evaluation environment: variable bindings.
/// Uses im::HashMap for O(1) clone (structural sharing).
pub type Env = HashMap<VarId, Value>;

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
}
