use crate::datacon::DataCon;
use crate::types::{AltCon, DataConId};
use std::collections::HashMap;

/// Lookup table for data constructor metadata.
/// Populated during deserialization from the CBOR metadata section.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataConTable {
    by_id: HashMap<DataConId, DataCon>,
    by_name: HashMap<String, Vec<DataConId>>,
    by_qualified_name: HashMap<String, DataConId>,
    /// Type-sibling groups: DataConIds that appear together in case branches.
    /// If Bin and Tip appear as alternatives in the same Case, they're siblings.
    siblings: HashMap<DataConId, Vec<DataConId>>,
}

impl DataConTable {
    /// Create an empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a data constructor. Overwrites if id already exists.
    pub fn insert(&mut self, dc: DataCon) {
        let id = dc.id;
        let name = dc.name.clone();
        let qualified_name = dc.qualified_name.clone();

        // If we're overwriting an existing entry for this id, remove its old name mappings.
        if let Some(old_dc) = self.by_id.insert(id, dc) {
            if let Some(vec) = self.by_name.get_mut(&old_dc.name) {
                vec.retain(|&existing| existing != id);
                if vec.is_empty() {
                    self.by_name.remove(&old_dc.name);
                }
            }
            if let Some(ref old_qn) = old_dc.qualified_name {
                self.by_qualified_name.remove(old_qn);
            }
        }

        // Now insert the mappings for the new name.
        self.by_name.entry(name).or_default().push(id);
        if let Some(qn) = qualified_name {
            self.by_qualified_name.insert(qn, id);
        }
    }

    /// Look up by DataConId.
    pub fn get(&self, id: DataConId) -> Option<&DataCon> {
        self.by_id.get(&id)
    }

    /// Look up by module-qualified name (e.g., "Data.Map.Bin"), returning the DataConId.
    pub fn get_by_qualified_name(&self, qname: &str) -> Option<DataConId> {
        self.by_qualified_name.get(qname).copied()
    }

    /// Look up by name, returning the DataConId.
    ///
    /// Returns `None` when multiple constructors share the same unqualified name,
    /// since the result would be ambiguous. Use `get_by_qualified_name`,
    /// `get_by_name_arity`, or `get_companion` instead.
    pub fn get_by_name(&self, name: &str) -> Option<DataConId> {
        match self.by_name.get(name) {
            Some(vec) if vec.len() > 1 => None,
            Some(vec) => vec.last().copied(),
            None => None,
        }
    }

    /// Look up by name AND expected arity, scanning all entries with this name.
    ///
    /// This avoids the ambiguity of `get_by_name` when multiple constructors
    /// share the same unqualified name (e.g. `Array` from aeson vs GHC internals).
    /// Returns the last matching entry (preserving insertion-order preference).
    pub fn get_by_name_arity(&self, name: &str, arity: u32) -> Option<DataConId> {
        self.by_name.get(name).and_then(|vec| {
            vec.iter()
                .rev()
                .find(|&&id| {
                    self.by_id
                        .get(&id)
                        .map_or(false, |dc| dc.rep_arity == arity)
                })
                .copied()
        })
    }

    /// Return all DataConIds sharing a given name (in insertion order).
    pub fn get_all_by_name(&self, name: &str) -> &[DataConId] {
        self.by_name.get(name).map_or(&[], |v| v.as_slice())
    }

    /// Find a constructor by name+arity that is a type-sibling of `known_id`.
    ///
    /// Uses sibling groups populated by `populate_siblings_from_expr` — if two
    /// DataCons appear as alternatives in the same case expression, they're from
    /// the same type. Falls back to scanning all entries if no sibling info exists.
    pub fn get_companion(
        &self,
        known_id: DataConId,
        name: &str,
        arity: u32,
    ) -> Option<DataConId> {
        // First try sibling groups (reliable, derived from case branches)
        if let Some(sibs) = self.siblings.get(&known_id) {
            for &sib_id in sibs {
                if let Some(dc) = self.by_id.get(&sib_id) {
                    if dc.name == name && dc.rep_arity == arity {
                        return Some(sib_id);
                    }
                }
            }
        }
        // Fallback: just use get_by_name_arity
        self.get_by_name_arity(name, arity)
    }

