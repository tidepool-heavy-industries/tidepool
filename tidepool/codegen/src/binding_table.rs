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
//! session (`PersistentSession::scopes`) and this table stays machine-free —
//! which is what keeps its `unsafe impl Send` justification a claim about
//! `RootSlot` addresses alone.

use std::collections::{HashMap, HashSet};

use tidepool_repr::{BindingName, SessionModule, SessionVarId, VarId};

use crate::old_space::RootSlot;
use crate::scope::{ScopeId, ScopeTree};

/// Identity of one immutable value-binding view captured for a child scope.
///
/// IDs are monotonic within a resident session and never reused. The ID is
/// presentation/provenance; `SessionVarId` remains the binding authority.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BindingTipId(pub u64);

#[derive(Debug)]
struct BindingTip {
    id: BindingTipId,
    visible: HashMap<BindingName, SessionVarId>,
    retained: HashSet<SessionVarId>,
}

/// A value retained by a prepared-STG `PreparedMachine`: tenured as-is (never
/// deep-forced, so its preparation policy is Tier-1's), rooted by `root` for
/// the machine's life. `handle` is the machine's own ownership handle for that root
/// (what a later program's `ImportBindings` names), held under the machine's
/// ROOT scope so no resource-scope close releases it; `identity` is what a later
/// program links against when it imports this binding.
#[derive(Clone, Debug)]
pub struct BoundValue {
    pub root: RootSlot,
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

/// The `name → (SessionVarId, RootSlot, SessionModule)` bridge (domain §4).
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
    /// Append-only-by-id store of every live binding (old gens retained),
    /// flat and globally keyed across every scope.
    live: HashMap<SessionVarId, BindingEntry>,
    /// Immutable, flattened inherited view captured when a scope is minted.
    tips: HashMap<ScopeId, BindingTip>,
    /// Names deliberately hidden by this scope's persistent declaration environment.
    hidden: HashMap<ScopeId, HashSet<BindingName>>,
    /// Binding tips and prepared work retaining each value identity.
    leases: HashMap<SessionVarId, usize>,
    /// Entries whose owning scope retired while another owner still leased
    /// them. They leave `live` when the final lease is released.
    retired_owners: HashSet<SessionVarId>,
    next_tip: u64,
    observations: HashMap<SessionVarId, ObservationBinding>,
    next_observation_order: u64,
    scope_local_aliases: HashSet<SessionVarId>,
}

struct ObservationBinding {
    dependencies: Vec<SessionVarId>,
    recent: Option<u64>,
    retain_while_current: bool,
}

impl Default for BindingTable {
    fn default() -> Self {
        Self {
            current: HashMap::new(),
            live: HashMap::new(),
            tips: HashMap::new(),
            hidden: HashMap::new(),
            leases: HashMap::new(),
            retired_owners: HashSet::new(),
            next_tip: 1,
            observations: HashMap::new(),
            next_observation_order: 0,
            scope_local_aliases: HashSet::new(),
        }
    }
}

// SAFETY: a `BindingTable` is only `!Send` because a `BindingEntry`'s
// `BoundValue` carries a `RootSlot(*mut *mut u8)`. That slot is a stable
// ADDRESS into the owning `PreparedMachine`'s persistent-root region — process-
// global address space, valid on any thread, and moved together WITH the machine
// (a resident session owns both). It is only ever dereferenced during a run, and
// a session is stowed-XOR-running (the same discipline that justifies
// `unsafe impl Send for PreparedMachine`), so the table and its slots are
// touched by exactly one thread at a time. Sending ownership across the
// suspend/resume thread boundary is therefore sound.
unsafe impl Send for BindingTable {}

impl BindingTable {
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
        self.observations.insert(
            id,
            ObservationBinding {
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
                if let Some(frame) = self.current.get_mut(&entry.scope) {
                    if frame.get(&entry.name) == Some(&id) {
                        frame.remove(&entry.name);
                    }
                }
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
            if let Some(observation) = self.observations.remove(&id) {
                pending.extend(observation.dependencies);
            }
        }
    }

