//! Incremental indexes over the session's [`BindingTable`] value plane.
//!
//! `tidepool_codegen::binding_table::BindingTable` is a flat, globally-keyed
//! store; it answers "is this id live" in O(1) but has no cheap answer to
//! "which prepared bindings does this import identity retain", "what is the
//! live `Val.G<g>` module set", or "is this root slot still aliased by
//! another live binding" -- those were previously answered by scanning every
//! live binding on EVERY turn ([`super::prepared`]'s `resolve_prepared_import`,
//! and [`super::persistent`]'s `prepared_retained`, `live_val_modules`, and
//! `release_binding_roots`).
//!
//! [`super::persistent::PersistentSession`] is the only place in this crate
//! that ever inserts into or evicts from `live` (`bind`, `bind_in`,
//! `bind_alias_in`, and the loop in `bind_replacing_decls_in` insert; every
//! eviction path -- `save_observation`, `collect_observations`,
//! `release_leases`, `drain_scope` -- is funneled through
//! `release_binding_roots`, including the one call site outside this file,
//! `resident.rs`'s `settle_dropped_custody`, which reaches it only through
//! `PersistentSession::release_binding_roots`). That makes it the only place
//! that can keep these indexes in exact sync with the table: every mutating
//! call is paired with [`BindingIndex::on_bind`] or [`BindingIndex::on_evict`]
//! right alongside the underlying `BindingTable` call, so a turn's cost here
//! is O(this turn's bindings), not O(session length).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use tidepool_codegen::binding_table::{BindingEntry, BoundValue};
use tidepool_codegen::old_space::RootSlot;
use tidepool_repr::execution_schema::SymbolIdentity;
use tidepool_repr::{SessionModule, SessionVarId};

/// A minimal, index-relevant view of one binding, taken from a `BindingEntry`
/// at the moment it enters or leaves `live`. Deliberately decoupled from
/// `BoundValue` (whose `Prepared` variant carries a `PreparedHandle` this
/// crate has no public constructor for) so index maintenance -- and its unit
/// tests -- never need a real prepared handle, only the identity, generation,
/// and root address the index actually keys on.
pub(super) struct BindRecord {
    id: SessionVarId,
    module: SessionModule,
    root: RootSlot,
    identity: SymbolIdentity,
}

impl BindRecord {
    pub(super) fn of(entry: &BindingEntry) -> Self {
        let BoundValue { identity, .. } = &entry.value;
        BindRecord {
            id: entry.id,
            module: entry.module,
            root: entry.value.root,
            identity: identity.clone(),
        }
    }
}

/// One live prepared-binding candidate for an import identity: its local
/// binding generation and the id to fetch the full entry by.
#[derive(Clone, Copy)]
struct PreparedCandidate {
    generation: u64,
    id: SessionVarId,
}

/// Per-session indexes over the value plane, maintained incrementally by
/// [`super::persistent::PersistentSession`] alongside every bind and
/// eviction. Never constructed or mutated anywhere else.
#[derive(Default)]
pub(crate) struct BindingIndex {
    /// Live `Val.G<g>` module names -- `PersistentSession::live_val_modules`.
    /// A `BTreeSet` because generations are minted monotonically per bind, so
    /// module names are already unique per live entry; this gives the sorted,
    /// deduplicated output the old per-turn sort+dedup produced, for free.
    live_modules: BTreeSet<String>,
    /// `(import identity, local generation)` pairs for every live prepared
    /// binding with a recorded identity -- `PersistentSession::prepared_retained`.
    prepared_retained: BTreeSet<(SymbolIdentity, u64)>,
    /// Import identity -> live prepared candidates, for
    /// `resolve_prepared_import`'s newest/exact-generation lookup. Order
    /// within a `Vec` is not meaningful (only `max_by_key` over it is read).
    prepared_by_identity: BTreeMap<SymbolIdentity, Vec<PreparedCandidate>>,
    /// How many live entries currently resolve to each root-slot address --
    /// the aliasing refcount `release_binding_roots` used to recompute by
    /// scanning every live binding per evicted entry. Keyed by the address
    /// as `usize` (never dereferenced here, only compared/hashed) rather
    /// than the raw `*mut *mut u8` so `BindingIndex` -- unlike `RootSlot`
    /// and `BindingTable` -- needs no `unsafe impl Send` of its own: a
    /// `PersistentSession` holding one stays auto-`Send` exactly because
    /// this index carries no live pointer.
    root_refs: HashMap<usize, usize>,
}

