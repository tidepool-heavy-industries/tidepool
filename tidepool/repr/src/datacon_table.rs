//! A lookup table for data constructor metadata.

use crate::datacon::DataCon;
use crate::execution_schema::{ConstructorDecl, JsonLayout, PreparedProgram, SymbolIdentity};
use crate::types::DataConId;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// A checked metadata ingestion cannot change a constructor's nominal owner.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DataConCollision {
    #[error("invalid constructor symbol {identity:?}: {detail}")]
    InvalidIdentity {
        identity: SymbolIdentity,
        detail: &'static str,
    },
    #[error("DataConId {id:?} belongs to {first:?}, not {second:?}")]
    Id {
        id: DataConId,
        first: SymbolIdentity,
        second: SymbolIdentity,
    },
    #[error("constructor {identity:?} has conflicting tags or arities: {first_tag}/{first_arity}, {second_tag}/{second_arity}")]
    Shape {
        identity: SymbolIdentity,
        first_tag: u32,
        first_arity: u32,
        second_tag: u32,
        second_arity: u32,
    },
    #[error("constructor {identity:?} claims two DataConIds: {first_id:?}, {second_id:?}")]
    Identity {
        identity: SymbolIdentity,
        first_id: DataConId,
        second_id: DataConId,
    },
}

/// A diagnostic spelling has several possible nominal owners.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("ambiguous constructor spelling {name:?}, arity {arity}: {candidates:?}")]
pub struct AmbiguousDataCon {
    pub name: String,
    pub arity: u32,
    pub candidates: Vec<SymbolIdentity>,
}

/// The complete output table must agree with every emitted constructor.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConstructorMetadataMismatch {
    #[error("constructor {identity:?} has no metadata at {host_id:?}")]
    Missing {
        host_id: DataConId,
        identity: SymbolIdentity,
    },
    #[error("constructor {prepared:?} at {host_id:?} resolves to {metadata:?}")]
    Identity {
        host_id: DataConId,
        prepared: SymbolIdentity,
        metadata: SymbolIdentity,
    },
    #[error("constructor {identity:?} at {host_id:?} has conflicting tag/arity metadata")]
    Shape {
        host_id: DataConId,
        identity: SymbolIdentity,
    },
}

/// Lookup table for data constructor metadata.
/// Populated during deserialization from the CBOR metadata section.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataConTable {
    metadata: Arc<DataConMetadata>,
    /// Per-program JSON runtime IDs, attached only while a typed host value is
    /// streamed through this table. Constructor metadata remains mergeable
    /// without carrying another program's JSON authority forward.
    json_layout: Option<JsonLayout<DataConId>>,
}

/// Constructor metadata is shared by response contexts and copied only on mutation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DataConMetadata {
    /// Mapping from unique DataConId to its metadata.
    by_id: HashMap<DataConId, DataCon>,
    /// Mapping from unqualified name to all DataConIds sharing that name.
    by_name: HashMap<String, Vec<DataConId>>,
    /// All nominal candidates for one diagnostic module-qualified spelling.
    by_qualified_name: HashMap<String, Vec<DataConId>>,
    /// The exact defining symbol, independently of diagnostic aliases.
    by_identity: HashMap<SymbolIdentity, DataConId>,
    /// Mapping from parent-type name (e.g. "Verdict") to all DataConIds of
    /// that type, kept sorted by constructor TAG (declaration order) by
    /// [`DataConTable::sort_type_name_bucket`] — see that function for why insertion
    /// order cannot be trusted to already be in that order.
    by_type_name: HashMap<String, Vec<DataConId>>,
    /// Record field labels per constructor, in field order (from GHC's
    /// `dataConFieldLabels`). Only present for record constructors; positional
    /// constructors have no entry. Kept as a side-table (not on `DataCon`) so it
    /// is pure render metadata and does not affect constructor identity/equality.
    field_labels: HashMap<DataConId, Vec<String>>,
    /// Rendered field types per constructor, in field (declaration) order
    /// (from GHC's `dataConOrigArgTys`, same `ppr` convention as
    /// `DataCon::type_name`). Present whenever the constructor has fields —
    /// including positional constructors, unlike `field_labels`. A side-table
    /// for the same reason as `field_labels`: pure render metadata, no effect
    /// on constructor identity/equality.
    field_types: HashMap<DataConId, Vec<String>>,
}

