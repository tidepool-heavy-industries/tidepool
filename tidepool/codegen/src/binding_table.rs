//! Session binding table — the bridge GHC already uses, made concrete.
//!
//! GHCi splits a binding's identity into a *type* half (`ic_tythings`) and a
//! *value* half (the linker's `closure_env`), keyed by one `Name`. Our
//! [`BindingTable`] is exactly that bridge: keyed by one [`SessionVarId`], the
//! type half is the thin `Tidepool.Session.Val.G<g>` interface on disk (GHC's generated module)
//! and the value half is the live, GC-rooted [`BoundValue`] in the resident
//! machine's heap (the JIT's managed storage).
//!
//! ## The two-layer shape (domain model §4)
//!
//! A mutable `name → SessionVarId` map (`current`) over a retained set of
//! `SessionVarId → BindingEntry` (`live`). Rebinding a name mints a *fresh*
//! `SessionVarId` (a new `Val.G<g'>` module → a new `stableVarId`) and repoints
//! only `current`, so old roots stay reachable from captures in
//! already-compiled fragments and the `DataConTable::insert_checked` collision
//! guard is structurally never tripped: the gen-versioned module name yields a
//! fresh, collision-free `0xFE` external id per (re)bind, and the structured
//! type lives in the `.hi`, not a string. The `var_id` is minted by the
//! Haskell extract (`Translate.stableVarId`) and stored here verbatim (see
//! [`SessionVarId`]).
//!
//! Automatic observations have a bounded recent-name window. Expired roots
//! remain live while newer observations or frozen tips reference them. Explicit
//! persistent captures acquire the ordinary scope lifetime for their dependencies.
//!
//! ## Scope frames and immutable tips
//!
//! The shadowing layer is one frame PER SCOPE ([`ScopeId`]), not one map for
//! the session: `current: ScopeId → (name → newest id)`. `live` stays FLAT and
//! globally keyed by [`SessionVarId`] — ids are globally unique, and every
//! id-keyed read ([`BindingTable::get`], [`BindingTable::seed_external_env`],
//! [`BindingTable::live_modules`]) therefore resolves a scoped binding with no
//! change at all.
//!
//! A newly minted inheriting scope captures a flattened immutable tip of the
//! values visible from its parent. Later parent rebindings cannot leak into
//! the child. The child retains those entries through root leases and writes
//! new bindings only to its own mutable frame.
//!
//! Every no-arg method means [`ScopeId::ROOT`], the flat session, and keeps its
//! exact pre-C2 behavior: `bind(e) == bind_in(ROOT, e)`, `resolve(n) ==
//! resolve_in(_, ROOT, n)`, and `iter_current()` is the ROOT frame. The scoped
//! siblings take the [`ScopeTree`] as a PARAMETER rather than owning one: the
//! persistent declaration environment keys off the same [`ScopeId`]s, so exactly one tree exists per
//! session (`PersistentSession::scopes`). This table stores handle identities;
//! their owning machine resolves them when a program imports or evaluates a value.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};

use rpds::{HashTrieMapSync, HashTrieSetSync};
use tidepool_repr::{BindingName, SessionModule, SessionVarId, VarId};

use crate::prepared_program::{GroupInstanceId, SourceBinder, SourceInstanceLease};
use crate::scope::{ScopeId, ScopeTree};

/// Identity of one immutable value-binding view captured for a child scope.
///
/// IDs are monotonic within a resident session and never reused. The ID is
/// presentation/provenance; `SessionVarId` remains the binding authority.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BindingTipId(pub u64);

/// Cache witness issued by the binding owner. A changed witness requires
/// recomputing scoped semantics; it is not itself a stale-admission verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindingScopeWitness {
    inherited: Option<BindingTipId>,
    frames: Vec<(ScopeId, u64)>,
    dependency_revision: Option<u64>,
}

/// Exact scoped binding and source-selection semantics for admission and
/// publication. Cache revisions do not participate in this proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindingScopeSnapshot {
    inherited: Option<BindingTipId>,
    dependencies: Vec<SessionVarId>,
    observation_dependencies: Vec<(SessionVarId, Vec<SessionVarId>)>,
    selection: SourceSelectionView,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("binding identity {id:?} already belongs to another immutable entry")]