impl BindingIndex {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Record a binding that just entered `live` (a fresh `bind`, `bind_in`,
    /// `bind_alias_in`, or `bind_replacing_decls_in` entry). Must be called
    /// exactly once per entry that enters `live`, with that same entry later
    /// passed to [`Self::on_evict`] exactly once when it leaves.
    pub(super) fn on_bind(&mut self, entry: &BindingEntry) {
        self.on_bind_record(&BindRecord::of(entry));
    }

    /// Record a binding that just left `live` (one entry
    /// [`super::persistent::PersistentSession::release_binding_roots`] is
    /// processing). Returns whether the underlying root slot is now
    /// unreferenced by any other still-live entry, replacing the old
    /// `bindings.iter_live().any(..)` alias scan (and its `released.contains`
    /// intra-batch dedup: two evicted entries sharing one root each decrement
    /// this refcount, so the LAST one to be processed is the one that
    /// observes zero and actually releases).
    pub(super) fn on_evict(&mut self, entry: &BindingEntry) -> bool {
        self.on_evict_record(&BindRecord::of(entry))
    }

    /// [`Self::on_bind`] for a caller that must build the [`BindRecord`]
    /// before consuming/moving the `BindingEntry` it came from (e.g. an
    /// alias bind whose `BindingTable` call takes the entry by value and can
    /// fail, in which case indexing must not happen at all).
    pub(super) fn on_bind_record(&mut self, record: &BindRecord) {
        self.live_modules.insert(record.module.module_name());
        *self
            .root_refs
            .entry(record.root.addr() as usize)
            .or_insert(0) += 1;
        let generation = record.module.gen().0;
        self.prepared_retained
            .insert((record.identity.clone(), generation));
        self.prepared_by_identity
            .entry(record.identity.clone())
            .or_default()
            .push(PreparedCandidate {
                generation,
                id: record.id,
            });
    }

    /// [`Self::on_evict`] for a pre-built [`BindRecord`] (see
    /// [`Self::on_bind_record`]'s rationale).
    pub(super) fn on_evict_record(&mut self, record: &BindRecord) -> bool {
        self.live_modules.remove(&record.module.module_name());
        self.prepared_retained
            .remove(&(record.identity.clone(), record.module.gen().0));
        if let Some(candidates) = self.prepared_by_identity.get_mut(&record.identity) {
            if let Some(pos) = candidates.iter().position(|c| c.id == record.id) {
                candidates.swap_remove(pos);
            }
            if candidates.is_empty() {
                self.prepared_by_identity.remove(&record.identity);
            }
        }
        let addr = record.root.addr() as usize;
        match self.root_refs.get_mut(&addr) {
            Some(count) => {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.root_refs.remove(&addr);
                    true
                } else {
                    false
                }
            }
            // Unknown to the index: never observed as bound, so nothing else
            // can be holding it either -- safe to release.
            None => true,
        }
    }

    /// Sorted, deduplicated live `Val.G<g>` module names.
    pub(super) fn live_modules(&self) -> Vec<String> {
        self.live_modules.iter().cloned().collect()
    }

    /// Sorted, deduplicated `(identity, generation)` pairs for every live
    /// prepared binding with a recorded identity.
    pub(super) fn prepared_retained(&self) -> Vec<(SymbolIdentity, u64)> {
        self.prepared_retained.iter().cloned().collect()
    }

    /// The id of the newest live prepared binding whose recorded import
    /// identity is `identity` (optionally pinned to an exact generation),
    /// mirroring `iter_live().filter(..).max_by_key(|e| e.module.gen())`.
    /// Generations are unique per live entry (minted monotonically per
    /// bind), so no tie-break policy is observable.
    pub(super) fn resolve_prepared(
        &self,
        identity: &SymbolIdentity,
        generation: Option<u64>,
    ) -> Option<SessionVarId> {
        let candidates = self.prepared_by_identity.get(identity)?;
        candidates
            .iter()
            .filter(|c| generation.is_none_or(|g| c.generation == g))
            .max_by_key(|c| c.generation)
            .map(|c| c.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::Generation;

    fn fake_slot(boxed: &mut *mut u8) -> RootSlot {
        // SAFETY: test-only; never dereferenced.
        unsafe { RootSlot::new(boxed as *mut *mut u8) }
    }

    fn identity(name: &str) -> SymbolIdentity {
        SymbolIdentity {
            unit: "unit".into(),
            module: "M".into(),
            namespace: "ns".into(),
            occurrence: name.into(),
            record_parent: None,
        }
    }

    fn record(gen: u64, raw: u64, root: RootSlot, identity: &SymbolIdentity) -> BindRecord {
        BindRecord {
            id: SessionVarId::from_extract(raw),
            module: SessionModule::val(Generation(gen)),
            root,
            identity: identity.clone(),
        }
    }

    /// A brute-force recomputation of every index from a slice of currently
    /// live records -- the ground truth incremental maintenance is checked
    /// against, mirroring the pre-fix scans exactly (`iter_live().filter...`,
    /// sort+dedup, `max_by_key`).
    struct BruteForce<'a> {
        live: &'a [BindRecord],
    }

    impl BruteForce<'_> {
        fn live_modules(&self) -> Vec<String> {
            let mut v: Vec<String> = self.live.iter().map(|r| r.module.module_name()).collect();
            v.sort();
            v.dedup();
            v
        }

        fn prepared_retained(&self) -> Vec<(SymbolIdentity, u64)> {
            let mut v: Vec<(SymbolIdentity, u64)> = self
                .live
                .iter()
                .map(|r| (r.identity.clone(), r.module.gen().0))
                .collect();
            v.sort();
            v.dedup();
            v
        }

        fn resolve_prepared(
            &self,
            identity: &SymbolIdentity,
            generation: Option<u64>,
        ) -> Option<SessionVarId> {
            self.live
                .iter()
                .filter(|r| &r.identity == identity)
                .filter(|r| generation.is_none_or(|g| r.module.gen().0 == g))
                .max_by_key(|r| r.module.gen())
                .map(|r| r.id)
        }
    }