    /// Populate sibling groups by scanning case branches in an expression tree.
    ///
    /// DataCons that appear as alternatives in the same Case expression are from
    /// the same algebraic type. This information is used by `get_companion` to
    /// disambiguate constructors that share unqualified names (e.g., Bin/Tip from
    /// Data.Map vs Data.Set).
    pub fn populate_siblings_from_expr(&mut self, expr: &crate::CoreExpr) {
        use crate::frame::CoreFrame;

        for node in &expr.nodes {
            if let CoreFrame::Case { alts, .. } = node {
                let data_con_ids: Vec<DataConId> = alts
                    .iter()
                    .filter_map(|alt| {
                        if let AltCon::DataAlt(id) = alt.con {
                            Some(id)
                        } else {
                            None
                        }
                    })
                    .collect();

                if data_con_ids.len() >= 2 {
                    for &id in &data_con_ids {
                        let sibs = self.siblings.entry(id).or_default();
                        for &other in &data_con_ids {
                            if other != id && !sibs.contains(&other) {
                                sibs.push(other);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Look up constructor name by DataConId.
    pub fn name_of(&self, id: DataConId) -> Option<&str> {
        self.by_id.get(&id).map(|dc| dc.name.as_str())
    }

    /// Iterate over all data constructors.
    pub fn iter(&self) -> impl Iterator<Item = &DataCon> {
        self.by_id.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datacon::SrcBang;

    fn make_datacon(id: u64, name: &str, tag: u32, rep_arity: u32) -> DataCon {
        DataCon {
            id: DataConId(id),
            name: name.to_string(),
            tag,
            rep_arity,
            field_bangs: vec![],
            qualified_name: None,
        }
    }

    fn make_datacon_qualified(
        id: u64,
        name: &str,
        tag: u32,
        rep_arity: u32,
        qname: &str,
    ) -> DataCon {
        DataCon {
            id: DataConId(id),
            name: name.to_string(),
            tag,
            rep_arity,
            field_bangs: vec![],
            qualified_name: Some(qname.to_string()),
        }
    }

    #[test]
    fn test_insert_and_get_by_id() {
        let mut table = DataConTable::new();
        let dc = make_datacon(1, "Just", 1, 1);
        table.insert(dc.clone());
        assert_eq!(table.get(DataConId(1)), Some(&dc));
    }

    #[test]
    fn test_insert_and_get_by_name() {
        let mut table = DataConTable::new();
        let dc = make_datacon(1, "Just", 1, 1);
        table.insert(dc);
        assert_eq!(table.get_by_name("Just"), Some(DataConId(1)));
    }

    #[test]
    fn test_maybe_rep_arity() {
        let mut table = DataConTable::new();
        let nothing = make_datacon(1, "Nothing", 1, 0);
        let just = make_datacon(2, "Just", 2, 1);
        table.insert(nothing.clone());
        table.insert(just.clone());

        assert_eq!(table.get(DataConId(1)).unwrap().rep_arity, 0);
        assert_eq!(table.get(DataConId(2)).unwrap().rep_arity, 1);
    }

    #[test]
    fn test_multiple_datacons() {
        let mut table = DataConTable::new();
        table.insert(make_datacon(1, "A", 1, 0));
        table.insert(make_datacon(2, "B", 2, 0));
        table.insert(make_datacon(3, "C", 3, 0));

        assert_eq!(table.len(), 3);
        let ids: Vec<u64> = table
            .iter()
            .map(|dc| match dc.id {
                DataConId(id) => id,
            })
            .collect();
        assert!(ids.contains(&1));
        assert!(ids.contains(&2));
        assert!(ids.contains(&3));
    }

    #[test]
    fn test_overwrite() {
        let mut table = DataConTable::new();
        let dc1 = make_datacon(1, "Just", 1, 1);
        let mut dc2 = make_datacon(1, "Just", 1, 1);
        dc2.field_bangs = vec![SrcBang::SrcBang];

        table.insert(dc1);
        table.insert(dc2.clone());

        assert_eq!(table.len(), 1);
        assert_eq!(table.get(DataConId(1)), Some(&dc2));
    }

    #[test]
    fn test_overwrite_name_and_by_name_consistency() {
        let mut table = DataConTable::new();

        let dc1 = make_datacon(1, "Just", 1, 1);
        let dc2 = make_datacon(1, "Other", 1, 1);

        table.insert(dc1);
        table.insert(dc2.clone());

        assert_eq!(table.len(), 1);
        assert_eq!(table.get(DataConId(1)), Some(&dc2));

        assert_eq!(table.get_by_name("Other"), Some(DataConId(1)));
        assert_eq!(table.get_by_name("Just"), None);

        let dc3 = make_datacon(2, "Same", 2, 0);
        let dc4 = make_datacon(3, "Same", 3, 0);

        table.insert(dc3.clone());
        // Only one "Same" — not ambiguous yet
        assert_eq!(table.get_by_name("Same"), Some(DataConId(2)));

        table.insert(dc4.clone());
        // Two "Same" entries — get_by_name would panic, use get_all_by_name instead
        let all = table.get_all_by_name("Same");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0], DataConId(2));
        assert_eq!(all[1], DataConId(3));
    }

    #[test]
    fn test_get_by_name_missing() {
        let table = DataConTable::new();
        assert_eq!(table.get_by_name("Missing"), None);
    }

    #[test]
    fn test_get_all_by_name() {
        let mut table = DataConTable::new();
        table.insert(make_datacon(100, "Tip", 1, 0));
        table.insert(make_datacon(200, "Tip", 1, 0));

        let all = table.get_all_by_name("Tip");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0], DataConId(100));
        assert_eq!(all[1], DataConId(200));
    }

    #[test]
    fn test_get_by_name_arity_disambiguates() {
        let mut table = DataConTable::new();
        table.insert(make_datacon(100, "Bin", 1, 5));
        table.insert(make_datacon(200, "Bin", 1, 3));

        assert_eq!(
            table.get_by_name_arity("Bin", 5),
            Some(DataConId(100))
        );
        assert_eq!(
            table.get_by_name_arity("Bin", 3),
            Some(DataConId(200))
        );
    }

    #[test]
    fn test_get_companion_with_siblings() {
        let mut table = DataConTable::new();
        // Data.Map constructors
        table.insert(make_datacon(100, "Bin", 1, 5));
        table.insert(make_datacon(101, "Tip", 2, 0));
        // Data.Set constructors (different IDs, same names)
        table.insert(make_datacon(200, "Bin", 1, 3));
        table.insert(make_datacon(201, "Tip", 2, 0));

        // Simulate case branches: Bin(100) and Tip(101) appear together
        table.siblings.insert(DataConId(100), vec![DataConId(101)]);
        table.siblings.insert(DataConId(101), vec![DataConId(100)]);
        // Bin(200) and Tip(201) appear together
        table.siblings.insert(DataConId(200), vec![DataConId(201)]);
        table.siblings.insert(DataConId(201), vec![DataConId(200)]);

        // Given Map's Bin (100), find companion Tip → should be 101
        assert_eq!(
            table.get_companion(DataConId(100), "Tip", 0),
            Some(DataConId(101))
        );

        // Given Set's Bin (200), find companion Tip → should be 201
        assert_eq!(
            table.get_companion(DataConId(200), "Tip", 0),
            Some(DataConId(201))
        );
    }

    #[test]
    fn test_get_by_qualified_name() {
        let mut table = DataConTable::new();
        table.insert(make_datacon_qualified(100, "Bin", 1, 5, "Data.Map.Bin"));
        table.insert(make_datacon_qualified(200, "Bin", 1, 3, "Data.Set.Bin"));

        assert_eq!(
            table.get_by_qualified_name("Data.Map.Bin"),
            Some(DataConId(100))
        );
        assert_eq!(
            table.get_by_qualified_name("Data.Set.Bin"),
            Some(DataConId(200))
        );
        assert_eq!(table.get_by_qualified_name("Data.Map.Tip"), None);
    }

    #[test]
    fn test_get_by_name_returns_none_on_ambiguity() {
        let mut table = DataConTable::new();
        table.insert(make_datacon_qualified(100, "Tip", 1, 0, "Data.Map.Tip"));
        table.insert(make_datacon_qualified(200, "Tip", 1, 0, "Data.Set.Tip"));

        // Ambiguous name returns None — use get_by_qualified_name instead
        assert_eq!(table.get_by_name("Tip"), None);
    }

    #[test]
    fn test_get_by_name_unique_still_works() {
        let mut table = DataConTable::new();
        table.insert(make_datacon_qualified(100, "Just", 2, 1, "Data.Maybe.Just"));
        // Only one "Just" — no ambiguity
        assert_eq!(table.get_by_name("Just"), Some(DataConId(100)));
    }

    #[test]
    fn test_overwrite_cleans_old_qualified_name() {
        let mut table = DataConTable::new();
        table.insert(make_datacon_qualified(1, "Foo", 1, 0, "Mod.A.Foo"));
        assert_eq!(
            table.get_by_qualified_name("Mod.A.Foo"),
            Some(DataConId(1))
        );

        // Overwrite same id with different qualified name
        table.insert(make_datacon_qualified(1, "Foo", 1, 0, "Mod.B.Foo"));
        assert_eq!(table.get_by_qualified_name("Mod.A.Foo"), None);
        assert_eq!(
            table.get_by_qualified_name("Mod.B.Foo"),
            Some(DataConId(1))
        );
    }

    #[test]
    fn test_overwrite_qualified_to_none() {
        let mut table = DataConTable::new();
        table.insert(make_datacon_qualified(1, "Foo", 1, 0, "Mod.Foo"));
        assert_eq!(
            table.get_by_qualified_name("Mod.Foo"),
            Some(DataConId(1))
        );

        // Overwrite with None qualified name — old mapping should be removed
        table.insert(make_datacon(1, "Foo", 1, 0));
        assert_eq!(table.get_by_qualified_name("Mod.Foo"), None);
    }

    #[test]
    fn test_get_by_name_ambiguous_without_qualified_names() {
        let mut table = DataConTable::new();
        // Two constructors with None qualified_name
        table.insert(make_datacon(100, "Dup", 1, 0));
        table.insert(make_datacon(200, "Dup", 1, 0));
        // Ambiguous name returns None
        assert_eq!(table.get_by_name("Dup"), None);
    }

    #[test]
    fn test_get_by_qualified_name_missing() {
        let table = DataConTable::new();
        assert_eq!(table.get_by_qualified_name("No.Such.Thing"), None);
    }

    #[test]
    fn test_qualified_name_does_not_affect_by_name() {
        let mut table = DataConTable::new();
        table.insert(make_datacon_qualified(1, "Bin", 1, 5, "Data.Map.Bin"));
        // get_by_name still works via unqualified name
        assert_eq!(table.get_by_name("Bin"), Some(DataConId(1)));
        // get_by_qualified_name also works
        assert_eq!(
            table.get_by_qualified_name("Data.Map.Bin"),
            Some(DataConId(1))
        );
    }

    #[test]
    fn test_qualified_name_disambiguates_same_name_same_arity() {
        let mut table = DataConTable::new();
        // Both "Tip" with arity 0 — get_by_name_arity can't disambiguate
        table.insert(make_datacon_qualified(100, "Tip", 1, 0, "Data.Map.Tip"));
        table.insert(make_datacon_qualified(200, "Tip", 1, 0, "Data.Set.Tip"));

        assert_eq!(
            table.get_by_qualified_name("Data.Map.Tip"),
            Some(DataConId(100))
        );
        assert_eq!(
            table.get_by_qualified_name("Data.Set.Tip"),
            Some(DataConId(200))
        );
        // get_by_name_arity returns one of them (last inserted)
        assert_eq!(
            table.get_by_name_arity("Tip", 0),
            Some(DataConId(200))
        );
    }
}
