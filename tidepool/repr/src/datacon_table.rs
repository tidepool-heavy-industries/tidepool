//! A lookup table for data constructor metadata.

use crate::datacon::DataCon;
use crate::execution_schema::{ConstructorDecl, ConstructorIdentity, JsonLayout, PreparedProgram};
use crate::types::DataConId;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// A checked metadata ingestion cannot change a constructor's nominal owner.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DataConCollision {
    #[error("invalid constructor symbol {identity:?}: {detail}")]
    InvalidIdentity {
        identity: ConstructorIdentity,
        detail: &'static str,
    },
    #[error("DataConId {id:?} belongs to {first:?}, not {second:?}")]
    Id {
        id: DataConId,
        first: ConstructorIdentity,
        second: ConstructorIdentity,
    },
    #[error("constructor {identity:?} has conflicting tags or arities: {first_tag}/{first_arity}, {second_tag}/{second_arity}")]
    Shape {
        identity: ConstructorIdentity,
        first_tag: u32,
        first_arity: u32,
        second_tag: u32,
        second_arity: u32,
    },
    #[error("constructor {identity:?} claims two DataConIds: {first_id:?}, {second_id:?}")]
    Identity {
        identity: ConstructorIdentity,
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
    pub candidates: Vec<ConstructorIdentity>,
}