pub struct BindingIdentityError {
    pub id: SessionVarId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingPromotionError {
    MissingOrForeignBinding,
    NotCurrentInSource,
    DuplicateName,
    MissingOrForeignSourceInstance,
    ConflictingSourceOrigin,
    MissingSourceDomain,
    UnverifiableSourceSelection,
}

/// Exact binding writes and dependency leases checked before a publication
/// decision. The caller holds the owning machine checkout through commit.
pub struct PreparedBindingPromotion {
    target: ScopeId,
    writes: Vec<(BindingName, SessionVarId)>,
    retained: HashSet<SessionVarId>,
    source_instances: HashSet<SourceLeaseKey>,
    source_origins: HashMap<
        tidepool_repr::execution_schema::CachedHomeOwner,
        crate::prepared_program::SourceInstanceDomain,
    >,
    source_domains: HashMap<crate::prepared_program::SourceInstanceDomain, SourceDomainRecord>,
    next_tip: Option<u64>,
}

pub struct PreparedSourceOwnerOrigin {
    scope: ScopeId,
    owner: tidepool_repr::execution_schema::CachedHomeOwner,
    domain: crate::prepared_program::SourceInstanceDomain,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceDomainRecord {
    selected: HashTrieSetSync<SourceLeaseKey>,
    authored: HashTrieMapSync<
        tidepool_repr::execution_schema::CachedHomeOwner,
        crate::prepared_program::SourceInstanceDomain,
    >,
}

impl SourceDomainRecord {
    fn empty() -> Self {
        Self {
            selected: HashTrieSetSync::new_sync(),
            authored: HashTrieMapSync::new_sync(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceSelectionView {
    current: crate::prepared_program::SourceInstanceDomain,
    domains: HashTrieMapSync<crate::prepared_program::SourceInstanceDomain, SourceDomainRecord>,
}

impl SourceSelectionView {
    fn for_scope(scope: ScopeId) -> Self {
        let current = crate::prepared_program::SourceInstanceDomain::for_scope(scope);
        let mut domains = HashTrieMapSync::new_sync();
        domains.insert_mut(current, SourceDomainRecord::empty());
        Self { current, domains }
    }

    fn add(&mut self, domain: crate::prepared_program::SourceInstanceDomain, key: SourceLeaseKey) {
        let mut record = self
            .domains
            .get(&domain)
            .cloned()
            .expect("source domain was admitted");
        record.selected.insert_mut(key);
        self.domains.insert_mut(domain, record);
    }

    /// Relabel the retained semantic graph; physical leases remain unchanged.
    fn fork(
        &self,
        namespace: BindingTipId,
        roots: impl IntoIterator<Item = crate::prepared_program::SourceInstanceDomain>,
    ) -> Result<
        (
            HashMap<
                crate::prepared_program::SourceInstanceDomain,
                crate::prepared_program::SourceInstanceDomain,
            >,
            HashMap<crate::prepared_program::SourceInstanceDomain, SourceDomainRecord>,
        ),
        BindingPromotionError,
    > {
        let mut pending: Vec<_> = roots.into_iter().collect();
        let mut reachable = std::collections::BTreeSet::new();
        while let Some(domain) = pending.pop() {
            if !reachable.insert(domain) {
                continue;
            }
            let record = self
                .domains
                .get(&domain)
                .ok_or(BindingPromotionError::MissingSourceDomain)?;
            pending.extend(record.authored.values().copied());
        }
        let remap: HashMap<_, _> = reachable
            .iter()
            .enumerate()
            .map(|(slot, old)| {
                (
                    *old,
                    crate::prepared_program::SourceInstanceDomain::in_view(namespace, slot as u64),
                )
            })
            .collect();
        let records = reachable
            .into_iter()
            .map(|old| {
                let original = self.domains.get(&old).expect("reachable domain admitted");
                let record = SourceDomainRecord {
                    selected: original.selected.clone(),
                    authored: original
                        .authored
                        .iter()
                        .map(|(owner, target)| (owner.clone(), remap[target]))
                        .collect(),
                };
                (remap[&old], record)
            })
            .collect();
        Ok((remap, records))
    }
}

/// One machine-owned materialized source binder. `GroupInstanceId` prevents a
/// second installation of the same immutable group from sharing mutable CAFs.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceLeaseKey {
    pub instance: GroupInstanceId,
    pub binder: Arc<SourceBinder>,
}

impl SourceLeaseKey {
    #[must_use]
    pub fn of(lease: &SourceInstanceLease) -> Self {
        Self {
            instance: lease.instance(),
            binder: Arc::new(lease.binder().clone()),
        }
    }
}

fn same_source_lease(left: &SourceInstanceLease, right: &SourceInstanceLease) -> bool {
    left.instance() == right.instance()
        && left.owner() == right.owner()
        && left.original_ordinal() == right.original_ordinal()
        && left.binder() == right.binder()
        && left.value() == right.value()
        && left.handle() == right.handle()
        && left.entry_signature() == right.entry_signature()
}

/// One install's exact changes to lexical selection and physical custody.
/// It cannot be cloned; failed-turn cleanup applies this delta at most once.
#[derive(Debug, Default)]
pub struct SourceScopeAdmission {
    scope: Option<ScopeId>,
    keys: Vec<SourceLeaseKey>,
    owned: Vec<SourceLeaseKey>,
    selected: Vec<(
        crate::prepared_program::SourceInstanceDomain,
        SourceLeaseKey,
    )>,
    shares: HashSet<SourceLeaseKey>,
    rolled_back: std::sync::atomic::AtomicBool,
}

static_assertions::assert_impl_all!(SourceScopeAdmission: Send, Sync);

impl std::ops::Deref for SourceScopeAdmission {
    type Target = [SourceLeaseKey];
    fn deref(&self) -> &Self::Target {
        &self.keys
    }
}

impl<'a> IntoIterator for &'a SourceScopeAdmission {
    type Item = &'a SourceLeaseKey;
    type IntoIter = std::slice::Iter<'a, SourceLeaseKey>;
    fn into_iter(self) -> Self::IntoIter {
        self.keys.iter()
    }
}

struct ScopedSourceLease {
    owner: ScopeId,
    token: SourceInstanceLease,
    shares: usize,
    owner_retired: bool,
}

pub struct ScopeDrain {
    pub bindings: Vec<BindingEntry>,
    pub source_instances: Vec<SourceInstanceLease>,
}

#[derive(Debug)]
struct BindingTip {
    id: BindingTipId,
    visible: HashTrieMapSync<BindingName, SessionVarId>,
    /// Frozen exact ancestor values, including shadowed generations needed by
    /// inherited declaration code. Leasing them keeps observations alive if
    /// their original owner later stops naming them.
    retained: HashTrieSetSync<SessionVarId>,
    source_instances: HashTrieSetSync<SourceLeaseKey>,
    leases: Arc<BindingLeaseChunk>,
}

#[derive(Debug)]
struct BindingLeaseChunk {
    parents: Vec<Arc<BindingLeaseChunk>>,
    bindings: HashSet<SessionVarId>,
    source_instances: HashSet<SourceLeaseKey>,
}

struct CaptureLeaseHistory {
    chunk: Weak<BindingLeaseChunk>,
    retained: HashTrieSetSync<SessionVarId>,
    source_instances: HashTrieSetSync<SourceLeaseKey>,
    append_epoch: Option<u64>,
}

/// Mutation-owned exact capture projection. Immutable inherited/promoted
/// memberships are closed; only own mutable observations require refresh.
struct CapturableMembership {
    retained: HashTrieSetSync<SessionVarId>,
    source_instances: HashTrieSetSync<SourceLeaseKey>,
    /// Mutable observation closure outside the scope's own binding set.
    /// Frozen inherited/promoted membership is maintained independently.
    dependency_ids: HashSet<SessionVarId>,
    dependency_revision: u64,
    dependencies_dirty: bool,
    // Nonzero epochs prove that every mutation since the latest capture only
    // added exact membership. A removal or retracting graph refresh breaks it.
    append_epoch: u64,
    added_bindings: HashSet<SessionVarId>,
    added_sources: HashSet<SourceLeaseKey>,
}

struct CaptureMembership {
    retained: HashTrieSetSync<SessionVarId>,
    source_instances: HashTrieSetSync<SourceLeaseKey>,
    append_epoch: Option<u64>,
    added_bindings: HashSet<SessionVarId>,
    added_sources: HashSet<SourceLeaseKey>,
}

impl CapturableMembership {
    fn break_append_proof(&mut self) {
        self.append_epoch = if self.append_epoch == 0 {
            0
        } else {
            self.append_epoch.checked_add(1).unwrap_or(0)
        };
        self.added_bindings.clear();
        self.added_sources.clear();
    }
}

/// A value retained by a prepared-STG `PreparedMachine`: tenured as-is (never
/// deep-forced, so its preparation policy is Tier-1's). `handle` names its
/// exact retained value in the owning machine's resource ledger. Later
/// programs name that handle in `ImportBindings`. It is held under the machine's
/// ROOT scope so no resource-scope close releases it; `identity` is what a later
/// program links against when it imports this binding.
#[derive(Clone, Debug)]
pub struct BoundValue {
    pub handle: crate::prepared_program::PreparedHandle,
    pub identity: tidepool_repr::execution_schema::SymbolIdentity,
}

/// One resolved session binding — the bridge record for a single `x`.
pub struct BindingEntry {
    /// The user-facing name (`"x"`).
    pub name: BindingName,
    /// Stable `0xFE` session id minted by the extractor.
    pub id: SessionVarId,
    /// `Tidepool.Session.Val.G<g>` — derives the `.hi` path and is what later
    /// turns inject (`SessionScope.ssValIfaces`).
    pub module: SessionModule,
    /// The live prepared heap root and import identity.
    pub value: BoundValue,
    /// `ppr` of the binding's type, for `:t` only. The STRUCTURED type carrier
    /// is the thin iface on disk, not this string.
    pub type_display: Option<String>,
    /// The bind's defining turn text (`x <- action` / `let x = e`) — so
    /// `:program` can re-emit the session as a replayable notebook. `None` for
    /// bindings minted outside a normal bind turn (tests).
    pub defining_expr: Option<String>,
    /// The scope frame this binding was born in — [`ScopeId::ROOT`] for every
    /// flat-session bind. Set by [`BindingTable::bind_in`] from its `scope`
    /// argument (a literal's value here is overwritten at bind time, so a
    /// construction site cannot desync the entry from its frame), and read by
    /// [`BindingTable::drain_scope`] to find exactly the entries a retiring
    /// scope owns.
    pub scope: ScopeId,
}

/// The `name → (SessionVarId, PreparedHandle, SessionModule)` bridge (domain §4).
///
/// `current` maps a name to its newest binding's id (shadowing: latest-wins);
/// `live` retains EVERY still-rooted binding, including shadowed older ones, so
/// fragments compiled against an old gen keep resolving. Scope retirement and
/// automatic-observation collection return unreferenced entries to the session
/// owner, which releases their root registrations.
///
/// `current` is keyed by [`ScopeId`] FIRST: one shadowing frame per scope, so
/// two sibling scopes can each bind `helper` without either seeing the other
/// and without the parent gaining the name. `live` is deliberately NOT keyed
/// by scope — a `SessionVarId` is globally unique, and an already-compiled
/// fragment resolves by id regardless of which frame minted it.
pub struct BindingTable {
    /// Shadowing layer, one frame per scope: scope → (name → newest gen's id).
    /// A scope with no bindings has no frame (absent, not empty).
    current: HashMap<ScopeId, HashMap<BindingName, SessionVarId>>,
    /// The exact name projection this frame passes to a new child. Seeded
    /// frames start at their inherited root; mutation changes only trie paths.
    /// Own aliases stay in `current` but suppress the captured name.
    capturable_names: HashMap<ScopeId, HashTrieMapSync<BindingName, SessionVarId>>,
    capturable_membership: HashMap<ScopeId, CapturableMembership>,
    /// Append-only-by-id store of every live binding (old gens retained),
    /// flat and globally keyed across every scope.
    live: HashMap<SessionVarId, BindingEntry>,
    owned: HashMap<ScopeId, HashSet<SessionVarId>>,
    modules: HashMap<SessionModule, usize>,
    revision: u64,
    scope_revisions: HashMap<ScopeId, u64>,
    /// Immutable, flattened inherited view captured when a scope is minted.
    tips: HashMap<ScopeId, BindingTip>,
    capture_history: HashMap<ScopeId, CaptureLeaseHistory>,
    /// Names deliberately hidden by this scope's persistent declaration environment.
    hidden: HashMap<ScopeId, HashSet<BindingName>>,
    /// Binding tips and prepared work retaining each value identity.
    leases: HashMap<SessionVarId, usize>,
    /// Entries whose owning scope retired while another owner still leased
    /// them. They leave `live` when the final lease is released.
    retired_owners: HashSet<SessionVarId>,
    next_tip: u64,
    observations: HashMap<SessionVarId, ObservationBinding>,
    observation_owners: HashMap<ScopeId, usize>,
    dependency_revision: u64,
    next_observation_order: u64,
    scope_local_aliases: HashSet<SessionVarId>,
    /// Exact private owners and their dependency closure retained by a
    /// public scope after promotion. The original ids, interfaces and roots
    /// remain in `live`; retiring the private scope cannot evict them.
    promoted: HashMap<ScopeId, HashSet<SessionVarId>>,
    /// Machine-owned source roots share this table's scope/tip lifetime.
    source_instances: HashMap<SourceLeaseKey, ScopedSourceLease>,
    source_owned: HashMap<ScopeId, HashSet<SourceLeaseKey>>,
    promoted_source_instances: HashMap<ScopeId, HashSet<SourceLeaseKey>>,
    /// Explicit lexical installation choices. Custody promotion retains roots
    /// without granting an ambient choice among independent mutable instances.
    source_selection: HashMap<ScopeId, SourceSelectionView>,
}

#[cfg(test)]
mod promotion_tests {
    use super::*;
    use crate::prepared_program::PreparedHandle;
    use crate::suspension::ValueHandle;
    use tidepool_repr::execution_schema::{RuntimeRep, SymbolIdentity};
    use tidepool_repr::{Generation, SessionModule};

    fn entry(name: &str, generation: u64) -> BindingEntry {
        BindingEntry {
            name: BindingName(name.into()),
            id: SessionVarId::from_extract(generation),
            module: SessionModule::val(Generation(generation)),
            value: BoundValue {
                handle: PreparedHandle::new(ValueHandle(generation), RuntimeRep::LiftedRef),
                identity: SymbolIdentity {
                    unit: "test".into(),
                    module: format!("Val.G{generation}"),
                    namespace: "value".into(),
                    occurrence: name.into(),
                    record_parent: None,
                },
            },
            type_display: None,
            defining_expr: None,
            scope: ScopeId::ROOT,
        }
    }

    #[test]
    fn promoted_private_identity_wins_by_completion_and_survives_source_retirement() {
        let mut tree = ScopeTree::new();
        let source = tree.mint_isolated();
        let public = tree.mint_isolated();
        let mut table = BindingTable::new();

        let old = entry("answer", 9);
        let newer_completion = entry("answer", 3);
        let old_id = old.id;
        let private_id = newer_completion.id;
        table.bind_in(public, old).expect("fresh immutable binding");
        table
            .bind_in(source, newer_completion)
            .expect("fresh immutable binding");

        table
            .promote_exact_bindings_in(source, public, &[private_id])
            .expect("exact private write publishes");
        assert_eq!(
            table.resolve_in(&tree, public, "answer").unwrap().id,
            private_id
        );
        assert_eq!(table.lease_count(private_id), 1);
        table
            .promote_exact_bindings_in(source, public, &[private_id])
            .expect("same exact promotion is idempotent");
        assert_eq!(table.lease_count(private_id), 1);
        assert!(table.drain_scope(source).is_empty());
        assert_eq!(
            table.resolve_in(&tree, public, "answer").unwrap().id,
            private_id
        );
        assert!(table
            .scope_reachable_modules(&tree, public)
            .any(|module| module == SessionModule::val(Generation(3))));
        let released = table.drain_scope(public);
        assert_eq!(released.len(), 2);
        assert!(released.iter().any(|entry| entry.id == old_id));
        assert!(released.iter().any(|entry| entry.id == private_id));
    }

    #[test]
    fn promoted_alias_retains_source_dependency_and_invalid_batch_changes_nothing() {
        let mut tree = ScopeTree::new();
        let source = tree.mint_isolated();
        let public = tree.mint_isolated();
        let mut table = BindingTable::new();

        let root = entry("root", 4);
        let root_id = root.id;
        table
            .bind_in(source, root)
            .expect("fresh immutable binding");
        let mut alias = entry("alias", 5);
        alias.value.handle = table.get(root_id).unwrap().value.handle;
        let alias_id = alias.id;
        table
            .bind_alias_in(source, alias, root_id)
            .expect("same-scope alias binds");
        let missing = SessionVarId::from_extract(99);
        assert_eq!(
            table.promote_exact_bindings_in(source, public, &[alias_id, missing]),
            Err(BindingPromotionError::MissingOrForeignBinding)
        );
        assert_eq!(table.lease_count(alias_id), 0);
        assert_eq!(table.lease_count(root_id), 0);
        assert!(table.resolve_in(&tree, public, "alias").is_none());

        table
            .promote_exact_bindings_in(source, public, &[alias_id])
            .expect("alias publishes with dependency closure");
        assert_eq!(table.lease_count(alias_id), 1);
        assert_eq!(table.lease_count(root_id), 1);
        assert!(table.drain_scope(source).is_empty());
        assert_eq!(
            table.resolve_in(&tree, public, "alias").unwrap().id,
            alias_id
        );
        let reachable: Vec<_> = table.scope_reachable_modules(&tree, public).collect();
        assert!(reachable.contains(&SessionModule::val(Generation(4))));
        assert!(reachable.contains(&SessionModule::val(Generation(5))));
        let released = table.drain_scope(public);
        assert_eq!(released.len(), 2);
        assert!(table.is_empty());
    }

    #[test]
    fn exact_retained_import_uses_frozen_scope_not_global_newest() {
        let mut tree = ScopeTree::new();
        let mut table = BindingTable::new();

        let first = entry("answer", 4);
        let first_id = first.id;
        let first_identity = first.value.identity.clone();
        table
            .bind_in(ScopeId::ROOT, first)
            .expect("fresh immutable binding");

        table
            .bind_in(ScopeId::ROOT, entry("answer", 6))
            .expect("fresh immutable binding");

        let captured = tree.mint_child(ScopeId::ROOT).expect("live root");
        table.seed_scope(&tree, ScopeId::ROOT, captured);

        let later = entry("later", 7);
        let later_identity = later.value.identity.clone();
        table
            .bind_in(ScopeId::ROOT, later)
            .expect("fresh immutable binding");
        let descendant = tree.mint_child(captured).expect("captured scope lives");
        table.seed_scope(&tree, captured, descendant);
        let sibling = tree.mint_isolated();

        let newer = entry("answer", 5);
        let newer_identity = newer.value.identity.clone();
        table
            .bind_in(sibling, newer)
            .expect("fresh immutable binding");

        assert_eq!(
            table
                .resolve_exact_prepared_in(&tree, captured, &first_identity, 4)
                .map(|entry| entry.id),
            Some(first_id)
        );
        assert!(table
            .resolve_exact_prepared_in(&tree, captured, &newer_identity, 5)
            .is_none());
        assert!(table
            .resolve_exact_prepared_in(&tree, captured, &later_identity, 7)
            .is_none());
        assert_eq!(
            table
                .resolve_exact_prepared_in(&tree, descendant, &first_identity, 4)
                .map(|entry| entry.id),
            Some(first_id)
        );
        assert!(table
            .resolve_exact_prepared_in(&tree, descendant, &later_identity, 7)
            .is_none());
        assert!(table
            .resolve_exact_prepared_in(&tree, sibling, &first_identity, 4)
            .is_none());

        assert!(table.save_observation(first_id, &[], 0).is_empty());
        assert!(table.lease_count(first_id) >= 1);
        assert!(table
            .resolve_exact_prepared_in(&tree, descendant, &first_identity, 4)
            .is_some());
        assert!(table.drain_scope(descendant).is_empty());
        assert!(table.drain_scope(captured).is_empty());
        assert_eq!(table.collect_observations().len(), 1);
    }
}

struct ObservationBinding {
    owner: ScopeId,
    dependencies: Vec<SessionVarId>,
    recent: Option<u64>,
    retain_while_current: bool,
}

impl Default for BindingTable {
    fn default() -> Self {
        Self {
            current: HashMap::new(),
            capturable_names: HashMap::new(),
            capturable_membership: HashMap::new(),
            live: HashMap::new(),
            owned: HashMap::new(),
            modules: HashMap::new(),
            revision: 1,
            scope_revisions: HashMap::new(),
            tips: HashMap::new(),
            capture_history: HashMap::new(),
            hidden: HashMap::new(),
            leases: HashMap::new(),
            retired_owners: HashSet::new(),
            next_tip: 1,
            observations: HashMap::new(),
            observation_owners: HashMap::new(),
            dependency_revision: 1,
            next_observation_order: 0,
            scope_local_aliases: HashSet::new(),
            promoted: HashMap::new(),
            source_instances: HashMap::new(),
            source_owned: HashMap::new(),
            promoted_source_instances: HashMap::new(),
            source_selection: HashMap::new(),
        }
    }
}

impl Drop for BindingTable {
    fn drop(&mut self) {
        // The machine owns final root destruction. Unwind internal chunk
        // ownership iteratively even when a long prefix drops as one table.
        let tips = std::mem::take(&mut self.tips);
        for tip in tips.into_values() {
            self.release_chunks(tip.leases);
        }
    }
}

impl BindingTable {
    /// Table-wide cache invalidation only. Exhaustion permanently disables
    /// witnesses instead of allowing an old revision to become current again.
    #[must_use]
    pub fn mutation_revision(&self) -> Option<u64> {
        (self.revision != 0).then_some(self.revision)
    }

    #[must_use]
    pub fn scope_witness(&self, tree: &ScopeTree, scope: ScopeId) -> Option<BindingScopeWitness> {
        if !tree.is_live(scope) || self.mutation_revision().is_none() {
            return None;
        }
        let inherited = self.tip_id(scope);
        let owners = if inherited.is_some() {
            vec![scope]
        } else {
            tree.lookup_chain(scope)
        };
        Some(BindingScopeWitness {
            inherited,
            dependency_revision: owners
                .iter()
                .any(|owner| self.observation_owners.contains_key(owner))
                .then_some(self.dependency_revision),
            frames: owners
                .into_iter()
                .map(|owner| {
                    (
                        owner,
                        self.scope_revisions.get(&owner).copied().unwrap_or(0),
                    )
                })
                .collect(),
        })
    }

    /// Recompute the relevant observation closure and retain exact source
    /// domain choices. Unrelated observation mutations invalidate caches but
    /// cannot change this immutable semantic snapshot.
    pub fn scope_snapshot(
        &self,
        tree: &ScopeTree,
        scope: ScopeId,
    ) -> Result<BindingScopeSnapshot, BindingPromotionError> {
        // The existing selection owner validates every selected lease and
        // authored domain before its metadata can authorize publication.
        self.source_domain_selection_in(tree, scope)?;
        let mut dependencies: Vec<_> = self
            .scope_dependency_ids_slow(tree, scope)
            .into_iter()
            .collect();
        dependencies.sort_by_key(|id| id.raw());
        // Own mutable observations can transitively reference foreign
        // observations. Retain the exact edges as well as the root union:
        // a changed per-write closure can hide inside an unchanged union.
        // Inherited/promoted custody is already frozen and does not follow
        // later edits to the original observation graph.
        let owners = if self.tips.contains_key(&scope) {
            vec![scope]
        } else {
            tree.lookup_chain(scope)
        };
        let own = owners
            .iter()
            .flat_map(|owner| self.owned.get(owner).into_iter().flat_map(HashSet::iter))
            .copied();
        let mut observation_dependencies: Vec<_> = self
            .dependency_closure(own)
            .into_iter()
            .filter_map(|id| {
                self.observations.get(&id).map(|observation| {
                    let mut edges = observation.dependencies.clone();
                    edges.sort_by_key(|id| id.raw());
                    edges.dedup();
                    (id, edges)
                })
            })
            .collect();
        observation_dependencies.sort_by_key(|(id, _)| id.raw());
        Ok(BindingScopeSnapshot {
            inherited: self.tip_id(scope),
            dependencies,
            observation_dependencies,
            selection: self
                .source_selection
                .get(&scope)
                .cloned()
                .unwrap_or_else(|| SourceSelectionView::for_scope(scope)),
        })
    }

    fn changed(&mut self, scope: ScopeId) {
        self.revision = if self.revision == 0 {
            0
        } else {
            self.revision.checked_add(1).unwrap_or(0)
        };
        if self.current.contains_key(&scope)
            || self.tips.contains_key(&scope)
            || self.owned.contains_key(&scope)
            || self.source_owned.contains_key(&scope)
            || self.promoted.contains_key(&scope)
            || self.promoted_source_instances.contains_key(&scope)
            || self.source_selection.contains_key(&scope)
        {
            self.scope_revisions.insert(scope, self.revision);
        } else {
            self.scope_revisions.remove(&scope);
        }
    }

    fn membership_mut(&mut self, scope: ScopeId) -> &mut CapturableMembership {
        self.capturable_membership.entry(scope).or_insert_with(|| {
            let tip = self.tips.get(&scope);
            CapturableMembership {
                retained: tip.map(|tip| tip.retained.clone()).unwrap_or_default(),
                source_instances: tip
                    .map(|tip| tip.source_instances.clone())
                    .unwrap_or_default(),
                dependency_ids: HashSet::new(),
                dependency_revision: self.dependency_revision,
                dependencies_dirty: false,
                append_epoch: 1,
                added_bindings: HashSet::new(),
                added_sources: HashSet::new(),
            }
        })
    }

    fn add_capturable_bindings(
        &mut self,
        scope: ScopeId,
        ids: impl IntoIterator<Item = SessionVarId>,
    ) {
        let projected = self.membership_mut(scope);
        for id in ids {
            if !projected.retained.contains(&id) {
                projected.retained.insert_mut(id);
                projected.added_bindings.insert(id);
            }
        }
    }

    fn add_capturable_sources(
        &mut self,
        scope: ScopeId,
        keys: impl IntoIterator<Item = SourceLeaseKey>,
    ) {
        let projected = self.membership_mut(scope);
        for key in keys {
            if !projected.source_instances.contains(&key) {
                projected.source_instances.insert_mut(key.clone());
                projected.added_sources.insert(key);
            }
        }
    }

    fn remove_capturable_binding(&mut self, scope: ScopeId, id: SessionVarId) {
        let frozen = self
            .tips
            .get(&scope)
            .is_some_and(|tip| tip.retained.contains(&id))
            || self
                .promoted
                .get(&scope)
                .is_some_and(|ids| ids.contains(&id));
        let observations = self.observation_owners.contains_key(&scope);
        if let Some(projected) = self.capturable_membership.get_mut(&scope) {
            projected.break_append_proof();
            if observations {
                projected.dependencies_dirty = true;
            }
            if !frozen {
                projected.retained.remove_mut(&id);
            }
        }
    }

    fn remove_capturable_source(&mut self, scope: ScopeId, key: &SourceLeaseKey) {
        let frozen = self
            .tips
            .get(&scope)
            .is_some_and(|tip| tip.source_instances.contains(key))
            || self
                .promoted_source_instances
                .get(&scope)
                .is_some_and(|keys| keys.contains(key));
        if let Some(projected) = self.capturable_membership.get_mut(&scope) {
            if !frozen && projected.source_instances.remove_mut(key) {
                projected.break_append_proof();
            }
        }
    }

    fn membership_dependencies_current(
        &self,
        scope: ScopeId,
        projected: &CapturableMembership,
    ) -> bool {
        !projected.dependencies_dirty
            && (!self.observation_owners.contains_key(&scope)
                || (self.dependency_revision != 0
                    && projected.dependency_revision == self.dependency_revision))
    }

    fn capture_membership(&mut self, tree: &ScopeTree, scope: ScopeId) -> CaptureMembership {
        if !tree.is_live(scope) {
            return CaptureMembership {
                retained: HashTrieSetSync::new_sync(),
                source_instances: HashTrieSetSync::new_sync(),
                append_epoch: None,
                added_bindings: HashSet::new(),
                added_sources: HashSet::new(),
            };
        }
        if !self.tips.contains_key(&scope) && tree.parent_of(scope).is_some() {
            // Unseeded legacy scopes observe mutable ancestors. Their full
            // current closure remains the existing conservative fallback.
            return CaptureMembership {
                retained: self
                    .scope_dependency_ids_slow(tree, scope)
                    .into_iter()
                    .collect(),
                source_instances: self
                    .source_instance_keys_in_slow(tree, scope)
                    .into_iter()
                    .collect(),
                append_epoch: None,
                added_bindings: HashSet::new(),
                added_sources: HashSet::new(),
            };
        }
        self.membership_mut(scope);
        if !self.membership_dependencies_current(scope, &self.capturable_membership[&scope]) {
            // Validate the complete mutable owner graph, including cycles and
            // foreign observation edits. Own bindings already have mutation-
            // maintained trie paths; only foreign dependency membership needs
            // reconciliation with the previous graph closure.
            let own = self.owned.get(&scope);
            let mut dependency_ids =
                self.dependency_closure(own.into_iter().flat_map(HashSet::iter).copied());
            dependency_ids.retain(|id| !own.is_some_and(|ids| ids.contains(id)));
            let tip = self.tips.get(&scope);
            let promoted = self.promoted.get(&scope);
            let revision = self.dependency_revision;
            let projected = self
                .capturable_membership
                .get_mut(&scope)
                .expect("membership exists");
            // A removed mutable edge cannot retract separately frozen custody.
            let removed = projected
                .dependency_ids
                .iter()
                .filter(|id| {
                    !dependency_ids.contains(id)
                        && !own.is_some_and(|ids| ids.contains(id))
                        && !tip.is_some_and(|tip| tip.retained.contains(id))
                        && !promoted.is_some_and(|ids| ids.contains(id))
                })
                .copied()
                .collect::<Vec<_>>();
            if !removed.is_empty() {
                projected.break_append_proof();
                for id in removed {
                    projected.retained.remove_mut(&id);
                }
            }
            for id in dependency_ids
                .difference(&projected.dependency_ids)
                .copied()
            {
                if !projected.retained.contains(&id) {
                    projected.retained.insert_mut(id);
                    projected.added_bindings.insert(id);
                }
            }
            projected.dependency_ids = dependency_ids;
            projected.dependency_revision = revision;
            projected.dependencies_dirty = false;
        }
        let projected = self
            .capturable_membership
            .get_mut(&scope)
            .expect("membership exists");
        CaptureMembership {
            retained: projected.retained.clone(),
            source_instances: projected.source_instances.clone(),
            append_epoch: (projected.append_epoch != 0).then_some(projected.append_epoch),
            added_bindings: std::mem::take(&mut projected.added_bindings),
            added_sources: std::mem::take(&mut projected.added_sources),
        }
    }

    /// Project one mutated local name over its immutable inherited winner.
    /// A current alias masks that inherited winner even though children do
    /// not inherit the alias itself. Explicit hiding masks both namespaces.
    fn refresh_capturable_name(&mut self, scope: ScopeId, name: &BindingName) {
        let current = self.current.get(&scope).and_then(|frame| frame.get(name));
        let selected = if let Some(id) = current {
            (!self.scope_local_aliases.contains(id) && self.live.contains_key(id)).then_some(*id)
        } else if self
            .hidden
            .get(&scope)
            .is_some_and(|hidden| hidden.contains(name))
        {
            None
        } else {
            self.tips
                .get(&scope)
                .and_then(|tip| tip.visible.get(name))
                .copied()
                .filter(|id| self.live.contains_key(id))
        };
        match selected {
            Some(id) => {
                let projected = self.capturable_names.entry(scope).or_default();
                if projected.get(name) != Some(&id) {
                    projected.insert_mut(name.clone(), id);
                }
            }
            None => {
                if let Some(projected) = self.capturable_names.get_mut(&scope) {
                    if projected.contains_key(name) {
                        projected.remove_mut(name);
                    }
                    if projected.is_empty() && !self.tips.contains_key(&scope) {
                        self.capturable_names.remove(&scope);
                    }
                }
            }
        }
    }

    fn remove_entry(&mut self, id: SessionVarId) -> Option<BindingEntry> {
        let entry = self.live.remove(&id)?;
        self.remove_observation(id);
        if let Some(owned) = self.owned.get_mut(&entry.scope) {
            owned.remove(&id);
            if owned.is_empty() {
                self.owned.remove(&entry.scope);
            }
        }
        self.remove_capturable_binding(entry.scope, id);
        let count = self
            .modules
            .get_mut(&entry.module)
            .expect("live module is indexed");
        *count -= 1;
        if *count == 0 {
            self.modules.remove(&entry.module);
        }
        self.refresh_capturable_name(entry.scope, &entry.name);
        self.changed(entry.scope);
        Some(entry)
    }

    fn remove_source(&mut self, key: &SourceLeaseKey) -> Option<ScopedSourceLease> {
        let lease = self.source_instances.remove(key)?;
        self.remove_capturable_source(lease.owner, key);
        if let Some(owned) = self.source_owned.get_mut(&lease.owner) {
            owned.remove(key);
            if owned.is_empty() {
                self.source_owned.remove(&lease.owner);
            }
        }
        self.changed(lease.owner);
        Some(lease)
    }

    /// Check immutable identity before a caller changes declaration visibility
    /// or transfers native root custody. Identical reinsertion changes nothing.
    pub fn validate_bind_in(
        &self,
        scope: ScopeId,
        entry: &BindingEntry,
    ) -> Result<(), BindingIdentityError> {
        if let Some(existing) = self.live.get(&entry.id) {
            if existing.scope != scope
                || existing.name != entry.name
                || existing.module != entry.module
                || existing.value.handle != entry.value.handle
                || existing.value.identity != entry.value.identity
                || existing.type_display != entry.type_display
                || existing.defining_expr != entry.defining_expr
            {
                return Err(BindingIdentityError { id: entry.id });
            }
        }
        Ok(())
    }

    fn insert_observation(&mut self, id: SessionVarId, observation: ObservationBinding) {
        self.remove_observation(id);
        let scope = observation.owner;
        *self.observation_owners.entry(scope).or_default() += 1;
        self.observations.insert(id, observation);
        self.membership_mut(scope).dependencies_dirty = true;
        self.changed(scope);
        self.dependency_revision = self.revision;
    }

    fn remove_observation(&mut self, id: SessionVarId) -> Option<ObservationBinding> {
        let observation = self.observations.remove(&id)?;
        if let Some(projected) = self.capturable_membership.get_mut(&observation.owner) {
            projected.dependencies_dirty = true;
        }
        let count = self
            .observation_owners
            .get_mut(&observation.owner)
            .expect("observation owner is indexed");
        *count -= 1;
        if *count == 0 {
            self.observation_owners.remove(&observation.owner);
        }
        self.changed(observation.owner);
        self.dependency_revision = self.revision;
        Some(observation)
    }

    /// Mark a compiler-issued binding as an automatic observation. Only the
    /// newest `limit` names in its scope remain discoverable. Values referenced
    /// by newer observations or frozen fork tips remain rooted until unused.
    #[allow(
        clippy::expect_used,
        reason = "monotonic observation identities must never wrap; exhaustion is an explicit process-lifetime invariant failure"
    )]
    pub fn save_observation(
        &mut self,
        id: SessionVarId,
        dependencies: &[VarId],
        limit: usize,
    ) -> Vec<BindingEntry> {
        let Some(entry) = self.live.get(&id) else {
            return Vec::new();
        };
        let scope = entry.scope;
        let dependencies = dependencies
            .iter()
            .copied()
            .map(SessionVarId::from_var)
            .filter(|id| self.observations.contains_key(id))
            .collect();
        // Cells reserve compilation generations before execution. A result can
        // therefore complete after pages compiled with newer generations; only
        // save order expresses which observations are recent.
        let order = self.next_observation_order;
        self.next_observation_order = order.checked_add(1).expect("observation order exhausted");
        self.insert_observation(
            id,
            ObservationBinding {
                owner: scope,
                dependencies,
                recent: Some(order),
                retain_while_current: false,
            },
        );
        let mut recent: Vec<_> = self
            .observations
            .iter()
            .filter_map(|(id, observation)| {
                let order = observation.recent?;
                self.live
                    .get(id)
                    .filter(|entry| entry.scope == scope)
                    .map(|_| (order, *id))
            })
            .collect();
        recent.sort_by_key(|(order, _)| *order);
        let expired = recent.len().saturating_sub(limit);
        for (_, id) in recent.into_iter().take(expired) {
            #[allow(
                clippy::expect_used,
                reason = "recent contains existing observation IDs"
            )]
            let observation = self
                .observations
                .get_mut(&id)
                .expect("existing observation");
            observation.recent = None;
            if let Some(entry) = self.live.get(&id) {
                let scope = entry.scope;
                let name = entry.name.clone();
                if let Some(frame) = self.current.get_mut(&entry.scope) {
                    if frame.get(&entry.name) == Some(&id) {
                        frame.remove(&entry.name);
                    }
                }
                self.refresh_capturable_name(scope, &name);
            }
        }
        self.collect_observations()
    }

