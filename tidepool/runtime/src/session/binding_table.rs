//! Incremental indexes over the session's [`BindingTable`] persistent binding store.
//!
//! `tidepool_codegen::binding_table::BindingTable` is a flat, globally-keyed
//! store; it answers "is this id live" in O(1) but has no cheap answer to
//! "which prepared bindings does this import identity retain", "what is the
//! live `Val.G<g>` module set", or "is this handle still aliased by
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

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use tidepool_codegen::binding_table::{BindingEntry, BoundValue};
use tidepool_codegen::suspension::ValueHandle;
use tidepool_repr::execution_schema::SymbolIdentity;
use tidepool_repr::{SessionModule, SessionVarId};

/// Immutable index facts captured before the entry is moved into the store.
/// Exact typed handle identity determines alias custody; the machine alone
/// owns and resolves physical root cells.
pub(super) struct BindRecord {
    id: SessionVarId,
    module: SessionModule,
    handle: ValueHandle,
    identity: SymbolIdentity,
}

impl BindRecord {
    pub(super) fn of(entry: &BindingEntry) -> Self {
        let BoundValue { identity, .. } = &entry.value;
        BindRecord {
            id: entry.id,
            module: entry.module,
            handle: entry.value.handle.raw(),
            identity: identity.clone(),
        }
    }
}

/// Per-session indexes over the persistent binding store, maintained incrementally by
/// [`super::persistent::PersistentSession`] alongside every bind and
/// eviction. Never constructed or mutated anywhere else.
#[derive(Default)]
pub(crate) struct BindingIndex {
    /// Live `Val.G<g>` module names -- `PersistentSession::live_val_modules`.
    /// One generated interface can publish several bindings, so this is a refcount rather
    /// than a set. Removing one name must not make the shared interface vanish
    /// from later compiler injection while another name still imports it.
    live_modules: BTreeMap<String, usize>,
    /// Compiler-owned thin interfaces share the exact live binding lifetime.
    /// They carry type evidence only; native imports still resolve live roots.
    value_interfaces: BTreeMap<String, RetainedValueInterface>,
    /// Import identity -> generation -> live binding ids. Generation membership
    /// is the single owner of both retained-pair enumeration and exact/latest
    /// resolution. Distinct live ids can share one identity/generation pair;
    /// evicting one must preserve it until the last id leaves.
    /// Order within an id vector has no semantic meaning.
    prepared_by_identity: BTreeMap<SymbolIdentity, BTreeMap<u64, Vec<SessionVarId>>>,
    /// Number of bindings sharing each exact retained handle. A fresh handle
    /// gets a process-unique identity even when an allocator reuses its cell.
    handle_refs: HashMap<ValueHandle, usize>,
}

enum RetainedValueInterface {
    LegacyDisk,
    Certified(Arc<tidepool_toolchain::checked_cell::CheckedValueArtifact>),
    #[cfg(test)]
    Fixture(Arc<[u8]>),
}
impl RetainedValueInterface {
    fn bytes(&self) -> Option<&Arc<[u8]>> {
        match self {
            Self::LegacyDisk => None,
            Self::Certified(interface) => Some(interface.bytes_owned()),
            #[cfg(test)]
            Self::Fixture(bytes) => Some(bytes),
        }
    }
}

impl BindingIndex {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn retain_value_interface(
        &mut self,
        interface: Arc<tidepool_toolchain::checked_cell::CheckedValueArtifact>,
    ) -> bool {
        if !self.accepts_value_interface(&interface) {
            return false;
        }
        let name = interface.owner().module_name();
        if self.value_interfaces.contains_key(&name) {
            return true;
        }
        // A failed display may settle type evidence without retaining a value.
        if self.is_module_live(&name) {
            self.value_interfaces
                .insert(name, RetainedValueInterface::Certified(interface));
        }
        true
    }

    /// The binding transaction validated this original certificate before its
    /// native write. Retention is an in-memory part of that same commit.
    pub(super) fn commit_value_interface(
        &mut self,
        interface: Arc<tidepool_toolchain::checked_cell::CheckedValueArtifact>,
    ) {
        let name = interface.owner().module_name();
        debug_assert!(self.is_module_live(&name));
        debug_assert!(self.accepts_value_interface(&interface));
        self.value_interfaces
            .insert(name, RetainedValueInterface::Certified(interface));
    }

