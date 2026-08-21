//! Session binding table — the bridge GHC already uses, made concrete.
//!
//! GHCi splits a binding's identity into a *type* half (`ic_tythings`) and a
//! *value* half (the linker's `closure_env`), keyed by one `Name`. Our
//! [`BindingTable`] is exactly that bridge: keyed by one [`SessionVarId`], the
//! type half is the thin `Tidepool.Session.Val.G<g>` iface on disk (GHC's plane)
//! and the value half is the live, GC-rooted [`BoundValue`] in the resident
//! machine's heap (the JIT's plane).
//!
//! ## The two-layer shape (domain model §4)
//!
//! A mutable `name → SessionVarId` map (`current`) over an append-only set of
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
//! ## Scope frames (PRD 21 lane C2)
//!
//! The shadowing layer is one frame PER SCOPE ([`ScopeId`]), not one map for
//! the session: `current: ScopeId → (name → newest id)`. `live` stays FLAT and
//! globally keyed by [`SessionVarId`] — ids are globally unique, and every
//! id-keyed read ([`BindingTable::get`], [`BindingTable::seed_external_env`],
//! [`BindingTable::live_modules`]) therefore resolves a scoped binding with no
//! change at all.
//!
//! Every no-arg method means [`ScopeId::ROOT`], the flat session, and keeps its
//! exact pre-C2 behavior: `bind(e) == bind_in(ROOT, e)`, `resolve(n) ==
//! resolve_in(_, ROOT, n)`, and `iter_current()` is the ROOT frame. The scoped
//! siblings take the [`ScopeTree`] as a PARAMETER rather than owning one: the
//! decl plane keys off the same [`ScopeId`]s, so exactly one tree exists per
//! session (`PersistentSession::scopes`) and this table stays machine-free —
//! which is what keeps its `unsafe impl Send` justification a claim about
//! `RootSlot` addresses alone.

use std::collections::HashMap;

use tidepool_repr::{BindingName, SessionModule, SessionVarId, VarId};

use crate::emit::ExternalEnv;
use crate::old_space::RootSlot;
use crate::scope::{ScopeId, ScopeTree};

/// The strict-force-vs-store-as-is distinction at the type level (domain §4).
///
/// Both variants hold the stable [`RootSlot`] the GC updates in place; the
/// distinction records *how the value was prepared at bind time*, which the
/// `:bindings` view and any future re-forcing logic consult.
#[derive(Copy, Clone, Debug)]
pub enum BoundValue {
    /// First-order data (Tier-0): `deep_force`d to normal form then tenured.
    Tier0Forced(RootSlot),
    /// A closure/PAP (Tier-1): tenured as-is (NOT deep-forced), valid while the
    /// session machine lives — its code stays callable across later fragments.
    Tier1Closure(RootSlot),
}

impl BoundValue {
    /// The GC-updated root slot the value resolution loads through.
    #[must_use]
    pub fn root(&self) -> RootSlot {
        match self {
            BoundValue::Tier0Forced(r) | BoundValue::Tier1Closure(r) => *r,
        }
    }

    /// Whether this binding was strict-forced at bind time (Tier-0).
    #[must_use]
    pub fn is_forced(&self) -> bool {
        matches!(self, BoundValue::Tier0Forced(_))
    }
}

