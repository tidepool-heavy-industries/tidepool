//! A lookup table for data constructor metadata.

use crate::datacon::DataCon;
use crate::types::{AltCon, DataConId};
use std::collections::{HashMap, HashSet};

/// The module-qualified identity of a constructor, used to distinguish a true
/// varId collision from a harmless re-encounter of the same constructor. Falls
/// back to the unqualified name when no qualified name is recorded.
fn dc_identity(dc: &DataCon) -> &str {
    dc.qualified_name.as_deref().unwrap_or(&dc.name)
}

/// A collision `insert_checked`/`extend_checked` refuses to silently absorb.
///
/// Two independent axes, both loud rather than last-writer-wins:
///
/// - [`Self::Id`]: two DISTINCT constructors (different module-qualified
///   identity) sharing one [`DataConId`] — i.e. a 56-bit `stableVarId` hash
///   collision. This is the silent-eviction class that took out freer-simple's
///   `Union`: a dcid-keyed map would overwrite one constructor's entry with
///   the other, and the lost constructor then resolves to `None` (or to the
///   wrong metadata) at effect-machine setup / case dispatch.
/// - [`Self::QualifiedName`]: two DISTINCT [`DataConId`]s claiming one
///   module-qualified name. `by_qualified_name` (and `freer_names::resolve`,
///   which consults it first) can only remember one id per name, so the
///   other would silently drop out of qualified-name resolution — the mirror
///   image of the `Id` case. A real accumulated session table carries zero
///   such duplicates (library constructor ids are stable across extract
///   invocations), so this indicates the extractor's id minting changed, not
///   a shape to tie-break.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DataConCollision {
    #[error(
        "DataConId {:#018x} collision: two distinct constructors hash to the same \
         varId — '{first}' and '{second}'. One would silently shadow the other in \
         the DataConTable (the freer-simple Union eviction class). This is a \
         Haskell-side stableVarId hash collision; rename one constructor or widen \
         the hash. (Set TIDEPOOL_VARID_AUDIT=1 on the extract for the forensic dump.)",
        .id.0
    )]
    Id {
        /// The colliding identifier.
        id: DataConId,
        /// Module-qualified identity of the constructor already in the table.
        first: String,
        /// Module-qualified identity of the constructor that collided with it.
        second: String,
    },
    #[error(
        "qualified name {qualified_name:?} collision: two distinct DataConIds claim \
         it — {:#018x} ('{first}') and {:#018x} ('{second}'). `by_qualified_name` \
         (and `freer_names::resolve`, which consults it first) can only remember \
         one id per name, so the other would silently drop out of qualified-name \
         resolution. A real accumulated session table carries zero such \
         duplicates, so this indicates the extractor's id minting changed; it is \
         not something to tie-break.",
        .first_id.0, .second_id.0
    )]
    QualifiedName {
        /// The qualified name both ids claim.
        qualified_name: String,
        /// The id already holding the qualified-name mapping.
        first_id: DataConId,
        /// Identity of the constructor already holding the mapping.
        first: String,
        /// The id that tried to claim the same qualified name.
        second_id: DataConId,
        /// Identity of the constructor that collided with it.
        second: String,
    },
}

/// Two or more DISTINCT constructors share both an unqualified name and a
/// requested representation arity. [`DataConTable::get_by_name_arity_checked`]
/// refuses to silently pick one (insertion order deciding encoding is exactly
/// the freer-simple `Union`-eviction class of bug, one query-time step
/// removed) — the caller must disambiguate via [`DataConTable::get_companion`]
/// (sibling-group identity) or [`DataConTable::get_by_qualified_name`]
/// (module-qualified identity) instead.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "ambiguous DataCon lookup: {} constructors named {name:?} with arity {arity} — {candidates:?}. \
     Insertion order cannot decide which one is correct; disambiguate via get_companion \
     (sibling-group identity) or get_by_qualified_name (module-qualified identity).",
    candidates.len()
)]
pub struct AmbiguousDataCon {
    /// The unqualified name that was looked up.
    pub name: String,
    /// The requested representation arity.
    pub arity: u32,
    /// Module-qualified identity (falling back to unqualified name) of every
    /// constructor that matched both the name and the arity.
    pub candidates: Vec<String>,
}

