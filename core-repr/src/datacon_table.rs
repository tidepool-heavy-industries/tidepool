use std::collections::HashMap;
use crate::types::DataConId;
use crate::datacon::DataCon;

/// Lookup table for data constructor metadata.
/// Populated during deserialization from the CBOR metadata section.
#[derive(Debug, Clone, Default)]
pub struct DataConTable {
    by_id: HashMap<DataConId, DataCon>,
    by_name: HashMap<String, DataConId>,
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

        // If we're overwriting an existing entry for this id, remove its old name mapping.
        if let Some(old_dc) = self.by_id.insert(id, dc) {
            if let Some(mapped_id) = self.by_name.get(&old_dc.name).copied() {
                if mapped_id == id {
                    self.by_name.remove(&old_dc.name);
                }
            }
        }

        // Now insert/overwrite the mapping for the new name.
        self.by_name.insert(name, id);
    }

    /// Look up by DataConId.
    pub fn get(&self, id: DataConId) -> Option<&DataCon> {
        self.by_id.get(&id)
    }

    /// Look up by name, returning the DataConId.
    pub fn get_by_name(&self, name: &str) -> Option<DataConId> {
        self.by_name.get(name).copied()
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
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
        let ids: Vec<u64> = table.iter().map(|dc| match dc.id { DataConId(id) => id }).collect();
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

        // Overwrite existing id with a different name; ensure by_name is updated and old name is removed.
        let dc1 = make_datacon(1, "Just", 1, 1);
        let dc2 = make_datacon(1, "Other", 1, 1);

        table.insert(dc1);
        table.insert(dc2.clone());

        // Only one entry for id 1 and it should be dc2.
        assert_eq!(table.len(), 1);
        assert_eq!(table.get(DataConId(1)), Some(&dc2));

        // by_name should now point "Other" to id 1 and no longer have "Just".
        assert_eq!(table.get_by_name("Other"), Some(DataConId(1)));
        assert_eq!(table.get_by_name("Just"), None);

        // Now insert two different ids with the same name and ensure by_name tracks the latest.
        let dc3 = make_datacon(2, "Same", 2, 0);
        let dc4 = make_datacon(3, "Same", 3, 0);

        table.insert(dc3.clone());
        assert_eq!(table.get_by_name("Same"), Some(DataConId(2)));

        table.insert(dc4.clone());
        assert_eq!(table.get_by_name("Same"), Some(DataConId(3)));
    }

    #[test]
    fn test_get_by_name_missing() {
        let table = DataConTable::new();
        assert_eq!(table.get_by_name("Missing"), None);
    }
}