/// One resolved session binding — the bridge record for a single `x`.
pub struct BindingEntry {
    /// The user-facing name (`"x"`).
    pub name: BindingName,
    /// Stable `0xFE` session id minted by the extract; the `ExternalEnv` key and
    /// the id a later reference turn's Core `NVar` carries.
    pub id: SessionVarId,
    /// `Tidepool.Session.Val.G<g>` — derives the `.hi` path and is what later
    /// turns inject (`SessionScope.ssValIfaces`).
    pub module: SessionModule,
    /// The live heap root (GC-safe slot) + its tier.
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
/// fragments compiled against an old gen keep resolving. Entries live until the
/// session machine drops (then the persistent roots are reclaimed wholesale).
///
/// `current` is keyed by [`ScopeId`] FIRST: one shadowing frame per scope, so
/// two sibling scopes can each bind `helper` without either seeing the other
/// and without the parent gaining the name. `live` is deliberately NOT keyed
/// by scope — a `SessionVarId` is globally unique, and an already-compiled
/// fragment resolves by id regardless of which frame minted it.
#[derive(Default)]
pub struct BindingTable {
    /// Shadowing layer, one frame per scope: scope → (name → newest gen's id).
    /// A scope with no bindings has no frame (absent, not empty).
    current: HashMap<ScopeId, HashMap<BindingName, SessionVarId>>,
    /// Append-only-by-id store of every live binding (old gens retained),
    /// flat and globally keyed across every scope.
    live: HashMap<SessionVarId, BindingEntry>,
}

// SAFETY: a `BindingTable` is only `!Send` because a `BindingEntry`'s
// `BoundValue` carries a `RootSlot(*mut *mut u8)`. That slot is a stable
// ADDRESS into the owning `JitEffectMachine`'s persistent-root region — process-
// global address space, valid on any thread, and moved together WITH the machine
// (a resident session owns both). It is only ever dereferenced during a run, and
// a session is stowed-XOR-running (the same discipline that justifies
// `unsafe impl Send for JitEffectMachine`), so the table and its slots are
// touched by exactly one thread at a time. Sending ownership across the
// suspend/resume thread boundary is therefore sound.
unsafe impl Send for BindingTable {}

impl BindingTable {
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
    /// [`crate::jit_machine::JitEffectMachine::run_fragment_and_bind`], which is
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
    /// (locked decision 4's "the parent never gains child declarations by
    /// name" holds by representation, not by a check), and sibling frames are
    /// disjoint maps, so two siblings binding the same name never collide.
    ///
    /// # Safety
    /// Same slot-liveness contract as [`Self::bind`] — this method only stores
    /// the [`RootSlot`], never dereferences it.
    pub fn bind_in(&mut self, scope: ScopeId, mut entry: BindingEntry) -> SessionVarId {
        entry.scope = scope;
        let id = entry.id;
        // NEWEST GEN WINS BY COMPARISON, not by insertion order: with
        // any-order resume (one-session plan), a bind minted at gen 6 can
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
            self.current
                .entry(scope)
                .or_default()
                .insert(entry.name.clone(), id);
        }
        self.live.insert(id, entry);
        id
    }