/// Lookup table for data constructor metadata.
/// Populated during deserialization from the CBOR metadata section.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataConTable {
    /// Mapping from unique DataConId to its metadata.
    by_id: HashMap<DataConId, DataCon>,
    /// Mapping from unqualified name to all DataConIds sharing that name.
    by_name: HashMap<String, Vec<DataConId>>,
    /// Mapping from module-qualified name to its DataConId.
    by_qualified_name: HashMap<String, DataConId>,
    /// Mapping from parent-type name (e.g. "Verdict") to all DataConIds of
    /// that type, kept sorted by constructor TAG (declaration order) by
    /// [`Self::sort_type_name_bucket`] — see that function for why insertion
    /// order cannot be trusted to already be in that order.
    by_type_name: HashMap<String, Vec<DataConId>>,
    /// Type-sibling groups: DataConIds that appear together in case branches.
    /// If Bin and Tip appear as alternatives in the same Case, they're siblings.
    siblings: HashMap<DataConId, Vec<DataConId>>,
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

    /// Insert a data constructor, refusing to silently overwrite a DISTINCT
    /// constructor already bound to the same id (a `stableVarId` collision).
    ///
    /// A re-encounter of the SAME constructor (identical module-qualified
    /// identity, AND agreeing tag/rep_arity) is idempotent — the table load
    /// legitimately sees a constructor from several metadata sources. An
    /// agreeing identity that disagrees on tag or rep_arity is still an error:
    /// two encodings of "the same" constructor that disagree on its actual
    /// shape indicate a corrupt or mismatched metadata source, not a harmless
    /// re-encounter, and silently keeping the last-inserted shape (as plain
    /// `insert` would) risks a runtime tag/arity mismatch downstream.
    ///
    /// This is the always-on table-integrity guard; it covers every producer,
    /// including future non-extract ones. (The Haskell extractor additionally
    /// stops coalescing colliding entries so they actually reach this check.)
    ///
    /// Also guards the `by_qualified_name` axis: two DISTINCT ids claiming one
    /// qualified name is a hard error too — see [`Self::check_collision`].
    pub fn insert_checked(&mut self, dc: DataCon) -> Result<(), DataConCollision> {
        self.check_collision(&dc)?;
        self.insert(dc);
        Ok(())
    }

    /// Check `dc` against both collision axes without mutating the table —
    /// the by-id axis (a `stableVarId` hash collision between two distinct
    /// constructors) and the `by_qualified_name` axis (two distinct
    /// [`DataConId`]s claiming one qualified name). A re-encounter of the SAME
    /// id or the SAME id already owning a qualified name is not a collision.
    ///
    /// Shared by [`Self::insert_checked`] and [`Self::extend_checked`] so both
    /// ingestion routes see byte-identical guard logic rather than two
    /// hand-maintained copies that could drift.
    fn check_collision(&self, dc: &DataCon) -> Result<(), DataConCollision> {
        if let Some(existing) = self.by_id.get(&dc.id) {
            if dc_identity(existing) != dc_identity(dc) {
                return Err(DataConCollision::Id {
                    id: dc.id,
                    first: dc_identity(existing).to_string(),
                    second: dc_identity(dc).to_string(),
                });
            }
            if existing.tag != dc.tag || existing.rep_arity != dc.rep_arity {
                return Err(DataConCollision::Id {
                    id: dc.id,
                    first: format!(
                        "{} (tag={}, rep_arity={})",
                        dc_identity(existing),
                        existing.tag,
                        existing.rep_arity
                    ),
                    second: format!(
                        "{} (tag={}, rep_arity={})",
                        dc_identity(dc),
                        dc.tag,
                        dc.rep_arity
                    ),
                });
            }
        }
        if let Some(qn) = &dc.qualified_name {
            if let Some(&existing_id) = self.by_qualified_name.get(qn) {
                if existing_id != dc.id {
                    let existing_identity = self
                        .by_id
                        .get(&existing_id)
                        .map(|d| dc_identity(d).to_string())
                        .unwrap_or_else(|| qn.clone());
                    return Err(DataConCollision::QualifiedName {
                        qualified_name: qn.clone(),
                        first_id: existing_id,
                        first: existing_identity,
                        second_id: dc.id,
                        second: dc_identity(dc).to_string(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Insert a data constructor's metadata into every index EXCEPT the final
    /// `by_type_name` bucket sort, returning the type_name whose bucket was
    /// touched (appended to on a new/type-changed entry, or simply left
    /// containing `id` with a possibly-changed tag). Callers are responsible
    /// for sorting that bucket afterward — [`Self::insert`] does so
    /// immediately; [`Self::extend_checked`] batches it across many calls.
    /// Shared by both so there is exactly one implementation of the
    /// retain/re-push bookkeeping.
    fn upsert_no_sort(&mut self, dc: DataCon) -> String {
        let id = dc.id;
        let name = dc.name.clone();
        let qualified_name = dc.qualified_name.clone();
        let type_name = dc.type_name.clone();

        // If we're overwriting an existing entry for this id, remove its old
        // name mapping — but ONLY if it actually changed. Re-pushing an
        // unchanged name would move `id` to the end of its `by_name` Vec, and
        // `get_by_name_arity` treats that Vec's order as a load-bearing
        // insertion-order tie-break between two ids sharing a name+arity. A
        // genuine re-encounter of the same constructor (identical name) must
        // keep its position, so skip the retain/re-push when the name is equal.
        let mut name_unchanged = false;
        let mut type_name_changed = false;
        if let Some(old_dc) = self.by_id.insert(id, dc) {
            if old_dc.name == name {
                name_unchanged = true;
            } else if let Some(vec) = self.by_name.get_mut(&old_dc.name) {
                vec.retain(|&existing| existing != id);
                if vec.is_empty() {
                    self.by_name.remove(&old_dc.name);
                }
            }
            if old_dc.type_name != type_name {
                type_name_changed = true;
                if let Some(vec) = self.by_type_name.get_mut(&old_dc.type_name) {
                    vec.retain(|&existing| existing != id);
                    if vec.is_empty() {
                        self.by_type_name.remove(&old_dc.type_name);
                    }
                }
            }
            if old_dc.qualified_name != qualified_name {
                if let Some(ref old_qn) = old_dc.qualified_name {
                    self.by_qualified_name.remove(old_qn);
                }
            }
        } else {
            type_name_changed = true; // first time this id is seen
        }

        // Insert the mapping for the new name (skipping an unchanged name so
        // its existing Vec position — and thus tie-break order — is preserved).
        if !name_unchanged {
            self.by_name.entry(name).or_default().push(id);
        }
        if type_name_changed {
            self.by_type_name
                .entry(type_name.clone())
                .or_default()
                .push(id);
        }
        if let Some(qn) = qualified_name {
            self.by_qualified_name.insert(qn, id);
        }
        type_name
    }

    /// Sort one `by_type_name` bucket by (tag, id). `by_type_name` orders by
    /// constructor TAG, not insertion order: the Haskell-side merge
    /// (`mergeMetaPreserving`) re-sorts entries by varId before they ever
    /// reach the wire, so insertion order at load time carries no
    /// declaration-order information. `dataConTag` is 1-based per-type
    /// declaration order by construction, so re-sorting the bucket keeps
    /// `constructors_of_type` correct regardless of what order entries arrive
    /// in (and keeps the table canonical/order-independent for equality
    /// comparisons). Must be called for every bucket an upsert touched, even
    /// when the id was already in it — an overwrite may have changed its tag.
    fn sort_type_name_bucket(&mut self, type_name: &str) {
        if let Some(bucket) = self.by_type_name.get_mut(type_name) {
            let by_id = &self.by_id;
            bucket.sort_by_key(|i| (by_id.get(i).map(|d| d.tag).unwrap_or(0), i.0));
        }
    }

    /// Insert a data constructor. Overwrites if id already exists.
    pub fn insert(&mut self, dc: DataCon) {
        let type_name = self.upsert_no_sort(dc);
        self.sort_type_name_bucket(&type_name);
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
    /// `insert_checked` uses, including the `by_qualified_name` axis — so a
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
        self.by_id.get(&id)
    }

    /// Look up by module-qualified name (e.g., "Data.Map.Bin"), returning the DataConId.
    pub fn get_by_qualified_name(&self, qname: &str) -> Option<DataConId> {
        self.by_qualified_name.get(qname).copied()
    }

    /// Record field labels for a constructor, in field order. Returns `None` for
    /// positional (non-record) constructors. Used by rendering to emit named-field
    /// JSON objects instead of positional `{"constructor", "fields"}`.
    pub fn field_labels_of(&self, id: DataConId) -> Option<&[String]> {
        self.field_labels.get(&id).map(Vec::as_slice)
    }

    /// Attach record field labels to a constructor id. Empty label lists are
    /// ignored (positional constructors carry no entry).
    pub fn set_field_labels(&mut self, id: DataConId, labels: Vec<String>) {
        if !labels.is_empty() {
            self.field_labels.insert(id, labels);
        }
    }

    /// Iterate over all `(DataConId, labels)` field-label entries (for serialization).
    pub fn field_labels_iter(&self) -> impl Iterator<Item = (DataConId, &[String])> {
        self.field_labels.iter().map(|(&id, v)| (id, v.as_slice()))
    }

    /// Rendered field types for a constructor, in field order. `None` for a
    /// nullary constructor (no fields at all). Used by
    /// `tidepool_harness::synopsis::type_document` to render a full
    /// GHC-style `data` declaration instead of a names-only synopsis.
    pub fn field_types_of(&self, id: DataConId) -> Option<&[String]> {
        self.field_types.get(&id).map(Vec::as_slice)
    }

    /// Attach rendered field types to a constructor id. Empty type lists are
    /// ignored (nullary constructors carry no entry) — mirrors
    /// `set_field_labels`.
    pub fn set_field_types(&mut self, id: DataConId, types: Vec<String>) {
        if !types.is_empty() {
            self.field_types.insert(id, types);
        }
    }

    /// Iterate over all `(DataConId, types)` field-type entries (for serialization).
    pub fn field_types_iter(&self) -> impl Iterator<Item = (DataConId, &[String])> {
        self.field_types.iter().map(|(&id, v)| (id, v.as_slice()))
    }

    /// Look up by name, returning the DataConId.
    ///
    /// Returns `None` when multiple constructors share the same unqualified name,
    /// since the result would be ambiguous. Use `get_by_qualified_name`,
    /// `get_by_name_arity`, or `get_companion` instead.
    pub fn get_by_name(&self, name: &str) -> Option<DataConId> {
        self.by_name
            .get(name)
            .and_then(|vec| (vec.len() == 1).then(|| vec[0]))
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
                .find(|&&id| self.by_id.get(&id).is_some_and(|dc| dc.rep_arity == arity))
                .copied()
        })
    }

    /// Look up by name AND expected arity, erroring loudly instead of
    /// tie-breaking when more than one constructor shares both — the strict
    /// counterpart of [`Self::get_by_name_arity`]. Callers that must not let
    /// metadata insertion order decide encoding (the derive's default
    /// resolution path, `tidepool-bridge`'s `get_resilient`) use this instead.
    ///
    /// - Zero matches (name absent entirely, or present only at other
    ///   arities): `Ok(None)` — a plain "not found," not an ambiguity.
    /// - Exactly one match: `Ok(Some(id))`.
    /// - Two or more matches: `Err(AmbiguousDataCon)` naming every candidate's
    ///   module-qualified identity (falling back to unqualified name).
    pub fn get_by_name_arity_checked(
        &self,
        name: &str,
        arity: u32,
    ) -> Result<Option<DataConId>, AmbiguousDataCon> {
        let Some(ids) = self.by_name.get(name) else {
            return Ok(None);
        };
        let mut candidates: Vec<DataConId> = ids
            .iter()
            .copied()
            .filter(|id| self.by_id.get(id).is_some_and(|dc| dc.rep_arity == arity))
            .collect();
        match candidates.len() {
            0 => Ok(None),
            1 => Ok(candidates.pop()),
            _ => Err(AmbiguousDataCon {
                name: name.to_string(),
                arity,
                candidates: candidates
                    .iter()
                    .map(|id| {
                        self.by_id
                            .get(id)
                            .map(|dc| dc_identity(dc).to_string())
                            .unwrap_or_else(|| format!("{id:?}"))
                    })
                    .collect(),
            }),
        }
    }

    /// Return all DataConIds sharing a given name (in insertion order).
    pub fn get_all_by_name(&self, name: &str) -> &[DataConId] {
        self.by_name.get(name).map_or(&[], |v| v.as_slice())
    }

    /// Resolve a rendered parent-type name (e.g. "Verdict") to its full
    /// constructor set, in declaration order. Empty when no constructor was
    /// recorded against that type name.
    pub fn constructors_of_type(&self, type_name: &str) -> Vec<DataConId> {
        self.by_type_name
            .get(type_name)
            .cloned()
            .unwrap_or_default()
    }

    /// Find a constructor by name+arity that is a type-sibling of `known_id`.
    ///
    /// Uses sibling groups populated by `populate_siblings_from_expr` — if two
    /// DataCons appear as alternatives in the same case expression, they're from
    /// the same type. Falls back to scanning all entries if no sibling info exists.
    pub fn get_companion(&self, known_id: DataConId, name: &str, arity: u32) -> Option<DataConId> {
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
            type_name: String::new(),
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
            type_name: String::new(),
        }
    }

    fn make_datacon_typed(
        id: u64,
        name: &str,
        tag: u32,
        rep_arity: u32,
        type_name: &str,
    ) -> DataCon {
        DataCon {
            id: DataConId(id),
            name: name.to_string(),
            tag,
            rep_arity,
            field_bangs: vec![],
            qualified_name: None,
            type_name: type_name.to_string(),
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
        // Two "Same" entries — get_by_name returns None (ambiguous), use
        // get_all_by_name instead
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

        assert_eq!(table.get_by_name_arity("Bin", 5), Some(DataConId(100)));
        assert_eq!(table.get_by_name_arity("Bin", 3), Some(DataConId(200)));
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
        assert_eq!(table.get_by_qualified_name("Mod.A.Foo"), Some(DataConId(1)));

        // Overwrite same id with different qualified name
        table.insert(make_datacon_qualified(1, "Foo", 1, 0, "Mod.B.Foo"));
        assert_eq!(table.get_by_qualified_name("Mod.A.Foo"), None);
        assert_eq!(table.get_by_qualified_name("Mod.B.Foo"), Some(DataConId(1)));
    }

    #[test]
    fn test_overwrite_qualified_to_none() {
        let mut table = DataConTable::new();
        table.insert(make_datacon_qualified(1, "Foo", 1, 0, "Mod.Foo"));
        assert_eq!(table.get_by_qualified_name("Mod.Foo"), Some(DataConId(1)));

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

    // ---- insert_checked: loud varId-collision detection ----

    /// Two DISTINCT constructors hashing to one id must be rejected loudly,
    /// naming both module-qualified — instead of the silent overwrite `insert`
    /// performs (which evicted freer-simple's Union).
    #[test]
    fn insert_checked_rejects_true_collision() {
        let mut table = DataConTable::new();
        table
            .insert_checked(make_datacon_qualified(
                42,
                "Union",
                1,
                1,
                "Data.OpenUnion.Internal.Union",
            ))
            .expect("first insert is clean");
        let err = table
            .insert_checked(make_datacon_qualified(
                42,
                "DynFlags",
                1,
                1,
                "GHC.Driver.Session.DynFlags",
            ))
            .expect_err("distinct constructor at same id must collide");
        match &err {
            DataConCollision::Id { id, first, second } => {
                assert_eq!(*id, DataConId(42));
                assert_eq!(first, "Data.OpenUnion.Internal.Union");
                assert_eq!(second, "GHC.Driver.Session.DynFlags");
            }
            other => panic!("expected DataConCollision::Id, got {other:?}"),
        }
        // The survivor is unchanged — the collision did not overwrite it.
        assert_eq!(
            table.get_by_qualified_name("Data.OpenUnion.Internal.Union"),
            Some(DataConId(42))
        );
        assert_eq!(
            table.get_by_qualified_name("GHC.Driver.Session.DynFlags"),
            None
        );
        // Error message names both constructors and the id.
        let msg = err.to_string();
        assert!(msg.contains("Union"), "msg: {msg}");
        assert!(msg.contains("DynFlags"), "msg: {msg}");
        assert!(msg.contains("0x000000000000002a"), "msg: {msg}");
    }

    /// The SAME constructor seen twice (same module-qualified identity, e.g.
    /// from both the wired-in list and a tycon scan) is a legitimate
    /// re-encounter and must stay silent (idempotent).
    #[test]
    fn insert_checked_allows_same_constructor_reencounter() {
        let mut table = DataConTable::new();
        let dc = make_datacon_qualified(7, "Just", 2, 1, "GHC.Maybe.Just");
        table.insert_checked(dc.clone()).expect("first insert");
        table
            .insert_checked(dc)
            .expect("identical re-encounter is not a collision");
        assert_eq!(table.len(), 1);
        assert_eq!(
            table.get_by_qualified_name("GHC.Maybe.Just"),
            Some(DataConId(7))
        );
    }

    /// An agreeing qualified name but DISAGREEING tag/rep_arity must still be
    /// rejected — the old last-wins behavior (plain `insert`) would silently
    /// keep whichever shape arrived last, and a downstream consumer that
    /// resolved the id earlier would then disagree with the table about the
    /// constructor's actual tag/arity.
    #[test]
    fn insert_checked_rejects_same_identity_disagreeing_tag_arity() {
        let mut table = DataConTable::new();
        table
            .insert_checked(make_datacon_qualified(5, "Foo", 1, 2, "Mod.Foo"))
            .expect("first insert is clean");
        let err = table
            .insert_checked(make_datacon_qualified(5, "Foo", 2, 3, "Mod.Foo"))
            .expect_err("agreeing identity but disagreeing tag/arity must collide");
        match &err {
            DataConCollision::Id { id, first, second } => {
                assert_eq!(*id, DataConId(5));
                assert!(first.contains("tag=1"), "first: {first}");
                assert!(first.contains("rep_arity=2"), "first: {first}");
                assert!(second.contains("tag=2"), "second: {second}");
                assert!(second.contains("rep_arity=3"), "second: {second}");
            }
            other => panic!("expected DataConCollision::Id, got {other:?}"),
        }
        // The survivor (first insert) is unchanged.
        assert_eq!(table.get(DataConId(5)).unwrap().tag, 1);
        assert_eq!(table.get(DataConId(5)).unwrap().rep_arity, 2);
    }

    /// Without qualified names, identity falls back to the unqualified name:
    /// same name = silent, different name = collision.
    #[test]
    fn insert_checked_falls_back_to_unqualified_name() {
        let mut table = DataConTable::new();
        table.insert_checked(make_datacon(9, "Same", 1, 0)).unwrap();
        table
            .insert_checked(make_datacon(9, "Same", 1, 0))
            .expect("same unqualified name is a re-encounter");
        let err = table
            .insert_checked(make_datacon(9, "Different", 1, 0))
            .expect_err("different unqualified name at same id collides");
        match &err {
            DataConCollision::Id { first, second, .. } => {
                assert_eq!(first, "Same");
                assert_eq!(second, "Different");
            }
            other => panic!("expected DataConCollision::Id, got {other:?}"),
        }
    }

    // ---- insert_checked / extend_checked: by_qualified_name collision guard ----

    /// Two DISTINCT ids claiming one qualified name must be rejected loudly,
    /// naming both ids — the mirror-image of the by-id guard above.
    #[test]
    fn insert_checked_rejects_distinct_ids_sharing_a_qualified_name() {
        let mut table = DataConTable::new();
        table
            .insert_checked(make_datacon_qualified(
                10,
                "Val",
                1,
                1,
                "Control.Monad.Freer.Val",
            ))
            .expect("first insert is clean");
        let err = table
            .insert_checked(make_datacon_qualified(
                910,
                "Val",
                1,
                1,
                "Control.Monad.Freer.Val",
            ))
            .expect_err("a second, distinct id claiming the same qualified name must collide");
        match &err {
            DataConCollision::QualifiedName {
                qualified_name,
                first_id,
                second_id,
                ..
            } => {
                assert_eq!(qualified_name, "Control.Monad.Freer.Val");
                assert_eq!(*first_id, DataConId(10));
                assert_eq!(*second_id, DataConId(910));
            }
            other => panic!("expected DataConCollision::QualifiedName, got {other:?}"),
        }
        // The survivor is unchanged — the collision did not overwrite it.
        assert_eq!(
            table.get_by_qualified_name("Control.Monad.Freer.Val"),
            Some(DataConId(10))
        );
        assert_eq!(table.get(DataConId(910)), None);
        // The error message names both colliding ids.
        let msg = err.to_string();
        assert!(msg.contains("0x000000000000000a"), "msg: {msg}");
        assert!(msg.contains("0x000000000000038e"), "msg: {msg}");
        assert!(msg.contains("Control.Monad.Freer.Val"), "msg: {msg}");
    }

    /// The SAME id re-presenting the qualified name it already owns is not a
    /// collision — the guard only fires when a DIFFERENT id claims it.
    #[test]
    fn insert_checked_allows_same_id_reclaiming_its_own_qualified_name() {
        let mut table = DataConTable::new();
        let dc = make_datacon_qualified(7, "Just", 2, 1, "GHC.Maybe.Just");
        table.insert_checked(dc.clone()).expect("first insert");
        table
            .insert_checked(dc)
            .expect("the same id re-claiming its own qualified name is not a collision");
        assert_eq!(table.len(), 1);
    }

    /// `extend_checked` must route through the SAME qualified-name guard as
    /// `insert_checked` — not a separately hand-maintained copy that could
    /// drift. Two distinct ids sharing a qualified name, fed through the
    /// batch API, must collide identically.
    #[test]
    fn extend_checked_rejects_distinct_ids_sharing_a_qualified_name() {
        let mut table = DataConTable::new();
        let first = make_datacon_qualified(1, "A", 1, 0, "Shared.Qualified.Name");
        let second = make_datacon_qualified(2, "B", 1, 0, "Shared.Qualified.Name");
        let err = table
            .extend_checked([first, second])
            .expect_err("extend_checked must reject the same qualified-name collision");
        match &err {
            DataConCollision::QualifiedName {
                first_id,
                second_id,
                ..
            } => {
                assert_eq!(*first_id, DataConId(1));
                assert_eq!(*second_id, DataConId(2));
            }
            other => panic!("expected DataConCollision::QualifiedName, got {other:?}"),
        }
        // Only the first (clean) entry landed.
        assert_eq!(table.len(), 1);
        assert_eq!(
            table.get_by_qualified_name("Shared.Qualified.Name"),
            Some(DataConId(1))
        );
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
    fn test_constructors_of_type_declaration_order() {
        let mut table = DataConTable::new();
        table.insert(make_datacon_typed(1, "GO", 1, 0, "Verdict"));
        table.insert(make_datacon_typed(2, "PARTIAL", 2, 0, "Verdict"));
        table.insert(make_datacon_typed(3, "NOGO", 3, 0, "Verdict"));
        // Unrelated type must not pollute the lookup.
        table.insert(make_datacon_typed(4, "Just", 1, 1, "Maybe"));

        assert_eq!(
            table.constructors_of_type("Verdict"),
            vec![DataConId(1), DataConId(2), DataConId(3)]
        );
        assert_eq!(table.constructors_of_type("Maybe"), vec![DataConId(4)]);
        assert_eq!(table.constructors_of_type("NoSuchType"), Vec::new());
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
        assert_eq!(table.get_by_name_arity("Tip", 0), Some(DataConId(200)));
    }

    // ---- get_by_name_arity_checked: loud ambiguity, no insertion-order tie-break ----

    /// Two distinct constructors sharing name AND arity (e.g. `Bin` from
    /// `Data.Map` vs `Data.Set`, both binary) must be a loud error naming
    /// both candidates — not a silent last-inserted pick.
    #[test]
    fn get_by_name_arity_checked_rejects_true_ambiguity() {
        let mut table = DataConTable::new();
        table.insert(make_datacon_qualified(
            100,
            "Bin",
            1,
            2,
            "Data.Map.Internal.Bin",
        ));
        table.insert(make_datacon_qualified(
            200,
            "Bin",
            1,
            2,
            "Data.Set.Internal.Bin",
        ));

        let err = table
            .get_by_name_arity_checked("Bin", 2)
            .expect_err("two distinct Bin/2 constructors must be ambiguous");
        assert_eq!(err.name, "Bin");
        assert_eq!(err.arity, 2);
        assert_eq!(err.candidates.len(), 2);
        assert!(err
            .candidates
            .contains(&"Data.Map.Internal.Bin".to_string()));
        assert!(err
            .candidates
            .contains(&"Data.Set.Internal.Bin".to_string()));
        let msg = err.to_string();
        assert!(msg.contains("Bin"), "msg: {msg}");
        assert!(msg.contains("Data.Map.Internal.Bin"), "msg: {msg}");
        assert!(msg.contains("Data.Set.Internal.Bin"), "msg: {msg}");
    }

    /// A requested arity that no same-named constructor carries is a plain
    /// "not found" — `Ok(None)`, never a silent fallback to a wrong-arity
    /// entry (that fallback lived in `tidepool-bridge::get_resilient`, not
    /// here, but this method must not reintroduce it).
    #[test]
    fn get_by_name_arity_checked_absent_arity_is_ok_none() {
        let mut table = DataConTable::new();
        table.insert(make_datacon(1, "Just", 2, 1));

        assert_eq!(table.get_by_name_arity_checked("Just", 5), Ok(None));
        assert_eq!(table.get_by_name_arity_checked("Missing", 0), Ok(None));
    }

    /// A single unambiguous name+arity match still resolves cleanly — the
    /// strict path must not regress the common, non-colliding case.
    #[test]
    fn get_by_name_arity_checked_resolves_unique_match() {
        let mut table = DataConTable::new();
        table.insert(make_datacon(1, "Just", 2, 1));
        table.insert(make_datacon(2, "Nothing", 1, 0));

        assert_eq!(
            table.get_by_name_arity_checked("Just", 1),
            Ok(Some(DataConId(1)))
        );
    }

    /// Same name, DIFFERENT arities is not ambiguous — arity alone
    /// disambiguates, each resolves to its own unique id.
    #[test]
    fn get_by_name_arity_checked_different_arities_not_ambiguous() {
        let mut table = DataConTable::new();
        table.insert(make_datacon(1, "Read", 1, 1));
        table.insert(make_datacon(2, "Read", 1, 2));

        assert_eq!(
            table.get_by_name_arity_checked("Read", 1),
            Ok(Some(DataConId(1)))
        );
        assert_eq!(
            table.get_by_name_arity_checked("Read", 2),
            Ok(Some(DataConId(2)))
        );
    }
}