    /// Explicit persistent code captures its referenced observations under the
    /// ordinary binding lifetime, including their compiled slot dependencies.
    pub fn preserve_observations(&mut self, referenced: &[VarId]) {
        let mut pending: Vec<_> = referenced
            .iter()
            .copied()
            .map(SessionVarId::from_var)
            .collect();
        while let Some(id) = pending.pop() {
            if let Some(observation) = self.remove_observation(id) {
                pending.extend(observation.dependencies);
            }
        }
    }

    /// Collect expired automatic roots only after accounting for compiled
    /// observation dependencies and immutable fork views.
    pub fn collect_observations(&mut self) -> Vec<BindingEntry> {
        let stale = self
            .observations
            .keys()
            .filter(|id| !self.live.contains_key(id))
            .copied()
            .collect::<Vec<_>>();
        for id in stale {
            self.remove_observation(id);
        }
        let mut pending: Vec<_> = self
            .observations
            .iter()
            .filter(|(id, observation)| {
                observation.recent.is_some()
                    || self.leases.get(id).copied().unwrap_or(0) > 0
                    || (observation.retain_while_current
                        && self.live.get(id).is_some_and(|entry| {
                            self.current
                                .get(&entry.scope)
                                .and_then(|frame| frame.get(&entry.name))
                                == Some(id)
                        }))
            })
            .map(|(id, _)| *id)
            .collect();
        let mut retained = HashSet::new();
        while let Some(id) = pending.pop() {
            if retained.insert(id) {
                if let Some(observation) = self.observations.get(&id) {
                    pending.extend(observation.dependencies.iter().copied());
                }
            }
        }
        let expired: Vec<_> = self
            .observations
            .keys()
            .filter(|id| !retained.contains(id))
            .copied()
            .collect();
        let mut released = Vec::new();
        for id in expired {
            self.remove_observation(id);
            if let Some(entry) = self.remove_live(id) {
                released.push(entry);
            }
        }
        released
    }