    pub(super) fn accepts_value_interface(
        &self,
        interface: &Arc<tidepool_toolchain::checked_cell::CheckedValueArtifact>,
    ) -> bool {
        self.value_interfaces
            .get(&interface.owner().module_name())
            .is_none_or(|existing| {
                matches!(existing, RetainedValueInterface::Certified(old)
                if Arc::ptr_eq(old, interface) || old == interface)
            })
    }

    pub(super) fn value_interface(&self, module: SessionModule) -> Option<&Arc<[u8]>> {
        self.value_interfaces
            .get(&module.module_name())
            .and_then(RetainedValueInterface::bytes)
    }

    pub(super) fn checked_value_artifact(
        &self,
        module: SessionModule,
    ) -> Option<&Arc<tidepool_toolchain::checked_cell::CheckedValueArtifact>> {
        match self.value_interfaces.get(&module.module_name())? {
            RetainedValueInterface::Certified(artifact) => Some(artifact),
            _ => None,
        }
    }

    pub(super) fn mark_legacy_interface(&mut self, module: SessionModule) {
        let name = module.module_name();
        if self.is_module_live(&name) {
            self.value_interfaces
                .entry(name)
                .or_insert(RetainedValueInterface::LegacyDisk);
        }
    }

    #[cfg(test)]
    pub(super) fn retain_fixture_interface(&mut self, module: SessionModule, bytes: Arc<[u8]>) {
        assert!(self.is_module_live(&module.module_name()));
        self.value_interfaces
            .insert(module.module_name(), RetainedValueInterface::Fixture(bytes));
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
    /// processing). Returns whether the underlying handle is now
    /// unreferenced by any other still-live entry, replacing the old
    /// `bindings.iter_live().any(..)` alias scan (and its `released.contains`
    /// intra-batch dedup: two evicted entries sharing one handle each decrement
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
        *self
            .live_modules
            .entry(record.module.module_name())
            .or_insert(0) += 1;
        *self.handle_refs.entry(record.handle).or_insert(0) += 1;
        let generation = record.module.gen().0;
        self.prepared_by_identity
            .entry(record.identity.clone())
            .or_default()
            .entry(generation)
            .or_default()
            .push(record.id);
    }