impl DataConTable {
    /// Create an empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Share constructor metadata with exactly this program's JSON authority.
    /// `None` clears any authority attached to the source context.
    pub fn with_json_layout(&self, layout: Option<JsonLayout<DataConId>>) -> Self {
        Self {
            metadata: Arc::clone(&self.metadata),
            json_layout: layout,
        }
    }

    /// JSON roles attached by the owning prepared program, if any.
    pub fn json_layout(&self) -> Option<&JsonLayout<DataConId>> {
        self.json_layout.as_ref()
    }

    /// Admit a row without replacing another owner or changing its physical shape.
    pub fn insert_checked(&mut self, dc: DataCon) -> Result<(), DataConCollision> {
        self.check_collision(&dc)?;
        let type_name = self.upsert_no_sort(dc);
        self.sort_type_name_bucket(&type_name);
        Ok(())
    }

    fn check_collision(&self, dc: &DataCon) -> Result<(), DataConCollision> {
        let identity = &dc.identity;
        if identity.unit.is_empty()
            || identity.module.is_empty()
            || identity.occurrence.is_empty()
            || identity.namespace != "constructor"
            || identity.occurrence != dc.name
            || identity
                .record_parent
                .as_ref()
                .is_some_and(|parent| parent.is_empty())
        {
            return Err(DataConCollision::InvalidIdentity {
                identity: identity.clone(),
                detail: "expected a complete constructor symbol agreeing with its name",
            });
        }
        if let Some(existing) = self.metadata.by_id.get(&dc.id) {
            if existing.identity != dc.identity {
                return Err(DataConCollision::Id {
                    id: dc.id,
                    first: existing.identity.clone(),
                    second: dc.identity.clone(),
                });
            }
            if existing.tag != dc.tag || existing.rep_arity != dc.rep_arity {
                return Err(DataConCollision::Shape {
                    identity: identity.clone(),
                    first_tag: existing.tag,
                    first_arity: existing.rep_arity,
                    second_tag: dc.tag,
                    second_arity: dc.rep_arity,
                });
            }
        }
        if let Some(&first_id) = self.metadata.by_identity.get(identity) {
            if first_id != dc.id {
                return Err(DataConCollision::Identity {
                    identity: identity.clone(),
                    first_id,
                    second_id: dc.id,
                });
            }
        }
        Ok(())
    }

    /// Joint admission is complete only after its producer has emitted all
    /// declarations and their metadata from the same lowering transaction.
    pub fn validate_program(
        &self,
        program: &PreparedProgram,
    ) -> Result<(), ConstructorMetadataMismatch> {
        self.validate_constructors(program.constructors())
    }

    pub fn validate_constructors(
        &self,
        constructors: &[ConstructorDecl],
    ) -> Result<(), ConstructorMetadataMismatch> {
        for declared in constructors {
            let Some(metadata) = self.get(declared.host_id) else {
                return Err(ConstructorMetadataMismatch::Missing {
                    host_id: declared.host_id,
                    identity: declared.identity.clone(),
                });
            };
            if metadata.identity != declared.identity {
                return Err(ConstructorMetadataMismatch::Identity {
                    host_id: declared.host_id,
                    prepared: declared.identity.clone(),
                    metadata: metadata.identity.clone(),
                });
            }
            if metadata.tag != declared.tag
                || metadata.rep_arity as usize != declared.field_reps.len()
            {
                return Err(ConstructorMetadataMismatch::Shape {
                    host_id: declared.host_id,
                    identity: declared.identity.clone(),
                });
            }
        }
        Ok(())
    }