    /// Create an empty binding table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Admit the one machine root for a newly materialized source binder.
    /// A second lexical installation has a different GroupInstanceId even if
    /// its immutable code key and source binder are identical.
    pub fn register_source_instance_in(
        &mut self,
        tree: &ScopeTree,
        scope: ScopeId,
        token: SourceInstanceLease,
    ) -> Result<SourceLeaseKey, SourceInstanceLease> {
        let domain = self
            .source_selection
            .get(&scope)
            .map(|view| view.current)
            .unwrap_or_else(|| crate::prepared_program::SourceInstanceDomain::for_scope(scope));
        self.register_source_instance_in_domain(tree, scope, domain, token)
    }

    fn register_source_instance_in_domain(
        &mut self,
        tree: &ScopeTree,
        scope: ScopeId,
        domain: crate::prepared_program::SourceInstanceDomain,
        token: SourceInstanceLease,
    ) -> Result<SourceLeaseKey, SourceInstanceLease> {
        if !tree.is_live(scope) {
            return Err(token);
        }
        let key = SourceLeaseKey::of(&token);
        if self.source_instances.contains_key(&key) {
            return Err(token);
        }
        self.source_instances.insert(
            key.clone(),
            ScopedSourceLease {
                owner: scope,
                token,
                shares: 0,
                owner_retired: false,
            },
        );
        self.source_owned
            .entry(scope)
            .or_default()
            .insert(key.clone());
        self.add_capturable_sources(scope, [key.clone()]);
        let selected = self
            .source_selection
            .entry(scope)
            .or_insert_with(|| SourceSelectionView::for_scope(scope));
        selected.add(domain, key.clone());
        self.changed(scope);
        Ok(key)
    }

    /// Admit one native batch without leaving a partial lexical installation
    /// if a scope retired or one exact group instance was already registered.
    pub fn register_source_instances_in(
        &mut self,
        tree: &ScopeTree,
        scope: ScopeId,
        tokens: Vec<SourceInstanceLease>,
    ) -> Result<Vec<SourceLeaseKey>, Vec<SourceInstanceLease>> {
        let mut seen = HashSet::new();
        if !tree.is_live(scope)
            || tokens.iter().any(|token| {
                let key = SourceLeaseKey::of(token);
                !seen.insert(key.clone()) || self.source_instances.contains_key(&key)
            })
        {
            return Err(tokens);
        }
        Ok(tokens
            .into_iter()
            .map(|token| {
                self.register_source_instance_in(tree, scope, token)
                    .expect("source batch was prevalidated under one checkout")
            })
            .collect())
    }

    /// Admit a domain-qualified native batch before committing its machine pins.
    /// Domains must already belong to this captured view and every physical root
    /// must be new; independent mutable roots are never chosen by code identity.
    pub fn register_source_instances_in_domains(
        &mut self,
        tree: &ScopeTree,
        scope: ScopeId,
        tokens: Vec<crate::prepared_program::SourceInstanceAttachment>,
        machine: &crate::prepared_program::PreparedMachine<'_>,
    ) -> Result<SourceScopeAdmission, Vec<crate::prepared_program::SourceInstanceAttachment>> {
        let empty = SourceSelectionView::for_scope(scope);
        let view = self.source_selection.get(&scope).unwrap_or(&empty);
        let mut physical = HashMap::<SourceLeaseKey, SourceInstanceLease>::new();
        let mut entries = Vec::new();
        for (domain, record) in view.domains.iter() {
            for key in record.selected.iter() {
                let Some(existing) = self.source_instances.get(key) else {
                    return Err(tokens);
                };
                entries.push((*domain, existing.token.clone()));
            }
        }
        let mut installs = HashSet::new();
        for attachment in &tokens {
            if machine.validate_source_attachment(attachment).is_err() {
                return Err(tokens);
            }
            let domain = attachment.domain();
            let token = attachment.lease();
            if !tree.is_live(scope) || !view.domains.contains_key(&domain) {
                return Err(tokens);
            }
            let key = SourceLeaseKey::of(token);
            if let Some(demand) = attachment.demand() {
                let anchor_key = SourceLeaseKey::of(demand.anchor());
                let selected = view
                    .domains
                    .get(&domain)
                    .is_some_and(|record| record.selected.contains(&anchor_key));
                if !selected
                    || self
                        .source_instances
                        .get(&anchor_key)
                        .is_none_or(|anchor| !same_source_lease(&anchor.token, demand.anchor()))
                {
                    return Err(tokens);
                }
            } else if self.source_instances.contains_key(&key) || !installs.insert(key.clone()) {
                return Err(tokens);
            }
            let existing = physical
                .get(&key)
                .or_else(|| self.source_instances.get(&key).map(|record| &record.token));
            if existing.is_some_and(|old| !same_source_lease(old, token)) {
                return Err(tokens);
            }
            physical.insert(key, token.clone());
            entries.push((domain, token.clone()));
        }
        let transitions = view
            .domains
            .iter()
            .map(|(domain, record)| {
                (
                    *domain,
                    record
                        .authored
                        .iter()
                        .map(|(owner, target)| (owner.clone(), *target))
                        .collect(),
                )
            })
            .collect();
        if crate::prepared_program::SourceDomainSelection::new(view.current, transitions, entries)
            .is_err()
        {
            return Err(tokens);
        }
        let mut admission = SourceScopeAdmission {
            scope: Some(scope),
            ..Default::default()
        };
        let custody = self.source_instance_keys_in(tree, scope);
        let mut attached = HashSet::new();
        for attachment in tokens {
            let (domain, token) = attachment.into_parts();
            let key = SourceLeaseKey::of(&token);
            let selected = self
                .source_selection
                .get(&scope)
                .and_then(|view| view.domains.get(&domain))
                .is_some_and(|record| record.selected.contains(&key));
            if !selected {
                admission.selected.push((domain, key.clone()));
            }
            if !admission.keys.contains(&key) && !selected {
                admission.keys.push(key.clone());
            }
            if self.source_instances.contains_key(&key) {
                if !custody.contains(&key) && attached.insert(key.clone()) {
                    admission.shares.insert(key.clone());
                    self.promoted_source_instances
                        .entry(scope)
                        .or_default()
                        .insert(key.clone());
                    self.add_capturable_sources(scope, [key.clone()]);
                    self.acquire_source_shares(&HashSet::from([key.clone()]));
                }
                self.source_selection
                    .entry(scope)
                    .or_insert_with(|| SourceSelectionView::for_scope(scope))
                    .add(domain, key);
            } else {
                admission.owned.push(
                    self.register_source_instance_in_domain(tree, scope, domain, token)
                        .expect("qualified batch prevalidated"),
                );
            }
        }
        self.changed(scope);
        Ok(admission)
    }

    pub fn register_source_install_in(
        &mut self,
        tree: &ScopeTree,
        scope: ScopeId,
        tokens: Vec<SourceInstanceLease>,
    ) -> Result<SourceScopeAdmission, Vec<SourceInstanceLease>> {
        let keys = self.register_source_instances_in(tree, scope, tokens)?;
        Ok(SourceScopeAdmission {
            scope: Some(scope),
            owned: keys.clone(),
            keys,
            ..Default::default()
        })
    }

    pub fn rollback_source_admission(
        &mut self,
        scope: ScopeId,
        admission: &SourceScopeAdmission,
    ) -> Option<Vec<SourceInstanceLease>> {
        if admission.scope != Some(scope)
            || admission
                .rolled_back
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            return None;
        }
        if admission.owned.iter().any(|key| {
            self.source_instances
                .get(key)
                .is_none_or(|lease| lease.owner != scope || lease.owner_retired)
        }) || admission.shares.iter().any(|key| {
            !self
                .promoted_source_instances
                .get(&scope)
                .is_some_and(|keys| keys.contains(key))
        }) {
            return None;
        }
        if admission.selected.iter().any(|(domain, key)| {
            !self
                .source_selection
                .get(&scope)
                .and_then(|view| view.domains.get(domain))
                .is_some_and(|record| record.selected.contains(key))
        }) {
            return None;
        }
        admission
            .rolled_back
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(view) = self.source_selection.get_mut(&scope) {
            for (domain, key) in &admission.selected {
                let mut record = view.domains.get(domain)?.clone();
                record.selected.remove_mut(key);
                view.domains.insert_mut(*domain, record);
            }
        }
        for key in &admission.shares {
            self.promoted_source_instances
                .get_mut(&scope)
                .expect("admission shares owned")
                .remove(key);
            self.remove_capturable_source(scope, key);
        }
        let mut released = self.release_source_shares(admission.shares.clone());
        released.extend(self.retire_source_instances_in(scope, &admission.owned)?);
        self.changed(scope);
        Some(released)
    }

