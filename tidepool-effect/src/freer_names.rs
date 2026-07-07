//! The five freer-simple / open-union / FTCQueue constructor names the effect
//! machine must resolve — `Val`, `E`, `Union`, `Leaf`, `Node` — plus their
//! module-qualified spellings.
//!
//! These are toolchain-pinned: the freer-simple/open-union/FTCQueue packages
//! are locked versions, so the defining modules never move. The qualified
//! spelling is the reliable discriminator when a user import collides on the
//! unqualified name — e.g. `Data.Tree.Node` shadows the FTCQueue continuation
//! `Node`, both at arity 2, so unqualified-name (and arity) lookup are both
//! ambiguous; `DataConTable::get_by_name` returns `None` for such a collision.
//!
//! Shared by the oracle (`crate::machine::EffectMachine::new`) and the JIT
//! (`tidepool-codegen`'s `effect_machine::ConTags::try_from`) so both resolve
//! via the SAME qualified-first scheme instead of two independently
//! maintained copies drifting apart (#F5). A later wave (plan 05 F2) reuses
//! these consts in `tidepool-repr`'s `normalize.rs` for the production-path
//! variant of the same collision.

/// Unqualified constructor name for `Val` (pure result).
pub const VAL: &str = "Val";
/// Unqualified constructor name for `E` (effect request).
pub const E: &str = "E";
/// Unqualified constructor name for `Union` (effect-type wrapper).
pub const UNION: &str = "Union";
/// Unqualified constructor name for `Leaf` (leaf continuation).
pub const LEAF: &str = "Leaf";
/// Unqualified constructor name for `Node` (composed continuation).
pub const NODE: &str = "Node";

/// Module-qualified spelling of `Val`, as recorded in the `DataConTable`
/// (`Module.Ctor`, via `Tidepool.Translate.qualifiedName`).
pub const VAL_QUALIFIED: &str = "Control.Monad.Freer.Val";
/// Module-qualified spelling of `E`.
pub const E_QUALIFIED: &str = "Control.Monad.Freer.E";
/// Module-qualified spelling of `Union`.
pub const UNION_QUALIFIED: &str = "Data.OpenUnion.Union";
/// Module-qualified spelling of `Leaf`.
pub const LEAF_QUALIFIED: &str = "Data.FTCQueue.Leaf";
/// Module-qualified spelling of `Node`.
pub const NODE_QUALIFIED: &str = "Data.FTCQueue.Node";

/// Resolve one of the five constructors by qualified name first, falling back
/// to the unqualified name only when the qualified spelling is absent
/// (preserves no-collision behavior for a producer that omits qualified
/// names). Returns `None` — never guesses — on ambiguity.
pub fn resolve(
    table: &tidepool_repr::DataConTable,
    qualified: &str,
    bare: &str,
) -> Option<tidepool_repr::DataConId> {
    table
        .get_by_qualified_name(qualified)
        .or_else(|| table.get_by_name(bare))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::datacon::DataCon;
    use tidepool_repr::types::DataConId;
    use tidepool_repr::DataConTable;

    #[test]
    fn resolve_prefers_qualified_over_bare_on_collision() {
        let mut table = DataConTable::new();
        // A user `Data.Tree.Node` collides on the bare name with the freer
        // continuation `Node` — `get_by_name` returns `None` for the
        // collision, so only the qualified lookup can disambiguate.
        table.insert(DataCon {
            id: DataConId(1),
            name: "Node".to_string(),
            tag: 0,
            rep_arity: 2,
            field_bangs: vec![],
            qualified_name: Some("Data.Tree.Node".to_string()),
        });
        table.insert(DataCon {
            id: DataConId(2),
            name: "Node".to_string(),
            tag: 1,
            rep_arity: 2,
            field_bangs: vec![],
            qualified_name: Some(NODE_QUALIFIED.to_string()),
        });
        assert_eq!(table.get_by_name("Node"), None, "bare name is ambiguous");
        assert_eq!(
            resolve(&table, NODE_QUALIFIED, NODE),
            Some(DataConId(2)),
            "qualified-first resolution must pick the freer continuation, not fail on the collision"
        );
    }

    #[test]
    fn resolve_falls_back_to_bare_when_qualified_absent() {
        let mut table = DataConTable::new();
        table.insert(DataCon {
            id: DataConId(1),
            name: "Leaf".to_string(),
            tag: 0,
            rep_arity: 1,
            field_bangs: vec![],
            qualified_name: None,
        });
        assert_eq!(resolve(&table, LEAF_QUALIFIED, LEAF), Some(DataConId(1)));
    }
}