    /// Insert a data constructor's metadata into every index EXCEPT the final
    /// `by_type_name` bucket sort, returning the type_name whose bucket was
    /// touched (appended to on a new/type-changed entry, or simply left
    /// containing `id` with a possibly-changed tag). Callers are responsible
    /// for sorting that bucket afterward — [`Self::insert_checked`] does so
    /// immediately; [`Self::extend_checked`] batches it across many calls.
    /// Shared by both so there is exactly one implementation of the
    /// retain/re-push bookkeeping.
    fn upsert_no_sort(&mut self, dc: DataCon) -> String {
        let metadata = Arc::make_mut(&mut self.metadata);
        let id = dc.id;
        let type_name = dc.type_name.clone();
        if let Some(old) = metadata.by_id.get(&id) {
            if old.type_name != dc.type_name {
                if let Some(bucket) = metadata.by_type_name.get_mut(&old.type_name) {
                    bucket.retain(|value| *value != id);
                }
                metadata.by_type_name.retain(|_, bucket| !bucket.is_empty());
                metadata
                    .by_type_name
                    .entry(type_name.clone())
                    .or_default()
                    .push(id);
            }
            if old.qualified_name != dc.qualified_name {
                if let Some(alias) = &old.qualified_name {
                    if let Some(bucket) = metadata.by_qualified_name.get_mut(alias) {
                        bucket.retain(|value| *value != id);
                    }
                    metadata
                        .by_qualified_name
                        .retain(|_, bucket| !bucket.is_empty());
                }
                if let Some(alias) = &dc.qualified_name {
                    metadata
                        .by_qualified_name
                        .entry(alias.clone())
                        .or_default()
                        .push(id);
                    metadata
                        .by_qualified_name
                        .get_mut(alias)
                        .unwrap()
                        .sort_unstable();
                }
            }
        } else {
            metadata
                .by_name
                .entry(dc.name.clone())
                .or_default()
                .push(id);
            metadata.by_name.get_mut(&dc.name).unwrap().sort_unstable();
            metadata
                .by_type_name
                .entry(type_name.clone())
                .or_default()
                .push(id);
            if let Some(alias) = &dc.qualified_name {
                metadata
                    .by_qualified_name
                    .entry(alias.clone())
                    .or_default()
                    .push(id);
                metadata
                    .by_qualified_name
                    .get_mut(alias)
                    .unwrap()
                    .sort_unstable();
            }
            metadata.by_identity.insert(dc.identity.clone(), id);
        }
        metadata.by_id.insert(id, dc);
        type_name
    }

    /// Preserve constructor declaration order independently of wire row order.
    /// Equal tags use the host ID as a deterministic tie breaker for aliases.
    fn sort_type_name_bucket(&mut self, type_name: &str) {
        let metadata = Arc::make_mut(&mut self.metadata);
        if let Some(bucket) = metadata.by_type_name.get_mut(type_name) {
            let by_id = &metadata.by_id;
            bucket.sort_by_key(|i| (by_id.get(i).map(|d| d.tag).unwrap_or(0), i.0));
        }
    }

    /// Batch sibling of [`Self::insert_checked`]: performs the same
    /// collision-checked insert for every constructor in `dcs`, but sorts
    /// each AFFECTED `by_type_name` bucket exactly once at the end instead of
    /// once per insert.
    ///
    /// Semantics match folding `insert_checked` over the same sequence
    /// exactly, INCLUDING on error: processing stops at the first collision
    /// (constructors after it are not applied), and every bucket touched by
    /// the constructors that WERE applied before the failure is still sorted
    /// before returning — so the table left behind by an error is byte-for-
    /// byte the same table `for dc in dcs { insert_checked(dc)? }` would have
    /// left at the same point, not a batch-deferred half state.
    ///
    /// Routes through [`Self::check_collision`] — the same guard
    /// `insert_checked` uses, including the exact-identity axis — so a
    /// collision on either axis stops the batch exactly where the sequential
    /// fold would.
    pub fn extend_checked<I>(&mut self, dcs: I) -> Result<(), DataConCollision>
    where
        I: IntoIterator<Item = DataCon>,
    {
        let mut affected: HashSet<String> = HashSet::new();
        let mut result = Ok(());
        for dc in dcs {
            if let Err(e) = self.check_collision(&dc) {
                result = Err(e);
                break;
            }
            let type_name = self.upsert_no_sort(dc);
            affected.insert(type_name);
        }
        for type_name in &affected {
            self.sort_type_name_bucket(type_name);
        }
        result
    }

    /// Look up by DataConId.
    pub fn get(&self, id: DataConId) -> Option<&DataCon> {
        self.metadata.by_id.get(&id)
    }

    /// Look up by module-qualified name (e.g., "Data.Map.Bin"), returning the DataConId.
    pub fn get_by_qualified_name(&self, qname: &str) -> Option<DataConId> {
        let ids = self.metadata.by_qualified_name.get(qname)?;
        (ids.len() == 1).then(|| ids[0])
    }

    /// Resolve the exact compiler-issued symbol.
    pub fn get_by_identity(&self, identity: &SymbolIdentity) -> Option<DataConId> {
        self.metadata.by_identity.get(identity).copied()
    }

    /// A spelling narrows candidates; it never chooses between nominal owners.
    pub fn get_by_qualified_name_checked(
        &self,
        name: &str,
        arity: u32,
    ) -> Result<Option<DataConId>, AmbiguousDataCon> {
        self.unique_candidate(self.metadata.by_qualified_name.get(name), name, arity)
    }