    /// Collect expired automatic roots only after accounting for compiled
    /// observation dependencies and immutable fork views.
    pub fn collect_observations(&mut self) -> Vec<BindingEntry> {
        self.observations.retain(|id, _| self.live.contains_key(id));
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
            self.observations.remove(&id);
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

    /// Record a (re)bind. Repoints `current[name]` to the fresh id and inserts
    /// the entry into `live`; any prior entry for the same name stays in `live`
    /// under its own (older) id. Returns the bound id.
    ///
    /// # Safety
    /// `entry.value`'s [`RootSlot`] must be a registered persistent GC root
    /// valid until the session machine drops (the contract carried by
    /// [`crate::machine::PreparedMachine::run_fragment_and_bind`], which is
    /// the only minter of a `RootSlot`). This method only stores the slot — it
    /// never dereferences it — so it is itself safe; the liveness invariant is
    /// upheld at the bind site and the Var-miss load site.
    pub fn bind(&mut self, entry: BindingEntry) -> SessionVarId {
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
    /// # Safety
    /// Same slot-liveness contract as [`Self::bind`] — this method only stores
    /// the [`RootSlot`], never dereferences it.
    pub fn bind_in(&mut self, scope: ScopeId, mut entry: BindingEntry) -> SessionVarId {
        entry.scope = scope;
        let id = entry.id;
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
        self.live.insert(id, entry);
        id
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
        let id = self.bind_in(scope, entry);
        self.scope_local_aliases.insert(id);
        self.observations.insert(
            id,
            ObservationBinding {
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
        if let Some(frame) = self.current.get_mut(&scope) {
            frame.remove(&name);
        }
        if self
            .tips
            .get(&scope)
            .is_some_and(|tip| tip.visible.contains_key(&name))
        {
            self.hidden.entry(scope).or_default().insert(name);
        }
    }

    /// Evict one binding from `live` ENTIRELY — the primitive scope
    /// retirement is built from, and the only thing that ever removes a `live`
    /// entry ([`Self::remove_current`] merely un-shadows one).
    ///
    /// PURE BOOKKEEPING: it removes the entry from `live`, and clears the
    /// owning frame's `name → id` mapping if (and only if) that frame still
    /// points at this id. It does NOT touch the machine — releasing the
    /// entry's GC root is the caller's separate, scope-owned step
    /// (`PreparedMachine::retire_scope_root`), which is what keeps this type
    /// machine-free and its `unsafe impl Send` justification intact.
    ///
    /// Returns the evicted entry (so the caller can read its [`RootSlot`]), or
    /// `None` if `id` was not live.
    pub fn remove_live(&mut self, id: SessionVarId) -> Option<BindingEntry> {
        if self.leases.get(&id).copied().unwrap_or(0) > 0 {
            return None;
        }
        let entry = self.live.remove(&id)?;
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
        Some(entry)
    }

    /// Retire one exact owner without touching older captured generations.
    /// A leased entry leaves the current frame now and is released after its
    /// final lease; an unleased entry is returned to the session owner now.
    pub fn retire_owner(&mut self, id: SessionVarId) -> Option<BindingEntry> {
        let entry = self.live.get(&id)?;
        if let Some(frame) = self.current.get_mut(&entry.scope) {
            if frame.get(&entry.name) == Some(&id) {
                frame.remove(&entry.name);
            }
        }
        if self.leases.get(&id).copied().unwrap_or(0) > 0 {
            self.retired_owners.insert(id);
            None
        } else {
            let removed = self.live.remove(&id);
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
        let mut released = self.release_tip(scope);
        let mut ids: Vec<SessionVarId> = self
            .live
            .iter()
            .filter(|(_, e)| e.scope == scope)
            .map(|(&id, _)| id)
            .collect();
        ids.sort_by_key(|id| id.raw());
        self.current.remove(&scope);
        self.hidden.remove(&scope);
        for id in ids {
            if self.leases.get(&id).copied().unwrap_or(0) > 0 {
                self.retired_owners.insert(id);
            } else if let Some(entry) = self.live.remove(&id) {
                self.scope_local_aliases.remove(&id);
                released.push(entry);
            }
        }
        released.sort_by_key(|entry| entry.id.raw());
        released
    }

    /// Freeze the value bindings visible from `parent` as `child`'s immutable
    /// inherited view. Each distinct referenced value receives one root lease
    /// owned by the child tip.
    pub fn seed_scope(
        &mut self,
        tree: &ScopeTree,
        parent: ScopeId,
        child: ScopeId,
    ) -> BindingTipId {
        if let Some(tip) = self.tips.get(&child) {
            return tip.id;
        }
        let inherited: Vec<_> = self
            .iter_current_in(tree, parent)
            .into_iter()
            .map(|(name, entry)| (name.clone(), entry.id))
            .collect();
        // Keep the parent's exact value identities rooted for inherited code,
        // but do not give a fresh actor its parent's local display alias name.
        let retained = self.acquire_leases(inherited.iter().map(|(_, id)| *id));
        let visible = inherited
            .into_iter()
            .filter(|(_, id)| !self.scope_local_aliases.contains(id))
            .collect();
        let id = BindingTipId(self.next_tip);
        self.next_tip += 1;
        self.tips.insert(
            child,
            BindingTip {
                id,
                visible,
                retained,
            },
        );
        id
    }

    /// The immutable inherited-view identity for `scope`, when it has one.
    #[must_use]
    pub fn tip_id(&self, scope: ScopeId) -> Option<BindingTipId> {
        self.tips.get(&scope).map(|tip| tip.id)
    }

    fn release_tip(&mut self, scope: ScopeId) -> Vec<BindingEntry> {
        let Some(tip) = self.tips.remove(&scope) else {
            return Vec::new();
        };
        self.release_leases(tip.retained)
    }

    /// Lease exact binding identities, including identities reserved for future
    /// materialization. Existing observation dependencies share the same lease.
    pub fn acquire_leases(
        &mut self,
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
        for id in &retained {
            *self.leases.entry(*id).or_default() += 1;
        }
        retained
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
                    if let Some(entry) = self.live.remove(&id) {
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
            for (name, id) in &tip.visible {
                if seen.iter().any(|(local, _)| *local == name)
                    || hidden.is_some_and(|hidden| hidden.contains(name))
                {
                    continue;
                }
                if let Some(entry) = self.live.get(id) {
                    seen.push((name, entry));
                }
            }
            seen.sort_by(|a, b| a.0 .0.cmp(&b.0 .0));
            return seen;
        }
        let mut seen: Vec<(&BindingName, &BindingEntry)> = Vec::new();
        for s in tree.lookup_chain(scope) {
            let Some(frame) = self.current.get(&s) else {
                continue;
            };
            for (name, id) in frame {
                // Nearest frame wins: a name already taken from a DEEPER frame
                // shadows this one.
                if seen.iter().any(|(n, _)| *n == name) {
                    continue;
                }
                if s != scope && self.scope_local_aliases.contains(id) {
                    continue;
                }
                if let Some(e) = self.live.get(id) {
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
        self.live.values().map(|e| e.module)
    }

    /// Number of live bindings.
    #[must_use]
    pub fn len(&self) -> usize {
        self.live.len()
    }

    /// Whether no bindings are live.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }
}