    /// Drop `name` from the ROOT frame so `iter_current`/`resolve` no longer
    /// see it (its `live` entry + root are retained for fragments compiled
    /// against the old gen). Used when a pure decl of the same name supersedes
    /// a materialized value binding (cross-plane shadow, GHCi-environment
    /// model). No-op if `name` isn't current.
    pub fn remove_current(&mut self, name: &str) {
        if let Some(frame) = self.current.get_mut(&ScopeId::ROOT) {
            frame.remove(&BindingName(name.to_string()));
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
    /// (`JitEffectMachine::retire_scope_root`), which is what keeps this type
    /// machine-free and its `unsafe impl Send` justification intact.
    ///
    /// Returns the evicted entry (so the caller can read its [`RootSlot`]), or
    /// `None` if `id` was not live.
    pub fn remove_live(&mut self, id: SessionVarId) -> Option<BindingEntry> {
        let entry = self.live.remove(&id)?;
        if let Some(frame) = self.current.get_mut(&entry.scope) {
            // Only if the frame still names THIS id: a newer same-name gen in
            // the same frame has already repointed it, and a shadowed older
            // gen must not resurrect by removal of its shadower.
            if frame.get(&entry.name) == Some(&id) {
                frame.remove(&entry.name);
            }
        }
        Some(entry)
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
        let mut ids: Vec<SessionVarId> = self
            .live
            .iter()
            .filter(|(_, e)| e.scope == scope)
            .map(|(&id, _)| id)
            .collect();
        ids.sort_by_key(|id| id.raw());
        self.current.remove(&scope);
        ids.into_iter()
            .filter_map(|id| self.live.remove(&id))
            .collect()
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

    /// Scoped [`Self::resolve`]: walk `scope → parent → … → ROOT` and take the
    /// FIRST frame that has `name` — children read parent bindings, a local
    /// bind shadows an inherited one, and a sibling's frame is never on the
    /// walk. `resolve(n) == resolve_in(tree, ScopeId::ROOT, n)` for any tree.
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
        for s in tree.lookup_chain(scope) {
            if let Some(id) = self.current.get(&s).and_then(|frame| frame.get(&key)) {
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

    /// The VISIBLE ENVIRONMENT at `scope`: the upward walk with child frames
    /// shadowing parent ones — each name once, bound to the nearest frame that
    /// has it. `iter_current_in(tree, ScopeId::ROOT)` is exactly
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
                if let Some(e) = self.live.get(id) {
                    seen.push((name, e));
                }
            }
        }
        seen.sort_by(|a, b| a.0 .0.cmp(&b.0 .0));
        seen
    }

    /// How many names `scope`'s OWN frame currently binds — the value-plane
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

    /// Build the `ExternalEnv` the JIT consults at a Var-miss, seeded with
    /// only the `SessionVarId → RootSlot` slot addresses the incoming
    /// fragment actually references — `referenced` (typically
    /// `tidepool_repr::free_vars(&fragment)`) intersected with `live`, not
    /// every live binding. The Var-miss site emits a per-fragment `load`
    /// through the slot to read the GC-current pointer, so seeding the slot
    /// *address* (never a snapshot pointer) is what keeps the read GC-safe.
    ///
    /// This narrowing is about what gets SEEDED into the compiled fragment's
    /// env, not what stays GC-reachable: a binding absent from `referenced`
    /// is simply not looked up here — its `RootSlot` was already registered
    /// as a persistent GC root at bind time (`OldSpace::tenure`, independent
    /// of this table entirely) and keeps surviving collections regardless of
    /// whether any later fragment ends up referencing it. See
    /// `tests/session_seed_external_env_root_retention.rs`'s
    /// `unreferenced_bindings_survive_a_real_gc_after_narrowed_seeding` for
    /// the GC-poison proof.
    ///
    /// SCOPE-BLIND BY CONSTRUCTION, and deliberately unchanged by C2: this
    /// looks bindings up in `live`, which stays flat and globally keyed by
    /// [`SessionVarId`], so a fragment compiled inside a child scope seeds its
    /// scoped bindings through exactly this path with no scope argument and no
    /// edit here. A `SessionVarId` already names one binding unambiguously;
    /// which frame minted it is a resolution-time question, already answered
    /// by the time a `VarId` reaches this call.
    #[must_use]
    pub fn seed_external_env(&self, referenced: &[VarId]) -> ExternalEnv {
        let mut env = ExternalEnv::new();
        for &var in referenced {
            if let Some(entry) = self.live.get(&SessionVarId::from_var(var)) {
                env.insert(entry.id.var(), entry.value.root().addr());
            }
        }
        env
    }

    /// Number of live bindings.
    #[must_use]
    pub fn len(&self) -> usize {
        self.live.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::{Generation, VarId};

    /// A fake registered slot. The table only stores the address; these tests
    /// never load through it, so a dangling box address is fine here.
    fn fake_slot(boxed: &mut *mut u8) -> RootSlot {
        // SAFETY: test-only; never dereferenced (no bind/run happens).
        unsafe { RootSlot::new(boxed as *mut *mut u8) }
    }

    fn entry(name: &str, gen: u64, raw: u64, slot: RootSlot) -> BindingEntry {
        BindingEntry {
            defining_expr: None,
            name: BindingName(name.to_string()),
            id: SessionVarId::from_extract(raw),
            module: SessionModule::val(Generation(gen)),
            value: BoundValue::Tier0Forced(slot),
            type_display: Some("Int".to_string()),
            // Overwritten by `bind_in` from its scope argument; ROOT here so a
            // never-bound fixture entry reads as a flat-session binding.
            scope: ScopeId::ROOT,
        }
    }

    #[test]
    fn rebind_repoints_current_but_retains_old() {
        let mut a: *mut u8 = std::ptr::null_mut();
        let mut b: *mut u8 = std::ptr::null_mut();
        let (sa, sb) = (fake_slot(&mut a), fake_slot(&mut b));
        let mut t = BindingTable::new();

        let id1 = t.bind(entry("x", 1, (0xFE << 56) | 1, sa));
        let id2 = t.bind(entry("x", 2, (0xFE << 56) | 2, sb));
        assert_ne!(id1, id2);

        // current resolves to the newest; both ids stay live.
        assert_eq!(t.resolve("x").unwrap().id, id2);
        assert!(t.get(id1).is_some(), "old gen retained for live fragments");
        assert!(t.get(id2).is_some());
        assert_eq!(t.len(), 2);
        // current view shows only the newest x.
        assert_eq!(t.iter_current().count(), 1);
    }

    /// `seed_external_env` seeds every REFERENCED live binding, not every
    /// live binding unconditionally; this covers the special case where
    /// every live binding also happens to be referenced.
    #[test]
    fn seed_external_env_seeds_every_referenced_binding_when_all_are_referenced() {
        let mut a: *mut u8 = std::ptr::null_mut();
        let mut b: *mut u8 = std::ptr::null_mut();
        let (sa, sb) = (fake_slot(&mut a), fake_slot(&mut b));
        let mut t = BindingTable::new();
        let x_var = VarId((0xFE << 56) | 1);
        let y_var = VarId((0xFE << 56) | 3);
        t.bind(entry("x", 1, x_var.0, sa));
        t.bind(entry("y", 1, y_var.0, sb));

        let env = t.seed_external_env(&[x_var, y_var]);
        assert_eq!(env.len(), 2);
        assert!(env.get(x_var).is_some());
        assert!(env.get(y_var).is_some());
    }

    /// A live binding NOT in the referenced set must not be seeded — this
    /// is the narrowing itself. (Whether it stays a GC root regardless is a
    /// separate claim, proved by
    /// `session_seed_external_env_root_retention.rs`'s GC-poison test, not by
    /// this table in isolation.)
    #[test]
    fn seed_external_env_narrows_to_only_referenced_bindings() {
        let mut a: *mut u8 = std::ptr::null_mut();
        let mut b: *mut u8 = std::ptr::null_mut();
        let (sa, sb) = (fake_slot(&mut a), fake_slot(&mut b));
        let mut t = BindingTable::new();
        let x_var = VarId((0xFE << 56) | 1);
        let y_var = VarId((0xFE << 56) | 3);
        t.bind(entry("x", 1, x_var.0, sa));
        t.bind(entry("y", 1, y_var.0, sb));

        let env = t.seed_external_env(&[x_var]);
        assert_eq!(env.len(), 1);
        assert!(env.get(x_var).is_some());
        assert!(env.get(y_var).is_none());
    }

    /// OUT-OF-ORDER MATERIALIZATION (one-session plan, Phase 1 audit pin):
    /// with any-order resume, a bind minted at gen 6 can materialize AFTER a
    /// same-name bind minted at gen 7. `current` must shadow by GEN
    /// COMPARISON, not insertion order — the late older bind stays `live`
    /// (old-gen fragments still resolve it by id) but never clobbers the
    /// newer name.
    #[test]
    fn out_of_order_older_gen_does_not_clobber_newer_current() {
        let mut a: *mut u8 = std::ptr::null_mut();
        let mut b: *mut u8 = std::ptr::null_mut();
        let (sa, sb) = (fake_slot(&mut a), fake_slot(&mut b));
        let mut t = BindingTable::new();

        // Gen 7 materializes FIRST (its turn completed first)...
        let newer = t.bind(entry("x", 7, (0xFE << 56) | 7, sa));
        // ...then the STALLED gen-6 bind of the same name lands late.
        let older = t.bind(entry("x", 6, (0xFE << 56) | 6, sb));

        let cur = t.resolve("x").expect("x is bound");
        assert_eq!(cur.id, newer, "current must stay on the NEWER gen");
        assert!(t.get(older).is_some(), "the older bind stays live by id");

        // And the ordinary in-order case still repoints as always.
        let mut c: *mut u8 = std::ptr::null_mut();
        let sc = fake_slot(&mut c);
        let newest = t.bind(entry("x", 8, (0xFE << 56) | 8, sc));
        assert_eq!(t.resolve("x").expect("x").id, newest);
    }

    /// A referenced `VarId` that isn't a live session binding at all (an
    /// ordinary local binder, or a stale id) is silently skipped rather than
    /// erroring — the whole point is an intersection with `live`, not a
    /// membership requirement on `referenced`.
    #[test]
    fn seed_external_env_ignores_a_referenced_var_not_in_the_table() {
        let mut a: *mut u8 = std::ptr::null_mut();
        let sa = fake_slot(&mut a);
        let mut t = BindingTable::new();
        let x_var = VarId((0xFE << 56) | 1);
        let not_bound = VarId((0xFE << 56) | 99);
        t.bind(entry("x", 1, x_var.0, sa));

        let env = t.seed_external_env(&[x_var, not_bound]);
        assert_eq!(env.len(), 1);
        assert!(env.get(x_var).is_some());
        assert!(env.get(not_bound).is_none());
    }

    // -- scope frames (PRD 21 lane C2) ------------------------------------
    //
    // One named test per clause of locked decision 4, on the value plane.

    /// CHILDREN READ PARENT: a name bound only at ROOT resolves from a child,
    /// through the upward walk — and the child's own frame does not have to
    /// exist for that to work.
    #[test]
    fn a_child_resolves_a_parent_binding() {
        let mut a: *mut u8 = std::ptr::null_mut();
        let sa = fake_slot(&mut a);
        let mut tree = ScopeTree::new();
        let child = tree.mint_child(ScopeId::ROOT).expect("root is live");
        let mut t = BindingTable::new();

        let id = t.bind(entry("x", 1, (0xFE << 56) | 1, sa));
        assert_eq!(t.resolve_in(&tree, child, "x").expect("inherited").id, id);
        assert_eq!(
            t.resolve("x").expect("x at root").id,
            t.resolve_in(&tree, ScopeId::ROOT, "x")
                .expect("x at root")
                .id,
            "resolve(n) == resolve_in(ROOT, n)"
        );
        let grandchild = tree.mint_child(child).expect("child is live");
        assert_eq!(
            t.resolve_in(&tree, grandchild, "x").expect("inherited").id,
            id,
            "the walk goes all the way up, not one hop"
        );
    }

    /// SIBLINGS SHADOW FREELY AND NEVER COLLIDE: two sibling frames binding
    /// the SAME name hold different ids, each resolves its own, and neither is
    /// visible from the other.
    #[test]
    fn sibling_frames_bind_the_same_name_to_different_ids() {
        let mut a: *mut u8 = std::ptr::null_mut();
        let mut b: *mut u8 = std::ptr::null_mut();
        let (sa, sb) = (fake_slot(&mut a), fake_slot(&mut b));
        let mut tree = ScopeTree::new();
        let l = tree.mint_child(ScopeId::ROOT).expect("root is live");
        let r = tree.mint_child(ScopeId::ROOT).expect("root is live");
        let mut t = BindingTable::new();

        let l_id = t.bind_in(l, entry("helper", 1, (0xFE << 56) | 1, sa));
        let r_id = t.bind_in(r, entry("helper", 2, (0xFE << 56) | 2, sb));
        assert_ne!(l_id, r_id);

        assert_eq!(t.resolve_in(&tree, l, "helper").expect("l").id, l_id);
        assert_eq!(t.resolve_in(&tree, r, "helper").expect("r").id, r_id);
        // Neither sibling's binding leaks into the other, and the higher gen
        // does NOT win across frames — frames are disjoint maps, so the
        // newest-gen rule never even compares them.
        assert_eq!(t.scope_binding_count(l), 1);
        assert_eq!(t.scope_binding_count(r), 1);
        assert!(
            t.resolve("helper").is_none(),
            "and the parent gained neither"
        );
    }

    /// CHILDREN WRITE LOCALLY / THE PARENT NEVER GAINS A CHILD NAME: a child
    /// bind touches one frame, and nothing walks downward.
    #[test]
    fn a_child_bind_never_appears_in_the_parents_current_view() {
        let mut a: *mut u8 = std::ptr::null_mut();
        let mut b: *mut u8 = std::ptr::null_mut();
        let (sa, sb) = (fake_slot(&mut a), fake_slot(&mut b));
        let mut tree = ScopeTree::new();
        let child = tree.mint_child(ScopeId::ROOT).expect("root is live");
        let mut t = BindingTable::new();

        t.bind(entry("x", 1, (0xFE << 56) | 1, sa));
        let child_id = t.bind_in(child, entry("y", 2, (0xFE << 56) | 2, sb));

        let root_names: Vec<&str> = t.iter_current().map(|(n, _)| n.0.as_str()).collect();
        assert_eq!(root_names, vec!["x"], "the parent frame gained nothing");
        assert!(t.resolve("y").is_none());
        assert_eq!(t.scope_binding_count(ScopeId::ROOT), 1);

        // The child SEES both: its own local bind plus the inherited one.
        let visible: Vec<&str> = t
            .iter_current_in(&tree, child)
            .into_iter()
            .map(|(n, _)| n.0.as_str())
            .collect();
        assert_eq!(visible, vec!["x", "y"]);
        // ...and both are still live by id, which is what a compiled fragment
        // resolves through.
        assert!(t.get(child_id).is_some());
        assert_eq!(t.len(), 2);
    }

    /// A LOCAL BIND SHADOWS AN INHERITED ONE, without disturbing the parent's.
    #[test]
    fn a_local_bind_shadows_the_inherited_name_only_in_its_own_frame() {
        let mut a: *mut u8 = std::ptr::null_mut();
        let mut b: *mut u8 = std::ptr::null_mut();
        let (sa, sb) = (fake_slot(&mut a), fake_slot(&mut b));
        let mut tree = ScopeTree::new();
        let child = tree.mint_child(ScopeId::ROOT).expect("root is live");
        let mut t = BindingTable::new();

        let root_id = t.bind(entry("x", 1, (0xFE << 56) | 1, sa));
        let child_id = t.bind_in(child, entry("x", 2, (0xFE << 56) | 2, sb));

        assert_eq!(t.resolve_in(&tree, child, "x").expect("local").id, child_id);
        assert_eq!(t.resolve("x").expect("root").id, root_id);
        let visible = t.iter_current_in(&tree, child);
        assert_eq!(visible.len(), 1, "one `x`, the nearest one");
        assert_eq!(visible[0].1.id, child_id);
    }

    /// `remove_live` evicts from `live` AND from the owning frame. A shadowed
    /// older gen of the same name in the same scope is untouched: it stays
    /// live (fragments compiled against it still resolve by id) and does NOT
    /// resurrect into `current`.
    #[test]
    fn remove_live_evicts_from_live_and_frame_while_a_shadowed_older_gen_survives() {
        let mut a: *mut u8 = std::ptr::null_mut();
        let mut b: *mut u8 = std::ptr::null_mut();
        let (sa, sb) = (fake_slot(&mut a), fake_slot(&mut b));
        let mut t = BindingTable::new();

        let older = t.bind(entry("x", 1, (0xFE << 56) | 1, sa));
        let newer = t.bind(entry("x", 2, (0xFE << 56) | 2, sb));
        assert_eq!(t.resolve("x").expect("x").id, newer);

        let evicted = t.remove_live(newer).expect("newer was live");
        assert_eq!(evicted.id, newer);
        assert_eq!(evicted.scope, ScopeId::ROOT);
        assert!(t.get(newer).is_none(), "gone from live");
        assert!(t.resolve("x").is_none(), "gone from its frame");
        assert!(t.get(older).is_some(), "the shadowed older gen survives");
        assert_eq!(t.len(), 1);

        // Idempotent on an id that is no longer live.
        assert!(t.remove_live(newer).is_none());
        // And removing the OLDER (already-shadowed) id leaves `current` alone,
        // because the frame never named it.
        let mut c: *mut u8 = std::ptr::null_mut();
        let sc = fake_slot(&mut c);
        let fresh = t.bind(entry("x", 3, (0xFE << 56) | 3, sc));
        assert!(t.remove_live(older).is_some());
        assert_eq!(t.resolve("x").expect("x").id, fresh);
    }

    /// `drain_scope` takes ONE frame wholesale — every gen born in it,
    /// shadowed ones included — and leaves parent, sibling and child frames
    /// untouched. Retirement is by scope, never by shadowing.
    #[test]
    fn drain_scope_takes_one_frame_wholesale_and_leaves_the_others() {
        let mut a: *mut u8 = std::ptr::null_mut();
        let mut b: *mut u8 = std::ptr::null_mut();
        let mut c: *mut u8 = std::ptr::null_mut();
        let mut d: *mut u8 = std::ptr::null_mut();
        let (sa, sb, sc, sd) = (
            fake_slot(&mut a),
            fake_slot(&mut b),
            fake_slot(&mut c),
            fake_slot(&mut d),
        );
        let mut tree = ScopeTree::new();
        let target = tree.mint_child(ScopeId::ROOT).expect("root is live");
        let sib = tree.mint_child(ScopeId::ROOT).expect("root is live");
        let mut t = BindingTable::new();

        t.bind(entry("x", 1, (0xFE << 56) | 1, sa));
        t.bind_in(target, entry("y", 2, (0xFE << 56) | 2, sb));
        t.bind_in(target, entry("y", 3, (0xFE << 56) | 3, sc)); // shadows the above
        t.bind_in(sib, entry("y", 4, (0xFE << 56) | 4, sd));
        assert_eq!(t.len(), 4);
        assert_eq!(t.scope_binding_count(target), 1, "one current name");

        let drained = t.drain_scope(target);
        assert_eq!(drained.len(), 2, "BOTH gens born in that frame");
        assert!(drained.iter().all(|e| e.scope == target));
        assert_eq!(t.scope_binding_count(target), 0);
        assert!(t.resolve_in(&tree, target, "y").is_none());

        assert_eq!(t.len(), 2, "ROOT's x and the sibling's y remain");
        assert!(t.resolve("x").is_some());
        assert_eq!(t.scope_binding_count(sib), 1);
        assert!(t.resolve_in(&tree, sib, "y").is_some());
    }

    /// A retired (or never-minted) scope has an empty lookup chain, so a stale
    /// reference resolves NOTHING rather than silently falling back to ROOT.
    #[test]
    fn a_stale_scope_reference_resolves_nothing() {
        let mut a: *mut u8 = std::ptr::null_mut();
        let sa = fake_slot(&mut a);
        let mut tree = ScopeTree::new();
        let dead = tree.mint_child(ScopeId::ROOT).expect("root is live");
        let mut t = BindingTable::new();
        t.bind(entry("x", 1, (0xFE << 56) | 1, sa));

        assert!(t.resolve_in(&tree, dead, "x").is_some(), "live: inherited");
        tree.retire(dead);
        assert!(t.resolve_in(&tree, dead, "x").is_none());
        assert!(t.resolve_in(&tree, ScopeId(999), "x").is_none());
        assert!(t.iter_current_in(&tree, dead).is_empty());
    }
}