    /// Record field labels for a constructor, in field order. Returns `None` for
    /// positional (non-record) constructors. Used by rendering to emit named-field
    /// JSON objects instead of positional `{"constructor", "fields"}`.
    pub fn field_labels_of(&self, id: DataConId) -> Option<&[String]> {
        self.metadata.field_labels.get(&id).map(Vec::as_slice)
    }

    /// Attach record field labels to a constructor id. Empty label lists are
    /// ignored (positional constructors carry no entry).
    pub fn set_field_labels(&mut self, id: DataConId, labels: Vec<String>) {
        if !labels.is_empty() {
            Arc::make_mut(&mut self.metadata)
                .field_labels
                .insert(id, labels);
        }
    }

    /// Iterate over all `(DataConId, labels)` field-label entries (for serialization).
    pub fn field_labels_iter(&self) -> impl Iterator<Item = (DataConId, &[String])> {
        self.metadata
            .field_labels
            .iter()
            .map(|(&id, v)| (id, v.as_slice()))
    }

    /// Rendered field types for a constructor, in field order. `None` for a
    /// nullary constructor (no fields at all).
    pub fn field_types_of(&self, id: DataConId) -> Option<&[String]> {
        self.metadata.field_types.get(&id).map(Vec::as_slice)
    }

    /// Attach rendered field types to a constructor id. Empty type lists are
    /// ignored (nullary constructors carry no entry) — mirrors
    /// `set_field_labels`.
    pub fn set_field_types(&mut self, id: DataConId, types: Vec<String>) {
        if !types.is_empty() {
            Arc::make_mut(&mut self.metadata)
                .field_types
                .insert(id, types);
        }
    }

    /// Iterate over all `(DataConId, types)` field-type entries (for serialization).
    pub fn field_types_iter(&self) -> impl Iterator<Item = (DataConId, &[String])> {
        self.metadata
            .field_types
            .iter()
            .map(|(&id, v)| (id, v.as_slice()))
    }

    /// Look up by name, returning the DataConId.
    ///
    /// Returns `None` when multiple constructors share the same unqualified name,
    /// since the result would be ambiguous. Use `get_by_qualified_name`,
    /// or a checked arity lookup instead.
    pub fn get_by_name(&self, name: &str) -> Option<DataConId> {
        self.metadata
            .by_name
            .get(name)
            .and_then(|vec| (vec.len() == 1).then(|| vec[0]))
    }

    /// Refuse ambiguity instead of depending on insertion order.
    pub fn get_by_name_arity_checked(
        &self,
        name: &str,
        arity: u32,
    ) -> Result<Option<DataConId>, AmbiguousDataCon> {
        self.unique_candidate(self.metadata.by_name.get(name), name, arity)
    }

    fn unique_candidate(
        &self,
        ids: Option<&Vec<DataConId>>,
        name: &str,
        arity: u32,
    ) -> Result<Option<DataConId>, AmbiguousDataCon> {
        let Some(ids) = ids else {
            return Ok(None);
        };
        let candidates: Vec<&DataCon> = ids
            .iter()
            .filter_map(|id| self.get(*id))
            .filter(|dc| dc.rep_arity == arity)
            .collect();
        match candidates.as_slice() {
            [] => Ok(None),
            [dc] => Ok(Some(dc.id)),
            _ => {
                let mut identities: Vec<_> =
                    candidates.iter().map(|dc| dc.identity.clone()).collect();
                identities.sort();
                Err(AmbiguousDataCon {
                    name: name.to_owned(),
                    arity,
                    candidates: identities,
                })
            }
        }
    }

    /// Return all DataConIds sharing a given name (in insertion order).
    pub fn get_all_by_name(&self, name: &str) -> &[DataConId] {
        self.metadata
            .by_name
            .get(name)
            .map_or(&[], |v| v.as_slice())
    }

    /// Resolve a rendered parent-type name (e.g. "Verdict") to its full
    /// constructor set, in declaration order. Empty when no constructor was
    /// recorded against that type name.
    pub fn constructors_of_type(&self, type_name: &str) -> Vec<DataConId> {
        self.metadata
            .by_type_name
            .get(type_name)
            .cloned()
            .unwrap_or_default()
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.metadata.by_id.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.metadata.by_id.is_empty()
    }

    /// Look up constructor name by DataConId.
    pub fn name_of(&self, id: DataConId) -> Option<&str> {
        self.metadata.by_id.get(&id).map(|dc| dc.name.as_str())
    }