/// The complete output table must agree with every emitted constructor.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConstructorMetadataMismatch {
    #[error("constructor {identity:?} has no metadata at {host_id:?}")]
    Missing {
        host_id: DataConId,
        identity: ConstructorIdentity,
    },
    #[error("constructor {prepared:?} at {host_id:?} resolves to {metadata:?}")]
    Identity {
        host_id: DataConId,
        prepared: ConstructorIdentity,
        metadata: ConstructorIdentity,
    },
    #[error("constructor {identity:?} at {host_id:?} has conflicting tag/arity metadata")]
    Shape {
        host_id: DataConId,
        identity: ConstructorIdentity,
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
    by_identity: HashMap<ConstructorIdentity, DataConId>,
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
    /// Stage dispatch facts for one admitted native batch. Constructor IDs and
    /// physical shape come from its declarations; compiler render annotations
    /// enrich those facts without supplying JSON or request-site authority.
    pub fn stage_constructor_admission<'a>(
        &self,
        incoming: Option<&Self>,
        declarations: impl IntoIterator<Item = &'a ConstructorDecl>,
    ) -> Result<Self, DataConCollision> {
        let mut candidate = self.with_json_layout(None);
        let mut affected = HashSet::new();
        if let Some(incoming) = incoming {
            for row in incoming.iter() {
                candidate.check_collision(row)?;
                let mut enriched = row.clone();
                if let Some(existing) = candidate.get(row.id) {
                    if enriched.qualified_name.is_none() {
                        enriched.qualified_name.clone_from(&existing.qualified_name);
                    }
                    if enriched.field_bangs.is_empty() {
                        enriched.field_bangs.clone_from(&existing.field_bangs);
                    }
                    if enriched.type_name.is_empty() {
                        enriched.type_name.clone_from(&existing.type_name);
                    }
                }
                if candidate.get(row.id) != Some(&enriched) {
                    affected.insert(candidate.upsert_no_sort(enriched));
                }
                if let Some(labels) = incoming.field_labels_of(row.id) {
                    if candidate.field_labels_of(row.id) != Some(labels) {
                        candidate.set_field_labels(row.id, labels.to_vec());
                    }
                }
                if let Some(types) = incoming.field_types_of(row.id) {
                    if candidate.field_types_of(row.id) != Some(types) {
                        candidate.set_field_types(row.id, types.to_vec());
                    }
                }
            }
        }
        for declaration in declarations {
            let symbol = &declaration.identity.symbol;
            let module = match symbol.module.as_str() {
                "Data.Text.Internal" => "Data.Text",
                "GHC.Internal.Maybe" => "GHC.Maybe",
                "GHC.Internal.Data.Either" => "Data.Either",
                module => module,
            };
            let projected = DataCon {
                identity: declaration.identity.clone(),
                id: declaration.host_id,
                name: symbol.occurrence.clone(),
                tag: declaration.tag,
                rep_arity: u32::try_from(declaration.field_reps.len()).map_err(|_| {
                    DataConCollision::InvalidIdentity {
                        identity: declaration.identity.clone(),
                        detail: "representation arity exceeds the metadata format",
                    }
                })?,
                field_bangs: Vec::new(),
                qualified_name: Some(format!("{module}.{}", symbol.occurrence)),
                type_name: declaration.identity.family.symbol.occurrence.clone(),
            };
            candidate.check_collision(&projected)?;
            match candidate.get(projected.id) {
                None => {
                    affected.insert(candidate.upsert_no_sort(projected));
                }
                Some(existing) if existing.qualified_name.is_none() => {
                    let mut enriched = existing.clone();
                    enriched.qualified_name = projected.qualified_name;
                    affected.insert(candidate.upsert_no_sort(enriched));
                }
                Some(_) => {}
            }
        }
        for name in affected {
            candidate.sort_type_name_bucket(&name);
        }
        Ok(candidate)
    }

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

    pub(crate) fn check_identity(
        identity: &ConstructorIdentity,
        name: &str,
    ) -> Result<(), DataConCollision> {
        if identity.symbol.unit.is_empty()
            || identity.symbol.module.is_empty()
            || identity.symbol.occurrence.is_empty()
            || identity.symbol.namespace != "constructor"
            || identity.symbol.occurrence != name
            || identity
                .symbol
                .record_parent
                .as_ref()
                .is_some_and(|parent| parent.is_empty())
            || identity.family.symbol.unit.is_empty()
            || identity.family.symbol.module.is_empty()
            || identity.family.symbol.occurrence.is_empty()
            || !matches!(identity.family.symbol.namespace.as_str(), "type" | "data")
            || identity.family.symbol.record_parent.is_some()
        {
            return Err(DataConCollision::InvalidIdentity {
                identity: identity.clone(),
                detail: "expected complete constructor and type family symbols agreeing with the constructor name",
            });
        }
        Ok(())
    }

    fn check_collision(&self, dc: &DataCon) -> Result<(), DataConCollision> {
        let identity = &dc.identity;
        Self::check_identity(identity, &dc.name)?;
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
    pub fn get_by_identity(&self, identity: &ConstructorIdentity) -> Option<DataConId> {
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
    /// or an exact identity lookup instead.
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
        let mut candidates = ids.iter().filter_map(|id| self.get(*id));
        let Some(first) = candidates.next() else {
            return Ok(None);
        };
        if candidates.any(|candidate| candidate.identity != first.identity) {
            let mut identities: Vec<_> = ids
                .iter()
                .filter_map(|id| self.get(*id))
                .map(|candidate| candidate.identity.clone())
                .collect();
            identities.sort();
            identities.dedup();
            return Err(AmbiguousDataCon {
                name: name.to_owned(),
                arity,
                candidates: identities,
            });
        }
        Ok((first.rep_arity == arity).then_some(first.id))
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

    proptest::proptest! {
        #[test]
        fn staged_batches_match_independent_identity_history(
            batches in proptest::collection::vec(proptest::collection::vec(1u64..32, 0..12), 0..16)
        ) {
            let mut table = DataConTable::new();
            let mut expected = std::collections::BTreeMap::new();
            for batch in batches {
                let expected_before = expected.clone();
                let mut incoming = DataConTable::new();
                for id in batch {
                    let row = nominal_row(id, "history-unit", "History", &format!("C{id}"), 0);
                    incoming.insert_checked(row.clone()).unwrap();
                    expected.insert(id, row);
                }
                let before = table.clone();
                table = table.stage_constructor_admission(Some(&incoming), []).unwrap();
                proptest::prop_assert_eq!(table.len(), expected.len());
                for (id, row) in &expected {
                    proptest::prop_assert_eq!(table.get(DataConId(*id)), Some(row));
                }
                proptest::prop_assert_eq!(before.len(), expected_before.len());
                for (id, row) in &expected_before {
                    proptest::prop_assert_eq!(before.get(DataConId(*id)), Some(row));
                }
                let repeated = table.stage_constructor_admission(Some(&incoming), []).unwrap();
                proptest::prop_assert!(Arc::ptr_eq(&table.metadata, &repeated.metadata));
            }
        }
    }

    #[test]
    fn staged_admission_preserves_annotations_and_sharing_and_refuses_atomically() {
        let mut original = DataConTable::new();
        original
            .insert_checked(make_datacon(1, "First", 1, 0))
            .unwrap();
        original.set_field_labels(DataConId(1), vec!["label".into()]);
        original.set_field_types(DataConId(1), vec!["Text".into()]);
        let unchanged = original
            .stage_constructor_admission(Some(&original), [])
            .unwrap();
        assert!(Arc::ptr_eq(&original.metadata, &unchanged.metadata));

        let mut incoming = DataConTable::new();
        let mut thin = original.get(DataConId(1)).unwrap().clone();
        thin.qualified_name = None;
        incoming.insert_checked(thin).unwrap();
        incoming.set_field_types(DataConId(1), vec!["Int".into()]);
        let enriched = original
            .stage_constructor_admission(Some(&incoming), [])
            .unwrap();
        assert_eq!(enriched.get(DataConId(1)), original.get(DataConId(1)));
        assert_eq!(
            enriched.field_labels_of(DataConId(1)),
            original.field_labels_of(DataConId(1))
        );
        assert_eq!(enriched.field_types_of(DataConId(1)).unwrap(), ["Int"]);
        assert_eq!(original.field_types_of(DataConId(1)).unwrap(), ["Text"]);

        incoming
            .insert_checked(make_datacon(2, "Second", 2, 0))
            .unwrap();
        let mut conflicting = make_datacon(1, "Impostor", 1, 0);
        conflicting.identity.symbol.unit = "another-owner".into();
        let mut conflict_batch = DataConTable::new();
        conflict_batch
            .insert_checked(make_datacon(2, "Second", 2, 0))
            .unwrap();
        conflict_batch.insert_checked(conflicting).unwrap();
        assert!(original
            .stage_constructor_admission(Some(&conflict_batch), [])
            .is_err());
        assert_eq!(original.len(), 1);
        assert!(original.get(DataConId(2)).is_none());
    }

    #[test]
    fn projected_aliases_preserve_nominal_identity_and_issued_shape() {
        use crate::execution_schema::{CheckedLayout, RuntimeRep};
        for (module, alias) in [
            ("Data.Text.Internal", "Data.Text"),
            ("GHC.Internal.Maybe", "GHC.Maybe"),
            ("GHC.Internal.Data.Either", "Data.Either"),
            ("Ordinary.Module", "Ordinary.Module"),
        ] {
            let row = nominal_row(18361356118689931113, "exact-unit", module, "RecipeRoot", 0);
            let declaration = ConstructorDecl {
                identity: row.identity.clone(),
                host_id: row.id,
                result_rep: RuntimeRep::LiftedRef,
                field_reps: vec![],
                strict_fields: vec![],
                layout: CheckedLayout {
                    fields: vec![],
                    alignment: 8,
                    payload_size: 0,
                    root_mask: vec![],
                },
                tag: 3,
                family_size: 7,
            };
            let table = DataConTable::new()
                .stage_constructor_admission(None, [&declaration])
                .unwrap();
            let projected = table.get(row.id).unwrap();
            assert_eq!(projected.identity, row.identity);
            assert_eq!(projected.tag, 3);
            assert_eq!(projected.rep_arity, 0);
            assert_eq!(
                projected.qualified_name.as_deref(),
                Some(format!("{alias}.RecipeRoot").as_str())
            );
            assert!(projected.field_bangs.is_empty());
            assert!(table.json_layout().is_none());
            let repeated = table
                .stage_constructor_admission(None, [&declaration])
                .unwrap();
            assert!(Arc::ptr_eq(&table.metadata, &repeated.metadata));
        }
    }

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
            identity: crate::execution_schema::testing::metadata_identity(
                crate::execution_schema::SymbolIdentity {
                    unit: unit.into(),
                    module: module.into(),
                    namespace: "constructor".into(),
                    occurrence: occurrence.into(),
                    record_parent: None,
                },
                "Ticket",
            ),
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
    fn malformed_family_identity_refuses_standalone_table_admission() {
        for corrupt in 0..5 {
            let mut row = nominal_row(1, "unit", "Module", "Ticket", 1);
            match corrupt {
                0 => row.identity.family.symbol.unit.clear(),
                1 => row.identity.family.symbol.module.clear(),
                2 => row.identity.family.symbol.occurrence.clear(),
                3 => row.identity.family.symbol.namespace = "value".into(),
                _ => row.identity.family.symbol.record_parent = Some("Parent".into()),
            }
            let mut table = DataConTable::new();
            assert!(matches!(
                table.insert_checked(row),
                Err(DataConCollision::InvalidIdentity { .. })
            ));
            assert!(table.is_empty());
        }
    }

    #[test]
    fn family_versions_refuse_spelling_selection_even_when_arities_differ() {
        let first = nominal_row(1, "unit", "Module", "Ticket", 1);
        let mut second = first.clone();
        second.id = DataConId(2);
        second.rep_arity = 2;
        second.identity.family.version = crate::execution_schema::TypeVersion::GhcAbi([1; 16]);
        let mut table = DataConTable::new();
        table
            .extend_checked([first.clone(), second.clone(), first.clone()])
            .unwrap();
        for arity in [1, 2, 3] {
            assert!(table.get_by_name_arity_checked("Ticket", arity).is_err());
            assert!(table
                .get_by_qualified_name_checked("Shared.Ticket", arity)
                .is_err());
        }
        assert_eq!(table.get_by_identity(&first.identity), Some(first.id));
        assert_eq!(table.get_by_identity(&second.identity), Some(second.id));
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(64))]
        #[test]
        fn versioned_metadata_histories_match_revision_oracle(
            operations in proptest::collection::vec((0_u8..4, 0_u8..3), 1..40)
        ) {
            let mut table = DataConTable::new();
            let mut model = std::collections::BTreeMap::new();
            for (revision, operation) in operations {
                let mut row = nominal_row(u64::from(revision) + 1, "unit", "Module", "Ticket", u32::from(revision) + 1);
                row.identity.family.version = crate::execution_schema::TypeVersion::GhcAbi([revision; 16]);
                let before = table.clone();
                match operation {
                    0 => {
                        table.insert_checked(row.clone()).unwrap();
                        model.insert(revision, row.clone());
                    }
                    1 if !model.is_empty() => {
                        row.id = model.values().next().unwrap().id;
                        if model.contains_key(&revision) && model[&revision].id == row.id {
                            table.insert_checked(row).unwrap();
                        } else {
                            proptest::prop_assert!(table.insert_checked(row).is_err());
                            proptest::prop_assert_eq!(&table, &before);
                        }
                    }
                    2 if model.contains_key(&revision) => {
                        row.rep_arity += 1;
                        proptest::prop_assert!(table.insert_checked(row).is_err());
                        proptest::prop_assert_eq!(&table, &before);
                    }
                    _ => {}
                }
                let restored = crate::serial::read_metadata(&crate::serial::write_metadata(&table, &crate::serial::MetaWarnings::default()).unwrap()).unwrap().0;
                proptest::prop_assert_eq!(&restored, &table);
                proptest::prop_assert_eq!(table.len(), model.len());
                for expected in model.values() {
                    proptest::prop_assert_eq!(table.get_by_identity(&expected.identity), Some(expected.id));
                }
                let spelling = table.get_by_name_arity_checked("Ticket", u32::from(revision) + 1);
                proptest::prop_assert_eq!(spelling.is_err(), model.len() > 1);
            }
        }
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
        row.identity.symbol.unit.clear();
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