    /// [`Self::on_evict`] for a pre-built [`BindRecord`] (see
    /// [`Self::on_bind_record`]'s rationale).
    pub(super) fn on_evict_record(&mut self, record: &BindRecord) -> bool {
        let module = record.module.module_name();
        if let Some(count) = self.live_modules.get_mut(&module) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.live_modules.remove(&module);
                self.value_interfaces.remove(&module);
            }
        }
        if let Some(generations) = self.prepared_by_identity.get_mut(&record.identity) {
            let generation = record.module.gen().0;
            if let Some(ids) = generations.get_mut(&generation) {
                if let Some(pos) = ids.iter().position(|id| *id == record.id) {
                    ids.swap_remove(pos);
                }
                if ids.is_empty() {
                    generations.remove(&generation);
                }
            }
            if generations.is_empty() {
                self.prepared_by_identity.remove(&record.identity);
            }
        }
        match self.handle_refs.get_mut(&record.handle) {
            Some(count) => {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.handle_refs.remove(&record.handle);
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
        self.live_modules.keys().cloned().collect()
    }

    /// Whether any live binding still resolves to `module_name` — the exact
    /// refcount `on_evict_record` maintains, read without mutating it. Used
    /// to tell whether a just-evicted stub generation's module has become
    /// fully unreferenced (see `PersistentSession::release_binding_roots`).
    pub(super) fn is_module_live(&self, module_name: &str) -> bool {
        self.live_modules.contains_key(module_name)
    }

    /// Sorted, deduplicated `(identity, generation)` pairs for every live
    /// prepared binding with a recorded identity.
    pub(super) fn prepared_retained(&self) -> Vec<(SymbolIdentity, u64)> {
        self.prepared_by_identity
            .iter()
            .flat_map(|(identity, generations)| {
                generations
                    .keys()
                    .map(move |generation| (identity.clone(), *generation))
            })
            .collect()
    }

    /// One live prepared binding at the newest eligible generation for `identity`,
    /// optionally pinned to an exact generation. Multiple immutable ids may
    /// share an identity/generation pair; their lookup order is unspecified,
    /// as it was in the underlying `BindingTable::iter_live` HashMap scan.
    pub(super) fn resolve_prepared(
        &self,
        identity: &SymbolIdentity,
        generation: Option<u64>,
    ) -> Option<SessionVarId> {
        let generations = self.prepared_by_identity.get(identity)?;
        let ids = match generation {
            Some(generation) => generations.get(&generation)?,
            None => generations.last_key_value()?.1,
        };
        ids.last().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::Generation;

    fn identity(name: &str) -> SymbolIdentity {
        SymbolIdentity {
            unit: "unit".into(),
            module: "M".into(),
            namespace: "ns".into(),
            occurrence: name.into(),
            record_parent: None,
        }
    }

    fn record(gen: u64, raw: u64, handle: ValueHandle, identity: &SymbolIdentity) -> BindRecord {
        BindRecord {
            id: SessionVarId::from_extract(raw),
            module: SessionModule::val(Generation(gen)),
            handle,
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
        let handle_a = ValueHandle(1);
        let handle_b = ValueHandle(2);

        let id_x = identity("x");
        let id_y = identity("y");

        let mut index = BindingIndex::new();
        let mut live: Vec<BindRecord> = Vec::new();

        // Bind: two distinct identities, distinct generations, distinct
        // roots.
        let r1 = record(1, 1, handle_a, &id_x);
        index.on_bind_record(&r1);
        live.push(r1);

        let r2 = record(2, 2, handle_b, &id_y);
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
        // sharing the SAME handle (as an aliasing rebind would); the old
        // entry stays live (as `BindingTable::bind_in` leaves a shadowed
        // gen), so both remain in the index simultaneously.
        let r3 = record(3, 3, handle_a, &id_x);
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
        // resolvable, and the aliasing refcount for its shared handle
        // (handle_a, shared between r1 and r3) must reflect that a live
        // binding (r3) still references it.
        let evicted = live.remove(0); // r1
        let safe = index.on_evict_record(&evicted);
        assert!(
            !safe,
            "handle_a is still referenced by r3 (same handle, later gen)"
        );

        let brute = BruteForce { live: &live };
        assert_eq!(index.live_modules(), brute.live_modules());
        assert_eq!(index.prepared_retained(), brute.prepared_retained());
        assert_eq!(
            index.resolve_prepared(&id_x, None),
            brute.resolve_prepared(&id_x, None)
        );
        assert_eq!(index.resolve_prepared(&id_x, Some(1)), None);

        // Evict everything else; handle_b (r2, unshared) must report safe to
        // release, and handle_a's last live reference (r3) must too.
        let evicted = live.remove(0); // r2 (index 0 after the first removal)
        let safe = index.on_evict_record(&evicted);
        assert!(safe, "handle_b had no other live reference");

        let evicted = live.remove(0); // r3
        let safe = index.on_evict_record(&evicted);
        assert!(safe, "r3 was handle_a's last live reference");

        let brute = BruteForce { live: &live };
        assert!(live.is_empty());
        assert_eq!(index.live_modules(), brute.live_modules());
        assert!(index.live_modules().is_empty());
        assert_eq!(index.prepared_retained(), brute.prepared_retained());
        assert!(index.prepared_retained().is_empty());
        assert_eq!(index.resolve_prepared(&id_x, None), None);
        assert_eq!(index.resolve_prepared(&id_y, None), None);
    }

    #[test]
    fn evicting_one_binding_keeps_its_shared_value_module_live() {
        let module = SessionModule::val(Generation(20));
        let page = BindRecord {
            id: SessionVarId::from_extract(1),
            module,
            handle: ValueHandle(1),
            identity: identity("retained"),
        };
        let alias = BindRecord {
            id: SessionVarId::from_extract(2),
            module,
            handle: ValueHandle(2),
            identity: identity("alias"),
        };
        let mut index = BindingIndex::new();
        index.on_bind_record(&page);
        index.on_bind_record(&alias);

        index.on_evict_record(&alias);
        assert_eq!(index.live_modules(), vec![module.module_name()]);

        index.on_evict_record(&page);
        assert!(index.live_modules().is_empty());
    }
    #[test]
    fn evicting_one_of_same_identity_generation_keeps_pair_retained() {
        let handle = ValueHandle(1);
        let name = identity("shared_pair");
        let first = record(7, 1, handle, &name);
        let second = record(7, 2, handle, &name);
        let mut index = BindingIndex::new();
        index.on_bind_record(&first);
        index.on_bind_record(&second);
        let bytes: Arc<[u8]> = Arc::from([9]);
        index.retain_fixture_interface(first.module, bytes.clone());

        assert!(!index.on_evict_record(&first));
        assert_eq!(index.prepared_retained(), vec![(name.clone(), 7)]);
        assert_eq!(index.resolve_prepared(&name, Some(7)), Some(second.id));
        assert_eq!(index.resolve_prepared(&name, None), Some(second.id));
        assert_eq!(index.value_interface(second.module), Some(&bytes));
        assert!(index.on_evict_record(&second));
        assert!(index.prepared_retained().is_empty());
        assert!(index.live_modules().is_empty());
        assert!(index.value_interface(second.module).is_none());
    }

    mod properties {
        use super::*;
        use proptest::prelude::*;
        use proptest::test_runner::{Config, FileFailurePersistence, TestRunner};
        use std::cell::RefCell;

        const IDENTITIES: u8 = 4;
        const ROOTS: usize = 3;

        #[derive(Clone, Copy, Debug)]
        struct Binding {
            id: u64,
            identity: u8,
            generation: u64,
            root: usize,
        }

        #[derive(Clone, Debug)]
        enum Op {
            Bind(Binding),
            Evict(u64),
            Read(u8, Option<u64>),
            Legacy(u64),
            Fixture(u64, u8),
        }

        #[derive(Clone, Debug)]
        enum Choice {
            Bind(u8, u64, usize),
            Shadow(usize, usize),
            Alias(usize, u8, u64),
            Evict(usize),
            Read(u8, Option<u64>),
            Legacy(u64),
            Fixture(usize, u8),
        }

        fn choice() -> impl Strategy<Value = Choice> {
            prop_oneof![
                4 => (0..IDENTITIES, 1u64..6, 0..ROOTS)
                    .prop_map(|(identity, generation, root)| Choice::Bind(identity, generation, root)),
                2 => (0usize..16, 0..ROOTS)
                    .prop_map(|(selector, root)| Choice::Shadow(selector, root)),
                2 => (0usize..16, 0..IDENTITIES, 1u64..6)
                    .prop_map(|(selector, identity, generation)| Choice::Alias(selector, identity, generation)),
                3 => (0usize..16).prop_map(Choice::Evict),
                2 => (0..IDENTITIES, proptest::option::of(0u64..10))
                    .prop_map(|(identity, generation)| Choice::Read(identity, generation)),
                1 => (0u64..8).prop_map(Choice::Legacy),
                1 => (0usize..16, any::<u8>())
                    .prop_map(|(selector, byte)| Choice::Fixture(selector, byte)),
            ]
        }

        /// Shrinking choices rebuilds a complete, valid AST. Every choice runs:
        /// operations needing a live source explicitly insert one if empty.
        /// Distinct ids may share an identity/generation pair. No operation
        /// is filtered, rejected, or silently skipped after shrinking.
        #[derive(Default)]
        struct History {
            ops: Vec<Op>,
            live: Vec<Binding>,
            next_id: u64,
        }

        impl History {
            fn bind(&mut self, identity: u8, generation: u64, root: usize) -> Binding {
                self.next_id += 1;
                let binding = Binding {
                    id: self.next_id,
                    identity,
                    generation,
                    root,
                };
                self.rebind(binding);
                binding
            }

            fn rebind(&mut self, binding: Binding) {
                assert!(self.live.iter().all(|entry| entry.id != binding.id));
                self.live.push(binding);
                self.ops.push(Op::Bind(binding));
            }

            fn source(&mut self, selector: usize) -> Binding {
                if self.live.is_empty() {
                    self.bind(0, 1, 0);
                }
                self.live[selector % self.live.len()]
            }

            fn evict(&mut self, id: u64) {
                let position = self.live.iter().position(|entry| entry.id == id).unwrap();
                self.live.remove(position);
                self.ops.push(Op::Evict(id));
            }

            fn choices(&mut self, choices: Vec<Choice>) {
                for choice in choices {
                    match choice {
                        Choice::Bind(identity, generation, root) => {
                            self.bind(identity, generation, root);
                        }
                        Choice::Shadow(selector, root) => {
                            let source = self.source(selector);
                            self.bind(source.identity, source.generation + 1, root);
                        }
                        Choice::Alias(selector, identity, generation) => {
                            let source = self.source(selector);
                            self.bind(identity, generation, source.root);
                        }
                        Choice::Evict(selector) => {
                            let source = self.source(selector);
                            self.evict(source.id);
                        }
                        Choice::Read(identity, generation) => {
                            self.ops.push(Op::Read(identity, generation));
                        }
                        Choice::Legacy(generation) => self.ops.push(Op::Legacy(generation)),
                        Choice::Fixture(selector, byte) => {
                            let source = self.source(selector);
                            self.ops.push(Op::Fixture(source.generation, byte));
                        }
                    }
                }
            }

            fn guided(&mut self, cohort: u8) {
                let generation = self
                    .live
                    .iter()
                    .map(|entry| entry.generation)
                    .max()
                    .unwrap_or(0)
                    + 1;
                // The reserved root is untouched by arbitrary prefixes, so
                // this cohort always reaches the shared root's final release.
                let original = self.bind(0, generation, ROOTS);
                self.ops.push(Op::Read(0, Some(generation)));
                self.ops.push(Op::Fixture(generation, 11));
                match cohort {
                    0 => {
                        self.bind(0, generation + 1, ROOTS);
                        self.ops.push(Op::Read(0, None));
                        self.evict(original.id);
                        self.ops.push(Op::Read(0, Some(generation)));
                    }
                    1 => {
                        let alias = self.bind(1, generation, ROOTS);
                        self.ops.push(Op::Legacy(generation));
                        self.ops.push(Op::Read(1, Some(generation)));
                        self.evict(original.id);
                        self.ops.push(Op::Read(1, None));
                        self.evict(alias.id);
                        self.ops.push(Op::Read(1, Some(generation)));
                        self.bind(0, generation, ROOTS);
                    }
                    2 => {
                        self.evict(original.id);
                        self.ops.push(Op::Read(0, Some(generation)));
                        // Reinstall the exact immutable id after eviction,
                        // as well as new-id reinsertion in the shared cohort.
                        self.rebind(original);
                        self.ops.push(Op::Legacy(generation));
                        self.ops.push(Op::Read(0, Some(generation)));
                    }
                    3 => {
                        let duplicate = self.bind(0, generation, ROOTS);
                        self.ops.push(Op::Read(0, Some(generation)));
                        self.evict(original.id);
                        self.ops.push(Op::Read(0, Some(generation)));
                        self.evict(duplicate.id);
                        self.ops.push(Op::Read(0, Some(generation)));
                        self.rebind(original);
                    }
                    _ => unreachable!("bounded cohort selector"),
                }
            }

            fn finish(mut self) -> Vec<Op> {
                while let Some(entry) = self.live.last() {
                    self.evict(entry.id);
                }
                self.ops.push(Op::Read(0, None));
                self.ops
            }
        }

        fn arbitrary_history() -> impl Strategy<Value = Vec<Op>> {
            proptest::collection::vec(choice(), 0..49).prop_map(|choices| {
                let mut history = History::default();
                history.choices(choices);
                history.finish()
            })
        }

        fn guided_history() -> impl Strategy<Value = (u8, Vec<Op>)> {
            (
                proptest::collection::vec(choice(), 0..17),
                0u8..4,
                proptest::collection::vec(choice(), 0..17),
            )
                .prop_map(|(prefix, cohort, suffix)| {
                    let mut history = History::default();
                    history.choices(prefix);
                    history.guided(cohort);
                    history.choices(suffix);
                    (cohort, history.finish())
                })
        }

        #[derive(Debug, Default)]
        struct Support {
            binds: usize,
            shadows: usize,
            shared_modules: usize,
            shared_pairs: usize,
            shared_roots: usize,
            reinsertions: usize,
            evictions: usize,
            held_roots: usize,
            released_roots: usize,
            reads: usize,
            fixtures: usize,
            legacy: usize,
        }

        fn check_resolution(
            index: &BindingIndex,
            live: &[BindRecord],
            name: &SymbolIdentity,
            generation: Option<u64>,
        ) {
            // BindingTable::iter_live is a HashMap scan, so equal-generation
            // ties have no ordering contract. The independent oracle accepts
            // any matching live id at the maximal eligible generation.
            let newest = live
                .iter()
                .filter(|entry| &entry.identity == name)
                .filter(|entry| generation.is_none_or(|gen| gen == entry.module.gen().0))
                .map(|entry| entry.module.gen().0)
                .max();
            let eligible: Vec<_> = live
                .iter()
                .filter(|entry| &entry.identity == name && Some(entry.module.gen().0) == newest)
                .map(|entry| entry.id)
                .collect();
            let resolved = index.resolve_prepared(name, generation);
            assert!(match resolved {
                Some(id) => eligible.contains(&id),
                None => eligible.is_empty(),
            }, "resolution {resolved:?} must be one of {eligible:?} for {name:?} at {generation:?}");
        }

        fn check_observables(
            index: &BindingIndex,
            live: &[BindRecord],
            interfaces: &[(u64, Option<Arc<[u8]>>)],
            max_generation: u64,
        ) {
            let brute = BruteForce { live };
            let handles: std::collections::BTreeSet<_> =
                live.iter().map(|entry| entry.handle).collect();
            assert_eq!(index.handle_refs.len(), handles.len());
            for handle in handles {
                let references = live.iter().filter(|entry| entry.handle == handle).count();
                assert_eq!(index.handle_refs.get(&handle), Some(&references));
            }
            assert_eq!(index.live_modules(), brute.live_modules());
            assert_eq!(index.prepared_retained(), brute.prepared_retained());
            for generation in 0..=max_generation + 1 {
                let module = SessionModule::val(Generation(generation));
                let is_live = live.iter().any(|entry| entry.module == module);
                assert_eq!(index.is_module_live(&module.module_name()), is_live);
                let expected = interfaces.iter().find(|(gen, _)| *gen == generation);
                assert_eq!(
                    index.value_interface(module),
                    expected.and_then(|(_, bytes)| bytes.as_ref())
                );
                assert!(index.checked_value_artifact(module).is_none());
                for name in 0..IDENTITIES {
                    let identity = identity(&format!("name_{name}"));
                    check_resolution(index, live, &identity, Some(generation));
                }
            }
            for name in 0..IDENTITIES {
                let identity = identity(&format!("name_{name}"));
                check_resolution(index, live, &identity, None);
            }
        }

        /// Vec records and Vec interface markers are the independent model;
        /// no production refcount or candidate map is copied into the oracle.
        fn replay(ops: &[Op]) -> Support {
            // Bounded groups select aliases. A group's later admission gets
            // a fresh handle once its last binding has released the old one.
            let mut handles = [None; ROOTS + 1];
            let mut next_handle = 1u64;
            let mut index = BindingIndex::new();
            let mut live = Vec::<BindRecord>::new();
            let mut interfaces = Vec::<(u64, Option<Arc<[u8]>>)>::new();
            let mut seen = Vec::<(u8, u64)>::new();
            let mut support = Support::default();
            let mut max_generation = 0;
            for (step, op) in ops.iter().enumerate() {
                match *op {
                    Op::Bind(binding) => {
                        let name = identity(&format!("name_{}", binding.identity));
                        assert!(live.iter().all(|entry| entry.id.raw() != binding.id));
                        support.shadows += usize::from(live.iter().any(|entry| {
                            entry.identity == name && entry.module.gen().0 != binding.generation
                        }));
                        let pair_live = live.iter().any(|entry| {
                            entry.identity == name && entry.module.gen().0 == binding.generation
                        });
                        support.shared_pairs += usize::from(pair_live);
                        support.shared_modules += usize::from(
                            live.iter()
                                .any(|entry| entry.module.gen().0 == binding.generation),
                        );
                        let handle = match handles[binding.root] {
                            Some(handle) => handle,
                            None => {
                                let handle = ValueHandle(next_handle);
                                next_handle += 1;
                                handles[binding.root] = Some(handle);
                                handle
                            }
                        };
                        support.shared_roots +=
                            usize::from(live.iter().any(|entry| entry.handle == handle));
                        support.reinsertions += usize::from(
                            !pair_live && seen.contains(&(binding.identity, binding.generation)),
                        );
                        seen.push((binding.identity, binding.generation));
                        let entry = record(binding.generation, binding.id, handle, &name);
                        index.on_bind_record(&entry);
                        live.push(entry);
                        max_generation = max_generation.max(binding.generation);
                        support.binds += 1;
                    }
                    Op::Evict(id) => {
                        let position = live.iter().position(|entry| entry.id.raw() == id).unwrap();
                        // Preserve model order: candidate lookup's old scanning
                        // oracle must not inherit the index's swap_remove order.
                        let entry = live.remove(position);
                        let final_reference =
                            !live.iter().any(|other| other.handle == entry.handle);
                        assert_eq!(
                            index.on_evict_record(&entry),
                            final_reference,
                            "root release at step {step}: {op:?}"
                        );
                        if final_reference {
                            for group in &mut handles {
                                if *group == Some(entry.handle) {
                                    *group = None;
                                }
                            }
                        }
                        if !live.iter().any(|other| other.module == entry.module) {
                            interfaces
                                .retain(|(generation, _)| *generation != entry.module.gen().0);
                        }
                        support.evictions += 1;
                        support.released_roots += usize::from(final_reference);
                        support.held_roots += usize::from(!final_reference);
                    }
                    Op::Read(name, generation) => {
                        let name = identity(&format!("name_{name}"));
                        check_resolution(&index, &live, &name, generation);
                        support.reads += 1;
                    }
                    Op::Legacy(generation) => {
                        let module = SessionModule::val(Generation(generation));
                        index.mark_legacy_interface(module);
                        if live.iter().any(|entry| entry.module == module)
                            && !interfaces.iter().any(|(gen, _)| *gen == generation)
                        {
                            interfaces.push((generation, None));
                        }
                        max_generation = max_generation.max(generation);
                        support.legacy += 1;
                    }
                    Op::Fixture(generation, byte) => {
                        let bytes: Arc<[u8]> = Arc::from([byte]);
                        index.retain_fixture_interface(
                            SessionModule::val(Generation(generation)),
                            bytes.clone(),
                        );
                        interfaces.retain(|(gen, _)| *gen != generation);
                        interfaces.push((generation, Some(bytes)));
                        support.fixtures += 1;
                    }
                }
                check_observables(&index, &live, &interfaces, max_generation);
            }
            assert!(live.is_empty());
            assert!(interfaces.is_empty());
            assert_eq!(support.binds, support.evictions);
            support
        }

        fn config() -> Config {
            // The native runner declares a durable seed path; Cargo keeps
            // ordinary SourceParallel persistence. Neither default resolves
            // through staged manifest paths. Standard PROPTEST_* settings
            // remain available for larger runs and precise seed replay.
            let mut config = Config::default();
            if std::env::var_os("PROPTEST_MAX_SHRINK_ITERS").is_none() {
                config.max_shrink_iters = 4096;
            }
            if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
                config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
            }
            config
        }

        #[derive(Default, serde::Serialize)]
        struct Observed {
            callbacks: usize,
            completed_histories: usize,
            binds: usize,
            reads: usize,
            held_roots: usize,
            released_roots: usize,
            cohorts: [usize; 4],
        }

        impl Observed {
            fn completed(&mut self, support: &Support) {
                self.completed_histories += 1;
                self.binds += support.binds;
                self.reads += support.reads;
                self.held_roots += support.held_roots;
                self.released_roots += support.released_roots;
            }
        }

        #[test]
        fn arbitrary_histories_match_scanning_oracle() {
            let mut config = proptest::test_runner::contextualize_config(config());
            config.source_file = Some(file!());
            config.test_name = Some(concat!(
                module_path!(),
                "::arbitrary_histories_match_scanning_oracle"
            ));
            let configuration = format!("{config:?}");
            let cases = config.cases;
            let observed = RefCell::new(Observed::default());
            let result = TestRunner::new(config).run(&arbitrary_history(), |ops| {
                observed.borrow_mut().callbacks += 1;
                let support = replay(&ops);
                observed.borrow_mut().completed(&support);
                Ok(())
            });
            eprintln!(
                "binding_index_campaign={}",
                serde_json::json!({
                    "campaign": "arbitrary",
                    "configured_cases": cases,
                    "configuration": configuration,
                    "observation_scope": "runner callbacks in this process, including replay and shrinking",
                    "observed": &*observed.borrow(),
                })
            );
            if let Err(error) = result {
                panic!("arbitrary BindingIndex property failed: {error}");
            }
        }

        #[test]
        fn guided_histories_match_scanning_oracle() {
            let mut config = proptest::test_runner::contextualize_config(config());
            config.source_file = Some(file!());
            config.test_name = Some(concat!(
                module_path!(),
                "::guided_histories_match_scanning_oracle"
            ));
            let configuration = format!("{config:?}");
            let cases = config.cases;
            let observed = RefCell::new(Observed::default());
            let result = TestRunner::new(config).run(&guided_history(), |(cohort, ops)| {
                observed.borrow_mut().callbacks += 1;
                let support = replay(&ops);
                prop_assert!(support.binds >= 2, "{support:?}");
                prop_assert!(support.reads >= 4, "{support:?}");
                prop_assert!(support.fixtures >= 1, "{support:?}");
                match cohort {
                    0 => {
                        prop_assert!(support.shadows >= 1, "{support:?}");
                        prop_assert!(support.held_roots >= 1, "{support:?}");
                    }
                    1 => {
                        prop_assert!(support.shared_modules >= 1, "{support:?}");
                        prop_assert!(support.shared_roots >= 1, "{support:?}");
                        prop_assert!(support.held_roots >= 1, "{support:?}");
                        prop_assert!(support.reinsertions >= 1, "{support:?}");
                    }
                    2 => prop_assert!(support.reinsertions >= 1, "{support:?}"),
                    3 => {
                        prop_assert!(support.shared_pairs >= 1, "{support:?}");
                        prop_assert!(support.held_roots >= 1, "{support:?}");
                        prop_assert!(support.reinsertions >= 1, "{support:?}");
                    }
                    _ => unreachable!("bounded cohort selector"),
                }
                prop_assert!(support.released_roots >= 1, "{support:?}");
                let mut observed = observed.borrow_mut();
                observed.completed(&support);
                observed.cohorts[usize::from(cohort)] += 1;
                Ok(())
            });
            eprintln!(
                "binding_index_campaign={}",
                serde_json::json!({
                    "campaign": "guided",
                    "configured_cases": cases,
                    "configuration": configuration,
                    "observation_scope": "runner callbacks in this process, including replay and shrinking",
                    "observed": &*observed.borrow(),
                })
            );
            if let Err(error) = result {
                panic!("guided BindingIndex property failed: {error}");
            }
        }

        #[test]
        fn guided_cohorts_exercise_shadow_shared_root_and_reinsertion() {
            for cohort in 0..4 {
                let mut history = History::default();
                history.guided(cohort);
                let support = replay(&history.finish());
                assert!(
                    support.binds >= 2 && support.reads >= 4 && support.fixtures >= 1,
                    "{support:?}"
                );
                match cohort {
                    0 => assert!(support.shadows > 0 && support.held_roots > 0, "{support:?}"),
                    1 => assert!(
                        support.shared_modules > 0
                            && support.shared_roots > 0
                            && support.held_roots > 0
                            && support.reinsertions > 0,
                        "{support:?}"
                    ),
                    2 => assert!(
                        support.reinsertions > 0 && support.legacy > 0,
                        "{support:?}"
                    ),
                    3 => assert!(
                        support.shared_pairs > 0
                            && support.held_roots > 0
                            && support.reinsertions > 0,
                        "{support:?}"
                    ),
                    _ => unreachable!("bounded cohort selector"),
                }
                assert!(support.released_roots > 0, "{support:?}");
                eprintln!("BindingIndex cohort {cohort} support: {support:?}");
            }
        }
    }
}