    /// An exact existing sibling root can be attached to a second captured
    /// domain only through its already selected physical group anchor.
    pub fn retained_source_sibling_attachment(
        &self,
        demand: &crate::prepared_program::InheritedSourceDemand,
    ) -> Option<crate::prepared_program::SourceInstanceAttachment> {
        let anchor = self
            .source_instances
            .get(&SourceLeaseKey::of(demand.anchor()))?;
        if !same_source_lease(&anchor.token, demand.anchor()) {
            return None;
        }
        let key = SourceLeaseKey {
            instance: demand.anchor().instance(),
            binder: Arc::new(demand.binder().clone()),
        };
        let token = &self.source_instances.get(&key)?.token;
        (token.owner() == demand.owner()
            && token.original_ordinal() == demand.original_ordinal()
            && token.value() == demand.value())
        .then(|| {
            crate::prepared_program::SourceInstanceAttachment::inherited(demand, token.clone()).ok()
        })
        .flatten()
    }

    /// Retire only the newly registered roots of one failed turn. All keys
    /// must still belong to that lexical owner; a shared capture keeps its
    /// exact instance rooted until the last share releases.
    pub fn retire_source_instances_in(
        &mut self,
        scope: ScopeId,
        keys: &[SourceLeaseKey],
    ) -> Option<Vec<SourceInstanceLease>> {
        let mut seen = HashSet::new();
        if keys.iter().any(|key| {
            !seen.insert(key.clone())
                || self
                    .source_instances
                    .get(key)
                    .is_none_or(|lease| lease.owner != scope || lease.owner_retired)
        }) {
            return None;
        }
        let mut released = Vec::new();
        for key in keys {
            let lease = self
                .source_instances
                .get_mut(key)
                .expect("source owner batch was prevalidated");
            lease.owner_retired = true;
            let unshared = lease.shares == 0;
            self.remove_capturable_source(scope, key);
            if let Some(selected) = self.source_selection.get_mut(&scope) {
                let domains: Vec<_> = selected.domains.keys().copied().collect();
                for domain in domains {
                    let mut record = selected
                        .domains
                        .get(&domain)
                        .cloned()
                        .expect("selected domain exists");
                    record.selected.remove_mut(key);
                    selected.domains.insert_mut(domain, record);
                }
            }
            self.changed(scope);
            if unshared {
                released.push(
                    self.remove_source(key)
                        .expect("unshared source owner was prevalidated")
                        .token,
                );
            }
        }
        Some(released)
    }

    /// All exact source instances retained by this scope, including dependencies
    /// of independently published values. This is custody, not lexical selection.
    /// Returned descriptors share machine handles; callers never release them.
    #[must_use]
    pub fn source_instances_in(
        &self,
        tree: &ScopeTree,
        scope: ScopeId,
    ) -> Vec<SourceInstanceLease> {
        let mut keys: Vec<_> = self
            .source_instance_keys_in(tree, scope)
            .into_iter()
            .collect();
        keys.sort_by(|left, right| {
            left.instance
                .cmp(&right.instance)
                .then_with(|| left.binder.cmp(&right.binder))
        });
        keys.into_iter()
            .map(|key| {
                self.source_instances
                    .get(&key)
                    .expect("visible source instance retains its machine root")
                    .token
                    .clone()
            })
            .collect()
    }

    /// Exact native instances selected by installation or frozen lexical capture.
    /// Published value dependencies remain rooted separately and cannot choose a
    /// mutable instance for an unrelated execution's source imports.
    #[must_use]
    pub fn selected_source_instances_in(
        &self,
        tree: &ScopeTree,
        scope: ScopeId,
    ) -> Vec<SourceInstanceLease> {
        let mut keys: Vec<_> = self
            .selected_source_keys_in(tree, scope)
            .iter()
            .cloned()
            .collect();
        keys.sort();
        keys.into_iter()
            .map(|key| {
                self.source_instances
                    .get(&key)
                    .expect("selected source retains exact custody")
                    .token
                    .clone()
            })
            .collect()
    }

    fn selected_source_keys_in(
        &self,
        tree: &ScopeTree,
        scope: ScopeId,
    ) -> HashTrieSetSync<SourceLeaseKey> {
        if !tree.is_live(scope) {
            return HashTrieSetSync::new_sync();
        }
        let owners = if self.tips.contains_key(&scope) || tree.parent_of(scope).is_none() {
            vec![scope]
        } else {
            tree.lookup_chain(scope)
        };
        owners
            .into_iter()
            .flat_map(|owner| {
                self.source_selection
                    .get(&owner)
                    .into_iter()
                    .flat_map(|view| view.domains.values())
                    .flat_map(|record| record.selected.iter().cloned())
            })
            .collect()
    }

    /// Freeze exact original-owner origins and their retained mutable choices.
    /// Every returned lease is backed by this scope's current physical custody.
    pub fn source_domain_selection_in(
        &self,
        tree: &ScopeTree,
        scope: ScopeId,
    ) -> Result<crate::prepared_program::SourceDomainSelection, BindingPromotionError> {
        if self.scope_witness(tree, scope).is_none() {
            return Err(BindingPromotionError::UnverifiableSourceSelection);
        }
        let empty = SourceSelectionView::for_scope(scope);
        let view = self.source_selection.get(&scope).unwrap_or(&empty);
        let custody = self.source_instance_keys_in(tree, scope);
        let mut entries = Vec::new();
        for (domain, record) in view.domains.iter() {
            for key in record.selected.iter() {
                if !custody.contains(key) {
                    return Err(BindingPromotionError::MissingOrForeignSourceInstance);
                }
                let lease = self
                    .source_instances
                    .get(key)
                    .ok_or(BindingPromotionError::MissingOrForeignSourceInstance)?;
                entries.push((*domain, lease.token.clone()));
            }
        }
        crate::prepared_program::SourceDomainSelection::new(
            view.current,
            view.domains
                .iter()
                .map(|(domain, record)| {
                    (
                        *domain,
                        record
                            .authored
                            .iter()
                            .map(|(owner, target)| (owner.clone(), *target))
                            .collect(),
                    )
                })
                .collect(),
            entries,
        )
        .map_err(|_| BindingPromotionError::ConflictingSourceOrigin)
    }

    pub fn prepare_source_owner_origin_in(
        &self,
        tree: &ScopeTree,
        scope: ScopeId,
        owner: tidepool_repr::execution_schema::CachedHomeOwner,
    ) -> Result<PreparedSourceOwnerOrigin, BindingPromotionError> {
        if self.scope_witness(tree, scope).is_none() {
            return Err(BindingPromotionError::UnverifiableSourceSelection);
        }
        let empty = SourceSelectionView::for_scope(scope);
        let view = self.source_selection.get(&scope).unwrap_or(&empty);
        if view
            .domains
            .get(&view.current)
            .ok_or(BindingPromotionError::MissingSourceDomain)?
            .authored
            .get(&owner)
            .is_some_and(|domain| *domain != view.current)
        {
            return Err(BindingPromotionError::ConflictingSourceOrigin);
        }
        Ok(PreparedSourceOwnerOrigin {
            scope,
            owner,
            domain: view.current,
        })
    }

    pub fn commit_source_owner_origin(&mut self, prepared: PreparedSourceOwnerOrigin) {
        let view = self
            .source_selection
            .entry(prepared.scope)
            .or_insert_with(|| SourceSelectionView::for_scope(prepared.scope));
        let mut record = view
            .domains
            .get(&view.current)
            .cloned()
            .expect("origin domain admitted");
        record.authored.insert_mut(prepared.owner, prepared.domain);
        view.domains.insert_mut(view.current, record);
        self.changed(prepared.scope);
    }

    /// Exact materialized instances visible at one frozen lexical scope.
    pub fn source_instance_keys_in(
        &self,
        tree: &ScopeTree,
        scope: ScopeId,
    ) -> HashSet<SourceLeaseKey> {
        if tree.is_live(scope)
            && (self.tips.contains_key(&scope) || tree.parent_of(scope).is_none())
        {
            if let Some(projected) = self.capturable_membership.get(&scope) {
                return projected.source_instances.iter().cloned().collect();
            }
        }
        self.source_instance_keys_in_slow(tree, scope)
    }

    fn source_instance_keys_in_slow(
        &self,
        tree: &ScopeTree,
        scope: ScopeId,
    ) -> HashSet<SourceLeaseKey> {
        if !tree.is_live(scope) {
            return HashSet::new();
        }
        let mut keys = self
            .tips
            .get(&scope)
            .map(|tip| tip.source_instances.iter().cloned().collect::<HashSet<_>>())
            .unwrap_or_default();
        let owners = if self.tips.contains_key(&scope) {
            vec![scope]
        } else {
            tree.lookup_chain(scope)
        };
        keys.extend(
            owners
                .iter()
                .flat_map(|owner| {
                    self.source_owned
                        .get(owner)
                        .into_iter()
                        .flat_map(HashSet::iter)
                })
                .filter(|key| {
                    self.source_instances
                        .get(*key)
                        .is_some_and(|lease| !lease.owner_retired)
                })
                .cloned(),
        );
        for owner in owners {
            if let Some(promoted) = self.promoted_source_instances.get(&owner) {
                keys.extend(promoted.iter().cloned());
            }
        }
        keys
    }

    fn acquire_source_shares(&mut self, keys: &HashSet<SourceLeaseKey>) {
        for key in keys {
            let lease = self
                .source_instances
                .get_mut(key)
                .expect("source lease key was prevalidated");
            lease.shares = lease
                .shares
                .checked_add(1)
                .expect("source lease share count exhausted");
        }
    }

    fn release_source_shares(&mut self, keys: HashSet<SourceLeaseKey>) -> Vec<SourceInstanceLease> {
        let mut released = Vec::new();
        for key in keys {
            let lease = self
                .source_instances
                .get_mut(&key)
                .expect("source lease share exists");
            assert!(lease.shares > 0, "source lease share underflow");
            lease.shares -= 1;
            if lease.shares == 0 && lease.owner_retired {
                released.push(
                    self.remove_source(&key)
                        .expect("retired source lease exists")
                        .token,
                );
            }
        }
        released
    }

    /// Record a (re)bind. Repoints `current[name]` to the fresh id and inserts
    /// the entry into `live`; any prior entry for the same name stays in `live`
    /// under its own (older) id. Returns the bound id.
    ///
    pub fn bind(&mut self, entry: BindingEntry) -> Result<SessionVarId, BindingIdentityError> {
        self.bind_in(ScopeId::ROOT, entry)
    }

    /// Scoped [`Self::bind`]: write ONLY `scope`'s frame. `bind(e) ==
    /// bind_in(ScopeId::ROOT, e)`.
    ///
    /// Nothing walks downward, so a child bind is invisible to the parent
    /// (the parent never gains child declarations by name — this holds by
    /// representation, not by a check), and sibling frames are disjoint
    /// maps, so two siblings binding the same name never collide.
    ///
    pub fn bind_in(
        &mut self,
        scope: ScopeId,
        mut entry: BindingEntry,
    ) -> Result<SessionVarId, BindingIdentityError> {
        self.validate_bind_in(scope, &entry)?;
        if self.live.contains_key(&entry.id) {
            return Ok(entry.id);
        }
        entry.scope = scope;
        let id = entry.id;
        let name = entry.name.clone();
        // NEWEST GEN WINS BY COMPARISON, not by insertion order: with
        // With any-order resume, a bind minted at gen 6 can
        // MATERIALIZE after a same-name bind minted at gen 7 — arrival order
        // no longer implies gen order. `current` must track the highest-gen
        // binding for the name; an out-of-order older materialization stays
        // `live` (fragments compiled against it still resolve) but never
        // shadows a newer one. Unchanged by C2 — it now applies WITHIN a
        // frame, comparing only against this scope's own current binding.
        let newer_current_exists = self
            .current
            .get(&scope)
            .and_then(|frame| frame.get(&entry.name))
            .and_then(|cur_id| self.live.get(cur_id))
            .is_some_and(|cur| cur.module.gen().0 > entry.module.gen().0);
        if !newer_current_exists {
            if let Some(hidden) = self.hidden.get_mut(&scope) {
                hidden.remove(&entry.name);
            }
            self.current
                .entry(scope)
                .or_default()
                .insert(entry.name.clone(), id);
        }
        self.owned.entry(scope).or_default().insert(id);
        self.add_capturable_bindings(scope, [id]);
        *self.modules.entry(entry.module).or_default() += 1;
        self.live.insert(id, entry);
        self.refresh_capturable_name(scope, &name);
        self.changed(scope);
        Ok(id)
    }