    /// Iterate over all data constructors.
    pub fn iter(&self) -> impl Iterator<Item = &DataCon> {
        self.metadata.by_id.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datacon::SrcBang;

    #[test]
    fn response_contexts_share_metadata_but_replace_json_authority() {
        let mut table = DataConTable::new();
        table
            .insert_checked(make_datacon(1, "Example", 1, 0))
            .expect("valid fixture metadata");
        let layout = JsonLayout {
            object: 1,
            array: 2,
            string: 3,
            number: 4,
            bool_: 5,
            null: 6,
            map_bin: 7,
            map_tip: 8,
            true_: 9,
            false_: 10,
            cons: 11,
            nil: 12,
            scientific: 13,
            integer_small: 14,
            integer_positive: 15,
            integer_negative: 16,
            text: 17,
            int: 18,
        }
        .map(DataConId);
        let first = table.with_json_layout(Some(layout));
        let other_layout = layout.map(|id| DataConId(id.0 + 100));
        let second = first.with_json_layout(Some(other_layout));
        let without_authority = second.with_json_layout(None);
        for context in [&first, &second, &without_authority] {
            assert!(Arc::ptr_eq(&table.metadata, &context.metadata));
            assert_eq!(context.get(DataConId(1)), table.get(DataConId(1)));
        }
        assert_eq!(first.json_layout(), Some(&layout));
        assert_eq!(second.json_layout(), Some(&other_layout));
        assert!(table.json_layout().is_none());
        assert!(without_authority.json_layout().is_none());
    }

    #[test]
    fn shared_table_mutation_keeps_snapshots_and_indexes_independent() {
        let mut table = DataConTable::new();
        table
            .insert_checked(make_datacon_typed(1, "First", 1, 1, "Example"))
            .expect("valid fixture metadata");
        table.set_field_labels(DataConId(1), vec!["original".into()]);
        table.set_field_types(DataConId(1), vec!["Int".into()]);
        let snapshot = table.clone();
        assert!(Arc::ptr_eq(&table.metadata, &snapshot.metadata));
        table
            .extend_checked([
                make_datacon_typed(3, "Third", 3, 0, "Example"),
                make_datacon_typed(2, "Second", 2, 0, "Example"),
            ])
            .unwrap();
        assert!(!Arc::ptr_eq(&table.metadata, &snapshot.metadata));
        let metadata = Arc::as_ptr(&table.metadata);
        table.set_field_labels(DataConId(1), vec!["changed".into()]);
        table.set_field_types(DataConId(1), vec!["Text".into()]);
        assert_eq!(
            Arc::as_ptr(&table.metadata),
            metadata,
            "unique metadata is mutated in place"
        );
        assert_eq!(snapshot.len(), 1);
        assert!(snapshot.get_by_name("Second").is_none());
        assert_eq!(
            snapshot.field_labels_of(DataConId(1)).unwrap(),
            ["original"]
        );
        assert_eq!(snapshot.field_types_of(DataConId(1)).unwrap(), ["Int"]);
        assert_eq!(
            table.constructors_of_type("Example"),
            [DataConId(1), DataConId(2), DataConId(3)]
        );
        assert_eq!(table.field_labels_of(DataConId(1)).unwrap(), ["changed"]);
        assert_eq!(table.field_types_of(DataConId(1)).unwrap(), ["Text"]);
    }

    fn make_datacon(id: u64, name: &str, tag: u32, rep_arity: u32) -> DataCon {
        let mut row = nominal_row(id, "fixture", "Fixture", name, rep_arity);
        row.tag = tag;
        row
    }

    fn make_datacon_typed(id: u64, name: &str, tag: u32, arity: u32, type_name: &str) -> DataCon {
        let mut row = make_datacon(id, name, tag, arity);
        row.type_name = type_name.into();
        row
    }

    fn nominal_row(id: u64, unit: &str, module: &str, occurrence: &str, arity: u32) -> DataCon {
        DataCon {
            identity: SymbolIdentity {
                unit: unit.into(),
                module: module.into(),
                namespace: "constructor".into(),
                occurrence: occurrence.into(),
                record_parent: None,
            },
            id: DataConId(id),
            name: occurrence.into(),
            tag: 1,
            rep_arity: arity,
            field_bangs: vec![],
            qualified_name: Some("Shared.Ticket".into()),
            type_name: "Ticket".into(),
        }
    }

    #[test]
    fn homonymous_units_at_one_host_id_refuse_without_replacement() {
        let first = nominal_row(18329113560068104472, "unit-one", "Homonymous", "Ticket", 0);
        let second = nominal_row(first.id.0, "unit-two", "Homonymous", "Ticket", 0);
        let mut table = DataConTable::new();
        table.insert_checked(first.clone()).unwrap();
        let snapshot = table.clone();
        assert!(matches!(
            table.insert_checked(second),
            Err(DataConCollision::Id { .. })
        ));
        assert_eq!(table, snapshot);
        assert_eq!(table.get_by_identity(&first.identity), Some(first.id));
    }

    #[test]
    fn distinct_exact_owners_share_spelling_without_granting_unique_lookup() {
        let first = nominal_row(1, "unit-one", "Homonymous", "Ticket", 0);
        let second = nominal_row(2, "unit-two", "Homonymous", "Ticket", 0);
        let mut table = DataConTable::new();
        table
            .extend_checked([first.clone(), second.clone()])
            .unwrap();
        assert_eq!(table.get_by_identity(&first.identity), Some(first.id));
        assert_eq!(table.get_by_identity(&second.identity), Some(second.id));
        assert_eq!(table.get_by_qualified_name("Shared.Ticket"), None);
        assert!(table
            .get_by_qualified_name_checked("Shared.Ticket", 0)
            .is_err());
        assert!(table.get_by_name_arity_checked("Ticket", 0).is_err());
    }

    #[test]
    fn one_exact_identity_cannot_claim_two_host_ids() {
        let first = nominal_row(1, "unit-one", "Homonymous", "Ticket", 0);
        let second = DataCon {
            id: DataConId(2),
            ..first.clone()
        };
        let mut table = DataConTable::new();
        table.insert_checked(first).unwrap();
        assert!(matches!(
            table.insert_checked(second),
            Err(DataConCollision::Identity { .. })
        ));
    }

    #[test]
    fn reencounter_is_idempotent_and_shape_changes_refuse() {
        let first = nominal_row(1, "unit-one", "Homonymous", "Ticket", 0);
        let mut table = DataConTable::new();
        table.insert_checked(first.clone()).unwrap();
        let snapshot = table.clone();
        table.insert_checked(first.clone()).unwrap();
        assert_eq!(table, snapshot);
        assert!(matches!(
            table.insert_checked(DataCon {
                tag: 2,
                ..first.clone()
            }),
            Err(DataConCollision::Shape { .. })
        ));
        assert!(matches!(
            table.insert_checked(DataCon {
                rep_arity: 1,
                ..first
            }),
            Err(DataConCollision::Shape { .. })
        ));
        assert_eq!(table, snapshot);
    }

    #[test]
    fn malformed_nominal_identity_is_not_ingested() {
        let mut row = nominal_row(1, "unit-one", "Homonymous", "Ticket", 0);
        row.identity.unit.clear();
        let mut table = DataConTable::new();
        assert!(matches!(
            table.insert_checked(row),
            Err(DataConCollision::InvalidIdentity { .. })
        ));
        assert!(table.is_empty());
    }

    #[test]
    fn checked_batch_retains_only_its_accepted_prefix() {
        let first = nominal_row(1, "unit-one", "Homonymous", "Ticket", 0);
        let second = nominal_row(2, "unit-two", "Homonymous", "Ticket", 0);
        let conflict = nominal_row(1, "unit-three", "Homonymous", "Ticket", 0);
        let untouched = nominal_row(3, "unit-four", "Homonymous", "Ticket", 0);
        let mut table = DataConTable::new();
        assert!(table
            .extend_checked([first.clone(), second.clone(), conflict, untouched])
            .is_err());
        assert_eq!(table.len(), 2);
        assert_eq!(table.constructors_of_type("Ticket"), [first.id, second.id]);
        assert_eq!(table.get(DataConId(3)), None);
    }

    #[test]
    fn freer_union_collision_preserves_both_existing_indexes() {
        let union = nominal_row(
            0xFFFF,
            "freer-simple",
            "Data.OpenUnion.Internal",
            "Union",
            1,
        );
        let other = nominal_row(0xFFFF, "ghc", "GHC.Driver.Session", "DynFlags", 0);
        let mut table = DataConTable::new();
        table.insert_checked(union.clone()).unwrap();
        assert!(matches!(
            table.insert_checked(other),
            Err(DataConCollision::Id { .. })
        ));
        assert_eq!(table.get_by_identity(&union.identity), Some(union.id));
        assert_eq!(table.get_by_name("Union"), Some(union.id));
    }
}