    #[test]
    fn bind_shadow_evict_matches_brute_force_recomputation() {
        let mut pointer_a: *mut u8 = std::ptr::null_mut();
        let mut pointer_b: *mut u8 = std::ptr::null_mut();
        let slot_a = fake_slot(&mut pointer_a);
        let slot_b = fake_slot(&mut pointer_b);

        let id_x = identity("x");
        let id_y = identity("y");

        let mut index = BindingIndex::new();
        let mut live: Vec<BindRecord> = Vec::new();

        // Bind: two distinct identities, distinct generations, distinct
        // roots.
        let r1 = record(1, 1, slot_a, &id_x);
        index.on_bind_record(&r1);
        live.push(r1);

        let r2 = record(2, 2, slot_b, &id_y);
        index.on_bind_record(&r2);
        live.push(r2);

        let brute = BruteForce { live: &live };
        assert_eq!(index.live_modules(), brute.live_modules());
        assert_eq!(index.prepared_retained(), brute.prepared_retained());
        assert_eq!(
            index.resolve_prepared(&id_x, None),
            brute.resolve_prepared(&id_x, None)
        );
        assert_eq!(
            index.resolve_prepared(&id_y, None),
            brute.resolve_prepared(&id_y, None)
        );

        // Shadow: rebind `x` at a newer generation under the SAME identity,
        // sharing the SAME root slot (as an aliasing rebind would); the old
        // entry stays live (as `BindingTable::bind_in` leaves a shadowed
        // gen), so both remain in the index simultaneously.
        let r3 = record(3, 3, slot_a, &id_x);
        index.on_bind_record(&r3);
        live.push(r3);

        let brute = BruteForce { live: &live };
        assert_eq!(index.live_modules(), brute.live_modules());
        assert_eq!(index.prepared_retained(), brute.prepared_retained());
        assert_eq!(
            index.resolve_prepared(&id_x, None),
            brute.resolve_prepared(&id_x, None)
        );
        assert_eq!(
            index.resolve_prepared(&id_x, Some(1)),
            brute.resolve_prepared(&id_x, Some(1))
        );
        assert_eq!(index.resolve_prepared(&id_x, None).unwrap().raw(), 3);

        // Evict the shadowed (older) `x` record; the newer one must remain
        // resolvable, and the aliasing refcount for its shared root
        // (slot_a, shared between r1 and r3) must reflect that a live
        // binding (r3) still references it.
        let evicted = live.remove(0); // r1
        let safe = index.on_evict_record(&evicted);
        assert!(
            !safe,
            "slot_a is still referenced by r3 (same root, later gen)"
        );

        let brute = BruteForce { live: &live };
        assert_eq!(index.live_modules(), brute.live_modules());
        assert_eq!(index.prepared_retained(), brute.prepared_retained());
        assert_eq!(
            index.resolve_prepared(&id_x, None),
            brute.resolve_prepared(&id_x, None)
        );
        assert_eq!(index.resolve_prepared(&id_x, Some(1)), None);

        // Evict everything else; slot_b (r2, unshared) must report safe to
        // release, and slot_a's last live reference (r3) must too.
        let evicted = live.remove(0); // r2 (index 0 after the first removal)
        let safe = index.on_evict_record(&evicted);
        assert!(safe, "slot_b had no other live reference");

        let evicted = live.remove(0); // r3
        let safe = index.on_evict_record(&evicted);
        assert!(safe, "r3 was slot_a's last live reference");

        let brute = BruteForce { live: &live };
        assert!(live.is_empty());
        assert_eq!(index.live_modules(), brute.live_modules());
        assert!(index.live_modules().is_empty());
        assert_eq!(index.prepared_retained(), brute.prepared_retained());
        assert!(index.prepared_retained().is_empty());
        assert_eq!(index.resolve_prepared(&id_x, None), None);
        assert_eq!(index.resolve_prepared(&id_y, None), None);
    }
}