    /// Make exact completed private bindings visible in `target` without
    /// cloning roots or selecting winners by compile-generation order. The
    /// caller validates both live scopes and declaration compatibility before
    /// this infallible visibility change. Every requested name is validated
    /// first, so an invalid batch leaves the target frame untouched.
    pub fn promote_exact_bindings_in(
        &mut self,
        source: ScopeId,
        target: ScopeId,
        ids: &[SessionVarId],
    ) -> Result<(), BindingPromotionError> {
        let prepared = self.prepare_exact_binding_promotion_in(source, target, ids)?;
        self.commit_exact_binding_promotion(prepared);
        Ok(())
    }

    /// Check every source owner and final name without changing visibility or
    /// acquiring a lease. Publication can then perform its durable work before
    /// the infallible commit, while retaining the same machine checkout.
    pub fn prepare_exact_binding_promotion_in(
        &self,
        source: ScopeId,
        target: ScopeId,
        ids: &[SessionVarId],
    ) -> Result<PreparedBindingPromotion, BindingPromotionError> {
        let mut names = HashSet::new();
        let mut writes = Vec::with_capacity(ids.len());
        for &id in ids {
            let entry = self
                .live
                .get(&id)
                .filter(|entry| entry.scope == source)
                .ok_or(BindingPromotionError::MissingOrForeignBinding)?;
            if self
                .current
                .get(&source)
                .and_then(|frame| frame.get(&entry.name))
                != Some(&id)
            {
                return Err(BindingPromotionError::NotCurrentInSource);
            }
            if !names.insert(entry.name.clone()) {
                return Err(BindingPromotionError::DuplicateName);
            }
            writes.push((entry.name.clone(), id));
        }
        let retained = self.dependency_closure(ids.iter().copied());
        Ok(PreparedBindingPromotion {
            target,
            writes,
            retained,
            source_instances: HashSet::new(),
            source_origins: HashMap::new(),
            source_domains: HashMap::new(),
            next_tip: None,
        })
    }

    /// Check binding writes and materialized source instances against one
    /// private lexical view before the durable publication decision.
    pub fn prepare_exact_publication_in(
        &self,
        tree: &ScopeTree,
        source: ScopeId,
        target: ScopeId,
        ids: &[SessionVarId],
        instances: &[SourceLeaseKey],
    ) -> Result<PreparedBindingPromotion, BindingPromotionError> {
        if !tree.is_live(source) || !tree.is_live(target) {
            return Err(BindingPromotionError::MissingOrForeignBinding);
        }
        let mut prepared = self.prepare_exact_binding_promotion_in(source, target, ids)?;
        let source_keys = self.source_instance_keys_in(tree, source);
        let target_keys = self.source_instance_keys_in(tree, target);
        for key in instances {
            if !source_keys.contains(key) {
                return Err(BindingPromotionError::MissingOrForeignSourceInstance);
            }
            if !target_keys.contains(key) {
                prepared.source_instances.insert(key.clone());
            }
        }
        Ok(prepared)
    }

    /// Authored declaration publication preserves the originating private
    /// lexical choices within the exact custody batch. Dependencies that are
    /// only retained by that view grant no selection; the installer refuses
    /// conflicting selected domains rather than choosing a mutable instance.
    pub fn prepare_authored_source_publication_in(
        &self,
        tree: &ScopeTree,
        source: ScopeId,
        target: ScopeId,
        ids: &[SessionVarId],
        instances: &[SourceLeaseKey],
        accepted_owners: &[tidepool_repr::execution_schema::CachedHomeOwner],
        binding_dependencies: &[SessionVarId],
    ) -> Result<PreparedBindingPromotion, BindingPromotionError> {
        let mut prepared =
            self.prepare_exact_publication_in(tree, source, target, ids, instances)?;
        let reachable = self.scope_dependency_ids(tree, source);
        if binding_dependencies
            .iter()
            .any(|id| !reachable.contains(id) || !self.live.contains_key(id))
        {
            return Err(BindingPromotionError::MissingOrForeignBinding);
        }
        // Authored full-body custody keeps exact hidden values and their
        // observation dependencies without adding names to the visible writes.
        prepared
            .retained
            .extend(self.dependency_closure(binding_dependencies.iter().copied()));
        let source_view = self
            .source_selection
            .get(&source)
            .ok_or(BindingPromotionError::MissingSourceDomain)?;
        let current = source_view
            .domains
            .get(&source_view.current)
            .ok_or(BindingPromotionError::MissingSourceDomain)?;
        let roots = accepted_owners
            .iter()
            .map(|owner| {
                current
                    .authored
                    .get(owner)
                    .copied()
                    .ok_or(BindingPromotionError::MissingSourceDomain)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let target_view = self.source_selection.get(&target);
        let target_record = target_view.and_then(|view| view.domains.get(&view.current));
        if accepted_owners
            .iter()
            .any(|owner| target_record.is_some_and(|record| record.authored.contains_key(owner)))
        {
            return Err(BindingPromotionError::ConflictingSourceOrigin);
        }
        let next_tip = self
            .next_tip
            .checked_add(1)
            .ok_or(BindingPromotionError::UnverifiableSourceSelection)?;
        let (remap, records) = source_view.fork(BindingTipId(self.next_tip), roots)?;
        let custody = self.source_instance_keys_in(tree, source);
        let target_custody = self.source_instance_keys_in(tree, target);
        for record in records.values() {
            for key in record.selected.iter() {
                if !custody.contains(key) || !self.source_instances.contains_key(key) {
                    return Err(BindingPromotionError::MissingOrForeignSourceInstance);
                }
                if !target_custody.contains(key) {
                    prepared.source_instances.insert(key.clone());
                }
            }
        }
        for owner in accepted_owners {
            prepared
                .source_origins
                .insert(owner.clone(), remap[&current.authored[owner]]);
        }
        prepared.source_domains = records;
        prepared.next_tip = Some(next_tip);
        Ok(prepared)
    }

    /// The checkout that prepared this promotion must still own the table;
    /// there is no fallible operation after the public durability boundary.
    pub fn commit_exact_binding_promotion(&mut self, prepared: PreparedBindingPromotion) {
        let PreparedBindingPromotion {
            target,
            writes,
            retained,
            source_instances,
            source_origins,
            source_domains,
            next_tip,
        } = prepared;
        let existing = self.promoted.entry(target).or_default();
        let added: HashSet<_> = retained.difference(existing).copied().collect();
        existing.extend(added.iter().copied());
        self.add_capturable_bindings(target, added.iter().copied());
        self.lease_exact_ids(&added);
        let existing_source = self.promoted_source_instances.entry(target).or_default();
        let added_source: HashSet<_> = source_instances
            .difference(existing_source)
            .cloned()
            .collect();
        existing_source.extend(added_source.iter().cloned());
        self.add_capturable_sources(target, added_source.iter().cloned());
        self.acquire_source_shares(&added_source);
        let selected = self
            .source_selection
            .entry(target)
            .or_insert_with(|| SourceSelectionView::for_scope(target));
        for (domain, record) in source_domains {
            selected.domains.insert_mut(domain, record);
        }
        let mut record = selected
            .domains
            .get(&selected.current)
            .cloned()
            .expect("current domain admitted");
        for (owner, domain) in source_origins {
            record.authored.insert_mut(owner, domain);
        }
        selected.domains.insert_mut(selected.current, record);
        if let Some(next_tip) = next_tip {
            self.next_tip = next_tip;
        }
        for (name, id) in writes {
            if let Some(hidden) = self.hidden.get_mut(&target) {
                hidden.remove(&name);
            }
            self.current
                .entry(target)
                .or_default()
                .insert(name.clone(), id);
            self.refresh_capturable_name(target, &name);
        }
        self.changed(target);
    }

    /// Publish an alias of an existing root through the ordinary scoped name
    /// map. Its source remains live while the alias is current, captured by
    /// another observation, or externally leased. Replacing the alias lets
    /// ordinary observation collection release the old entry and its source
    /// when nothing else reaches them. Both entries share one registered slot.
    pub fn bind_alias_in(
        &mut self,
        scope: ScopeId,
        entry: BindingEntry,
        source: SessionVarId,
    ) -> Option<(SessionVarId, Vec<BindingEntry>)> {
        if self
            .live
            .get(&source)
            .is_none_or(|source_entry| source_entry.scope != scope)
            || self.live.contains_key(&entry.id)
        {
            return None;
        }
        let id = self.bind_in(scope, entry).ok()?;
        self.scope_local_aliases.insert(id);
        let name = self.live[&id].name.clone();
        self.refresh_capturable_name(scope, &name);
        self.insert_observation(
            id,
            ObservationBinding {
                owner: scope,
                dependencies: vec![source],
                recent: None,
                retain_while_current: true,
            },
        );
        Some((id, self.collect_observations()))
    }

    /// Drop `name` from the ROOT frame so `iter_current`/`resolve` no longer
    /// see it (its `live` entry + root are retained for fragments compiled
    /// against the old gen). Used when a pure decl of the same name supersedes
    /// a materialized value binding (cross-store shadow, GHCi-environment
    /// model). No-op if `name` isn't current.
    pub fn remove_current(&mut self, name: &str) {
        self.remove_current_in(ScopeId::ROOT, name);
    }

    /// Scoped [`Self::remove_current`]: remove `name` from this scope's frame
    /// only.  A declaration committed in a child scope must not make an
    /// ancestor's materialized binding disappear from the ancestor's view.
    pub fn remove_current_in(&mut self, scope: ScopeId, name: &str) {
        let name = BindingName(name.to_string());
        let mut changed = false;
        if let Some(frame) = self.current.get_mut(&scope) {
            changed |= frame.remove(&name).is_some();
        }
        if self
            .tips
            .get(&scope)
            .is_some_and(|tip| tip.visible.contains_key(&name))
        {
            changed |= self.hidden.entry(scope).or_default().insert(name.clone());
        }
        if changed {
            self.refresh_capturable_name(scope, &name);
            self.changed(scope);
        }
    }

    /// Evict one binding from `live` ENTIRELY — the primitive scope
    /// retirement is built from, and the only thing that ever removes a `live`
    /// entry ([`Self::remove_current`] merely un-shadows one).
    ///
    /// PURE BOOKKEEPING: it removes the entry from `live`, and clears the
    /// owning frame's `name → id` mapping if (and only if) that frame still
    /// points at this id. It does not touch the machine. The session owner
    /// releases the retained handle after its final alias
    /// leaves the store.
    ///
    /// Returns the evicted entry with its retained handle, or
    /// `None` if `id` was not live.
    pub fn remove_live(&mut self, id: SessionVarId) -> Option<BindingEntry> {
        if self.leases.get(&id).copied().unwrap_or(0) > 0 {
            return None;
        }
        let entry = self.remove_entry(id)?;
        self.retired_owners.remove(&id);
        if let Some(frame) = self.current.get_mut(&entry.scope) {
            // Only if the frame still names THIS id: a newer same-name gen in
            // the same frame has already repointed it, and a shadowed older
            // gen must not resurrect by removal of its shadower.
            if frame.get(&entry.name) == Some(&id) {
                frame.remove(&entry.name);
            }
        }
        self.scope_local_aliases.remove(&id);
        self.refresh_capturable_name(entry.scope, &entry.name);
        Some(entry)
    }

    /// Retire one exact owner without touching older captured generations.
    /// A leased entry leaves the current frame now and is released after its
    /// final lease; an unleased entry is returned to the session owner now.
    pub fn retire_owner(&mut self, id: SessionVarId) -> Option<BindingEntry> {
        let entry = self.live.get(&id)?;
        let scope = entry.scope;
        let name = entry.name.clone();
        let mut changed = false;
        if let Some(frame) = self.current.get_mut(&entry.scope) {
            if frame.get(&entry.name) == Some(&id) {
                frame.remove(&entry.name);
                changed = true;
            }
        }
        self.refresh_capturable_name(scope, &name);
        if self.leases.get(&id).copied().unwrap_or(0) > 0 {
            changed |= self.retired_owners.insert(id);
            if changed {
                self.changed(scope);
            }
            None
        } else {
            let removed = self.remove_entry(id);
            if removed.is_some() {
                self.retired_owners.remove(&id);
                self.scope_local_aliases.remove(&id);
            }
            removed
        }
    }

    /// Evict every binding born in `scope`'s OWN frame (shadowed older gens
    /// included) and drop the frame itself. Returns the evicted entries, in
    /// ascending id order for determinism.
    ///
    /// Retirement is BY SCOPE, never by shadowing: this drains one frame
    /// wholesale and leaves every other frame — parent, sibling, child —
    /// untouched. Descendant frames are the caller's job, in the deepest-first
    /// order [`ScopeTree::retire`] hands back.
    pub fn drain_scope(&mut self, scope: ScopeId) -> Vec<BindingEntry> {
        assert!(
            self.source_owned.get(&scope).is_none_or(HashSet::is_empty)
                && self
                    .tips
                    .get(&scope)
                    .is_none_or(|tip| tip.source_instances.is_empty())
                && self
                    .promoted_source_instances
                    .get(&scope)
                    .is_none_or(HashSet::is_empty),
            "source roots require drain_scope_with_sources"
        );
        self.drain_scope_with_sources(scope).bindings
    }

    /// Retire one scope's binding and materialized source ownership together.
    /// The session owner releases returned machine handles under checkout.
    pub fn drain_scope_with_sources(&mut self, scope: ScopeId) -> ScopeDrain {
        self.capture_history.remove(&scope);
        self.source_selection.remove(&scope);
        self.capturable_membership.remove(&scope);
        let (mut released, mut source_instances) = self.release_tip(scope);
        if let Some(promoted) = self.promoted.remove(&scope) {
            released.extend(self.release_leases(promoted));
        }
        if let Some(promoted) = self.promoted_source_instances.remove(&scope) {
            source_instances.extend(self.release_source_shares(promoted));
        }
        let mut ids: Vec<SessionVarId> = self
            .owned
            .get(&scope)
            .into_iter()
            .flat_map(HashSet::iter)
            .copied()
            .collect();
        ids.sort_by_key(|id| id.raw());
        self.current.remove(&scope);
        self.capturable_names.remove(&scope);
        self.hidden.remove(&scope);
        self.changed(scope);
        for id in ids {
            if self.leases.get(&id).copied().unwrap_or(0) > 0 {
                self.retired_owners.insert(id);
            } else if let Some(entry) = self.remove_entry(id) {
                self.scope_local_aliases.remove(&id);
                released.push(entry);
            }
        }
        let owned: Vec<_> = self
            .source_owned
            .get(&scope)
            .into_iter()
            .flat_map(HashSet::iter)
            .cloned()
            .collect();
        for key in owned {
            let lease = self
                .source_instances
                .get_mut(&key)
                .expect("owned source lease exists");
            lease.owner_retired = true;
            if lease.shares == 0 {
                source_instances.push(
                    self.remove_source(&key)
                        .expect("retired source lease exists")
                        .token,
                );
            }
        }
        released.sort_by_key(|entry| entry.id.raw());
        source_instances.sort_by(|left, right| {
            left.instance()
                .cmp(&right.instance())
                .then_with(|| left.binder().cmp(right.binder()))
        });
        ScopeDrain {
            bindings: released,
            source_instances,
        }
    }

    /// Freeze the value bindings visible from `parent` as `child`'s immutable
    /// inherited view. Immutable chunks share existing physical custody;
    /// each newly captured exact root receives one lease in the new delta.
    pub fn seed_scope(
        &mut self,
        tree: &ScopeTree,
        parent: ScopeId,
        child: ScopeId,
    ) -> BindingTipId {
        if let Some(tip) = self.tips.get(&child) {
            return tip.id;
        }
        let id = BindingTipId(self.next_tip);
        let next_tip = self
            .next_tip
            .checked_add(1)
            .expect("binding tip identity exhausted");
        let visible = if !tree.is_live(parent) {
            HashTrieMapSync::new_sync()
        } else if self.tips.contains_key(&parent) || tree.parent_of(parent).is_none() {
            self.capturable_names
                .get(&parent)
                .cloned()
                .unwrap_or_default()
        } else {
            // Unseeded legacy children still read mutable ancestor frames.
            // Their exact current view cannot reuse an owner-local projection.
            self.iter_current_in(tree, parent)
                .into_iter()
                .filter(|(_, entry)| !self.scope_local_aliases.contains(&entry.id))
                .map(|(name, entry)| (name.clone(), entry.id))
                .collect()
        };
        // Inherited custody is already closed at its capture. Only the
        // parent's own mutable owner dependencies may grow this new capture.
        let parent_view = self
            .source_selection
            .get(&parent)
            .cloned()
            .unwrap_or_else(|| SourceSelectionView::for_scope(parent));
        let (remap, records) = parent_view
            .fork(id, [parent_view.current])
            .expect("captured source domain graph is closed");
        let mut selected = SourceSelectionView {
            current: remap[&parent_view.current],
            domains: records.into_iter().collect(),
        };
        if let Some(previous) = self.source_selection.remove(&child) {
            let mut record = selected
                .domains
                .get(&selected.current)
                .cloned()
                .expect("captured current domain exists");
            let previous_current = previous
                .domains
                .get(&previous.current)
                .expect("preseed current domain exists");
            for key in previous_current.selected.iter() {
                record.selected.insert_mut(key.clone());
            }
            for (owner, domain) in previous_current.authored.iter() {
                record.authored.insert_mut(owner.clone(), *domain);
            }
            for (domain, record) in previous.domains.iter() {
                selected.domains.insert_mut(*domain, record.clone());
            }
            selected.domains.insert_mut(selected.current, record);
        }
        self.source_selection.insert(child, selected);
        let membership = self.capture_membership(tree, parent);
        let leases = self.capture_leases(parent, &membership);
        let CaptureMembership {
            retained,
            source_instances,
            ..
        } = membership;
        self.next_tip = next_tip;
        self.capturable_names.insert(child, visible.clone());
        self.tips.insert(
            child,
            BindingTip {
                id,
                visible,
                retained,
                source_instances,
                leases,
            },
        );
        // Normally a fresh scope has no own entries. Preserve the supported
        // pre-seed owner case without widening its new frozen inheritance.
        let mut child_membership = CapturableMembership {
            retained: self.tips[&child].retained.clone(),
            source_instances: self.tips[&child].source_instances.clone(),
            dependency_ids: HashSet::new(),
            dependency_revision: self.dependency_revision,
            dependencies_dirty: self.observation_owners.contains_key(&child),
            append_epoch: 1,
            added_bindings: HashSet::new(),
            added_sources: HashSet::new(),
        };
        if let Some(previous) = self.capturable_membership.remove(&child) {
            child_membership.dependencies_dirty |= previous.dependencies_dirty;
            child_membership.dependency_ids = previous.dependency_ids;
            for id in previous.retained.iter() {
                child_membership.retained.insert_mut(*id);
            }
            for key in previous.source_instances.iter() {
                child_membership.source_instances.insert_mut(key.clone());
            }
        }
        self.capturable_membership.insert(child, child_membership);
        self.capture_history.remove(&child);
        let local_names = self
            .current
            .get(&child)
            .into_iter()
            .flat_map(HashMap::keys)
            .cloned()
            .collect::<Vec<_>>();
        for name in local_names {
            self.refresh_capturable_name(child, &name);
        }
        self.changed(child);
        id
    }

    /// Freeze an environment that can outlive `parent` and every ancestor.
    /// Inherited declaration modules may refer to older shadowed value
    /// generations, so retain the ancestors' complete live value set and
    /// transitive tip dependencies as well as the ordinary visible names.
    pub fn seed_detached_scope(
        &mut self,
        tree: &ScopeTree,
        parent: ScopeId,
        child: ScopeId,
    ) -> BindingTipId {
        self.seed_scope(tree, parent, child)
    }

    /// Retain all value generations an exact external facade from `source`
    /// could import, including dependencies its own inherited tip retains,
    /// without making its names visible in `target`.
    pub fn retain_scope_dependencies(
        &mut self,
        tree: &ScopeTree,
        source: ScopeId,
        target: ScopeId,
    ) -> bool {
        if !tree.is_live(source) || !tree.is_live(target) || !self.tips.contains_key(&target) {
            return false;
        }
        let already = &self.tips[&target].retained;
        let retained: HashSet<_> = self
            .scope_dependency_ids(tree, source)
            .iter()
            .filter(|id| !already.contains(id))
            .copied()
            .collect();
        let source_keys = self.source_instance_keys_in(tree, source);
        let already_source = self.source_instance_keys_in(tree, target);
        let additional_source: HashSet<_> =
            source_keys.difference(&already_source).cloned().collect();
        if !retained.is_empty() || !additional_source.is_empty() {
            self.lease_exact_ids(&retained);
            self.acquire_source_shares(&additional_source);
            self.add_capturable_bindings(target, retained.iter().copied());
            self.add_capturable_sources(target, additional_source.iter().cloned());
            let tip = self.tips.get_mut(&target).expect("target was checked");
            for id in &retained {
                tip.retained.insert_mut(*id);
            }
            for key in &additional_source {
                tip.source_instances.insert_mut(key.clone());
            }
            tip.leases = Arc::new(BindingLeaseChunk {
                parents: vec![Arc::clone(&tip.leases)],
                bindings: retained,
                source_instances: additional_source,
            });
            self.changed(target);
        }
        true
    }

    /// The immutable inherited-view identity for `scope`, when it has one.
    #[must_use]
    pub fn tip_id(&self, scope: ScopeId) -> Option<BindingTipId> {
        self.tips.get(&scope).map(|tip| tip.id)
    }

    fn release_tip(&mut self, scope: ScopeId) -> (Vec<BindingEntry>, Vec<SourceInstanceLease>) {
        let Some(tip) = self.tips.remove(&scope) else {
            return (Vec::new(), Vec::new());
        };
        self.release_chunks(tip.leases)
    }

    // Metadata has one latest owner-local history entry. Its Weak chunk never
    // retains roots; captures and live tips own every physical lease.
    fn capture_leases(
        &mut self,
        parent: ScopeId,
        membership: &CaptureMembership,
    ) -> Arc<BindingLeaseChunk> {
        let retained = &membership.retained;
        let source_instances = &membership.source_instances;
        let previous = self.capture_history.remove(&parent).and_then(|history| {
            history.chunk.upgrade().map(|chunk| {
                (
                    chunk,
                    history.retained,
                    history.source_instances,
                    history.append_epoch,
                )
            })
        });
        let inherited = self.tips.get(&parent).map(|tip| {
            (
                Arc::clone(&tip.leases),
                tip.retained.clone(),
                tip.source_instances.clone(),
                None,
            )
        });
        let mut selected = None;
        // Test coverage while selecting the current deltas, without walking
        // the historical chunk chain. A retraction cannot retain obsolete
        // native custody through a previous capture's physical parent.
        for (chunk, covered, covered_source, epoch) in previous.into_iter().chain(inherited) {
            // Equal nonzero epochs are issued only by the owning append-only
            // mutation paths. Weak upgrade supplies physical custody, never
            // membership authority. The exact accumulated additions suffice.
            let append_only = epoch.is_some() && epoch == membership.append_epoch;
            let bindings = if append_only {
                membership.added_bindings.clone()
            } else if retained.ptr_eq(&covered) {
                HashSet::new()
            } else {
                retained
                    .iter()
                    .filter(|id| !covered.contains(id))
                    .copied()
                    .collect()
            };
            let sources = if append_only {
                membership.added_sources.clone()
            } else if source_instances.ptr_eq(&covered_source) {
                HashSet::new()
            } else {
                source_instances
                    .iter()
                    .filter(|key| !covered_source.contains(key))
                    .cloned()
                    .collect()
            };
            if append_only
                || (retained.size() - bindings.len() == covered.size()
                    && source_instances.size() - sources.len() == covered_source.size())
            {
                selected = Some((chunk, bindings, sources));
                break;
            }
            // A failed candidate can be the final strong reference. Return
            // its custody through the same iterative drain mechanism.
            let released = self.release_chunks(chunk);
            assert!(
                released.0.is_empty() && released.1.is_empty(),
                "candidate remains captured"
            );
        }
        let (parents, bindings, sources) = match selected {
            Some((chunk, bindings, sources)) if bindings.is_empty() && sources.is_empty() => {
                self.capture_history.insert(
                    parent,
                    CaptureLeaseHistory {
                        chunk: Arc::downgrade(&chunk),
                        retained: retained.clone(),
                        source_instances: source_instances.clone(),
                        append_epoch: membership.append_epoch,
                    },
                );
                return chunk;
            }
            Some((parent, bindings, sources)) => (vec![parent], bindings, sources),
            None => (
                Vec::new(),
                retained.iter().copied().collect(),
                source_instances.iter().cloned().collect(),
            ),
        };
        self.lease_exact_ids(&bindings);
        self.acquire_source_shares(&sources);
        let chunk = Arc::new(BindingLeaseChunk {
            parents,
            bindings,
            source_instances: sources,
        });
        self.capture_history.insert(
            parent,
            CaptureLeaseHistory {
                chunk: Arc::downgrade(&chunk),
                retained: retained.clone(),
                source_instances: source_instances.clone(),
                append_epoch: membership.append_epoch,
            },
        );
        chunk
    }

    fn release_chunks(
        &mut self,
        chunk: Arc<BindingLeaseChunk>,
    ) -> (Vec<BindingEntry>, Vec<SourceInstanceLease>) {
        let mut pending = vec![chunk];
        let mut bindings = Vec::new();
        let mut sources = Vec::new();
        while let Some(chunk) = pending.pop() {
            if let Ok(chunk) = Arc::try_unwrap(chunk) {
                bindings.extend(self.release_leases(chunk.bindings));
                sources.extend(self.release_source_shares(chunk.source_instances));
                pending.extend(chunk.parents);
            }
        }
        (bindings, sources)
    }

    /// Lease exact binding identities, including identities reserved for future
    /// materialization. Existing observation dependencies share the same lease.
    pub fn acquire_leases(
        &mut self,
        ids: impl IntoIterator<Item = SessionVarId>,
    ) -> HashSet<SessionVarId> {
        let retained = self.dependency_closure(ids);
        self.lease_exact_ids(&retained);
        retained
    }

    fn dependency_closure(
        &self,
        ids: impl IntoIterator<Item = SessionVarId>,
    ) -> HashSet<SessionVarId> {
        let mut retained = HashSet::new();
        let mut pending = ids.into_iter().collect::<Vec<_>>();
        while let Some(id) = pending.pop() {
            if retained.insert(id) {
                if let Some(observation) = self.observations.get(&id) {
                    pending.extend(observation.dependencies.iter().copied());
                }
            }
        }
        retained
    }

    fn lease_exact_ids(&mut self, ids: &HashSet<SessionVarId>) {
        for id in ids {
            *self.leases.entry(*id).or_default() += 1;
        }
    }

    /// Release a previously acquired set; the session owns deregistration of
    /// the returned roots. Unmaterialized identities need no special cleanup.
    pub fn release_leases(
        &mut self,
        retained: impl IntoIterator<Item = SessionVarId>,
    ) -> Vec<BindingEntry> {
        let mut released = Vec::new();
        for id in retained {
            let Some(count) = self.leases.get_mut(&id) else {
                continue;
            };
            *count -= 1;
            if *count == 0 {
                self.leases.remove(&id);
                if self.retired_owners.remove(&id) {
                    if let Some(entry) = self.remove_entry(id) {
                        self.scope_local_aliases.remove(&id);
                        released.push(entry);
                    }
                }
            }
        }
        released
    }

    /// How many outstanding leases retain `id` right now; zero for an
    /// unleased or unknown identity. [`Self::remove_live`] refuses a leased
    /// entry, and an owner that must report *why* reads this first.
    #[must_use]
    pub fn lease_count(&self, id: SessionVarId) -> usize {
        self.leases.get(&id).copied().unwrap_or(0)
    }

    /// Resolve a name to its CURRENT binding (newest gen) in the ROOT frame,
    /// or `None` if the name is not session-bound (the caller then falls
    /// through to normal Var resolution / the unresolved-var trap).
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<&BindingEntry> {
        let id = self
            .current
            .get(&ScopeId::ROOT)?
            .get(&BindingName(name.to_string()))?;
        self.live.get(id)
    }

    /// Scoped [`Self::resolve`]. A seeded scope reads its own mutable frame,
    /// then the immutable inherited tip captured at mint time. Unseeded
    /// scopes retain the legacy upward walk for low-level callers; production
    /// scope minting always seeds a tip.
    ///
    /// `None` for a retired or never-minted `scope` (its lookup chain is
    /// empty) — a stale scope reference resolves nothing rather than silently
    /// falling back to ROOT.
    #[must_use]
    pub fn resolve_in(
        &self,
        tree: &ScopeTree,
        scope: ScopeId,
        name: &str,
    ) -> Option<&BindingEntry> {
        let key = BindingName(name.to_string());
        if self.tips.contains_key(&scope) {
            if let Some(id) = self.current.get(&scope).and_then(|frame| frame.get(&key)) {
                return self.live.get(id);
            }
            if self
                .hidden
                .get(&scope)
                .is_some_and(|hidden| hidden.contains(&key))
            {
                return None;
            }
            return self
                .tips
                .get(&scope)
                .and_then(|tip| tip.visible.get(&key))
                .and_then(|id| self.live.get(id));
        }
        for s in tree.lookup_chain(scope) {
            if let Some(id) = self.current.get(&s).and_then(|frame| frame.get(&key)) {
                if s != scope && self.scope_local_aliases.contains(id) {
                    continue;
                }
                return self.live.get(id);
            }
        }
        None
    }

    /// Look up a specific (possibly shadowed) binding by its id.
    #[must_use]
    pub fn get(&self, id: SessionVarId) -> Option<&BindingEntry> {
        self.live.get(&id)
    }

    /// Resolve a compiler-retained import only through the exact lexical
    /// view that admitted the turn. The compiler supplies both the original
    /// Name and its value-interface generation; neither a spelling lookup nor
    /// the newest globally retained binding can authorize a sibling's root.
    #[must_use]
    pub fn resolve_exact_prepared_in(
        &self,
        tree: &ScopeTree,
        scope: ScopeId,
        identity: &tidepool_repr::execution_schema::SymbolIdentity,
        generation: u64,
    ) -> Option<&BindingEntry> {
        if !tree.is_live(scope) {
            return None;
        }
        let module = SessionModule::val(tidepool_repr::Generation(generation));
        self.scope_reachable_binding_ids(tree, scope)
            .into_iter()
            .filter_map(|id| self.live.get(&id))
            .find(|entry| entry.module == module && entry.value.identity == *identity)
    }

    /// Every live binding (including shadowed older gens), unordered.
    pub fn iter_live(&self) -> impl Iterator<Item = &BindingEntry> {
        self.live.values()
    }

    /// The current (newest) ROOT-frame bindings, as `(name, entry)` pairs —
    /// the `:bindings` view (shadowed older gens are excluded, and so is every
    /// non-ROOT scope: a child's names are not the flat session's).
    pub fn iter_current(&self) -> impl Iterator<Item = (&BindingName, &BindingEntry)> {
        self.current
            .get(&ScopeId::ROOT)
            .into_iter()
            .flat_map(move |frame| {
                frame
                    .iter()
                    .filter_map(|(name, id)| self.live.get(id).map(|e| (name, e)))
            })
    }

    /// The visible environment at `scope`: its local mutable frame over its
    /// immutable inherited tip. Unseeded scopes use the legacy upward walk.
    /// `iter_current_in(tree, ScopeId::ROOT)` is exactly
    /// [`Self::iter_current`]'s set.
    ///
    /// A `Vec` rather than an iterator because the shadowing dedup is stateful;
    /// the result is sorted by name so callers get a deterministic order out of
    /// `HashMap` iteration.
    #[must_use]
    pub fn iter_current_in(
        &self,
        tree: &ScopeTree,
        scope: ScopeId,
    ) -> Vec<(&BindingName, &BindingEntry)> {
        if !tree.is_live(scope) {
            return Vec::new();
        }
        if let Some(tip) = self.tips.get(&scope) {
            let hidden = self.hidden.get(&scope);
            let mut seen: Vec<(&BindingName, &BindingEntry)> = self
                .current
                .get(&scope)
                .into_iter()
                .flat_map(|frame| frame.iter())
                .filter_map(|(name, id)| self.live.get(id).map(|entry| (name, entry)))
                .collect();
            let mut names: HashSet<_> = seen.iter().map(|(name, _)| *name).collect();
            for (name, id) in tip.visible.iter() {
                if names.contains(name) || hidden.is_some_and(|hidden| hidden.contains(name)) {
                    continue;
                }
                if let Some(entry) = self.live.get(id) {
                    names.insert(name);
                    seen.push((name, entry));
                }
            }
            seen.sort_by(|a, b| a.0 .0.cmp(&b.0 .0));
            return seen;
        }
        let mut seen: Vec<(&BindingName, &BindingEntry)> = Vec::new();
        let mut names = HashSet::new();
        for s in tree.lookup_chain(scope) {
            let Some(frame) = self.current.get(&s) else {
                continue;
            };
            for (name, id) in frame {
                // Nearest frame wins: a name already taken from a DEEPER frame
                // shadows this one.
                if names.contains(name) {
                    continue;
                }
                if s != scope && self.scope_local_aliases.contains(id) {
                    continue;
                }
                if let Some(e) = self.live.get(id) {
                    names.insert(name);
                    seen.push((name, e));
                }
            }
        }
        seen.sort_by(|a, b| a.0 .0.cmp(&b.0 .0));
        seen
    }

    /// How many names `scope`'s OWN frame currently binds — the persistent-binding-store
    /// accounting class per scope, the scoped mirror of `iter_current().count()`
    /// (which is the ROOT frame). Shadowed older gens are not counted, exactly
    /// as they are not counted at ROOT; a scope with no frame answers 0.
    #[must_use]
    pub fn scope_binding_count(&self, scope: ScopeId) -> usize {
        self.current.get(&scope).map_or(0, HashMap::len)
    }

    /// The set of live `Val.G<g>` modules to inject (`SessionScope.ssValIfaces`)
    /// before compiling a reference turn — one per still-rooted binding.
    pub fn live_modules(&self) -> impl Iterator<Item = SessionModule> + '_ {
        self.modules.keys().copied()
    }

    /// The live `Val.G<g>` modules a turn compiled at `scope` can reach:
    /// every generation, shadowed ones included, bound in a frame that
    /// [`Self::iter_current_in`] reads for `scope`, plus the values its
    /// inherited tip holds.
    ///
    /// A seeded scope reads only its own frame over its immutable tip, so an
    /// ancestor's later binds are outside this set; an unseeded scope reads
    /// its whole lookup chain. Bindings owned by any other scope (a sibling,
    /// a descendant, an unrelated actor's isolated root) are never imported
    /// by such a turn, and whatever an imported value reaches through them is
    /// leased by its owner's custody rather than named here.
    ///
    /// Frozen tips include shadowed generations required by inherited code.
    /// One entry may be reached through more than one custody path; callers
    /// selecting a set deduplicate the returned identities.
    pub fn scope_reachable_bindings<'a>(
        &'a self,
        tree: &ScopeTree,
        scope: ScopeId,
    ) -> impl Iterator<Item = &'a BindingEntry> {
        let tip = self.tips.get(&scope).filter(|_| tree.is_live(scope));
        let frames = if !tree.is_live(scope) {
            Vec::new()
        } else if tip.is_some() {
            vec![scope]
        } else {
            tree.lookup_chain(scope)
        };
        let promoted_frames = frames.clone();
        let owned = frames
            .into_iter()
            .flat_map(|owner| self.owned.get(&owner).into_iter().flat_map(HashSet::iter))
            .filter_map(|id| self.live.get(id));
        let inherited = tip
            .into_iter()
            .flat_map(|tip| tip.visible.values().chain(tip.retained.iter()))
            .filter_map(|id| self.live.get(id));
        let promoted = promoted_frames
            .into_iter()
            .flat_map(|scope| {
                self.promoted
                    .get(&scope)
                    .into_iter()
                    .flat_map(HashSet::iter)
            })
            .filter_map(|id| self.live.get(id));
        owned.chain(inherited).chain(promoted)
    }

    /// Exact scoped modules, including shadowed and retained generations.
    pub fn scope_reachable_modules(
        &self,
        tree: &ScopeTree,
        scope: ScopeId,
    ) -> impl Iterator<Item = SessionModule> + '_ {
        self.scope_reachable_bindings(tree, scope)
            .map(|entry| entry.module)
    }

    /// Exact live identities available to this scope, including shadowed
    /// owners, inherited and promoted custody, and observation dependencies.
    /// This does not grant another scope's same-module bindings.
    pub fn scope_reachable_binding_ids(
        &self,
        tree: &ScopeTree,
        scope: ScopeId,
    ) -> Vec<SessionVarId> {
        let mut ids = self
            .scope_dependency_ids(tree, scope)
            .into_iter()
            .filter(|id| self.live.contains_key(id))
            .collect::<Vec<_>>();
        ids.sort_by_key(|id| id.raw());
        ids
    }

    fn scope_dependency_ids(&self, tree: &ScopeTree, scope: ScopeId) -> HashSet<SessionVarId> {
        if tree.is_live(scope)
            && (self.tips.contains_key(&scope) || tree.parent_of(scope).is_none())
        {
            if let Some(projected) = self.capturable_membership.get(&scope) {
                if self.membership_dependencies_current(scope, projected) {
                    return projected.retained.iter().copied().collect();
                }
            }
        }
        self.scope_dependency_ids_slow(tree, scope)
    }

    fn scope_dependency_ids_slow(&self, tree: &ScopeTree, scope: ScopeId) -> HashSet<SessionVarId> {
        if !tree.is_live(scope) {
            return HashSet::new();
        }
        let tip = self.tips.get(&scope);
        let owners = if tip.is_some() {
            vec![scope]
        } else {
            tree.lookup_chain(scope)
        };
        let own = owners
            .iter()
            .flat_map(|owner| self.owned.get(owner).into_iter().flat_map(HashSet::iter))
            .copied();
        let inherited = tip
            .into_iter()
            .flat_map(|tip| tip.visible.values().chain(tip.retained.iter()))
            .copied();
        let promoted = owners
            .iter()
            .flat_map(|owner| self.promoted.get(owner).into_iter().flat_map(HashSet::iter))
            .copied();
        self.dependency_closure(own)
            .into_iter()
            .chain(inherited)
            .chain(promoted)
            .collect()
    }

    /// Number of live bindings.
    #[must_use]
    pub fn len(&self) -> usize {
        self.live.len()
    }

    /// Whether no bindings are live.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live.is_empty() && self.source_instances.is_empty()
    }
}

#[cfg(test)]
#[path = "binding_table/identity_tests.rs"]
mod identity_tests;

#[cfg(test)]
#[path = "binding_table/persistent_membership_cost_tests.rs"]
mod persistent_membership_cost_tests;

#[cfg(test)]
#[path = "binding_table/cost_tests.rs"]
pub(crate) mod cost_tests;
