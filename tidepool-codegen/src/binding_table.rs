//! Session binding table — the bridge GHC already uses, made concrete.
//!
//! GHCi splits a binding's identity into a *type* half (`ic_tythings`) and a
//! *value* half (the linker's `closure_env`), keyed by one `Name`. Our
//! [`BindingTable`] is exactly that bridge: keyed by one [`SessionVarId`], the
//! type half is the thin `Tidepool.Session.Val.G<g>` iface on disk (GHC's plane)
//! and the value half is the live, GC-rooted [`BoundValue`] in the resident
//! machine's heap (the JIT's plane).
//!
//! ## The two-layer shape (domain model §4; kimi-r2 #9 rewrite)
//!
//! A mutable `name → SessionVarId` map (`current`) over an append-only set of
//! `SessionVarId → BindingEntry` (`live`). Rebinding a name mints a *fresh*
//! `SessionVarId` (a new `Val.G<g'>` module → a new `stableVarId`) and repoints
//! only `current`, so old roots stay reachable from captures in
//! already-compiled fragments and the `DataConTable::insert_checked` collision
//! guard is structurally never tripped. This REVISES the Wave-0 scaffold's
//! `0xFD` counter-minted id + `type_string` field: under Option C the
//! gen-versioned module name already yields a fresh, collision-free `0xFE`
//! external id per (re)bind, and the structured type lives in the `.hi`, not a
//! string. The `var_id` is minted by the Haskell extract (`Translate.stableVarId`)
//! and stored here verbatim (see [`SessionVarId`]).

use std::collections::HashMap;

use tidepool_repr::{BindingName, SessionModule, SessionVarId};

use crate::emit::ExternalEnv;
use crate::old_space::RootSlot;

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
}

/// The `name → (SessionVarId, RootSlot, SessionModule)` bridge (domain §4).
///
/// `current` maps a name to its newest binding's id (shadowing: latest-wins);
/// `live` retains EVERY still-rooted binding, including shadowed older ones, so
/// fragments compiled against an old gen keep resolving. Entries live until the
/// session machine drops (then the persistent roots are reclaimed wholesale).
#[derive(Default)]
pub struct BindingTable {
    /// Shadowing layer: name → newest gen's id.
    current: HashMap<BindingName, SessionVarId>,
    /// Append-only-by-id store of every live binding (old gens retained).
    live: HashMap<SessionVarId, BindingEntry>,
}

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
        let id = entry.id;
        self.current.insert(entry.name.clone(), id);
        self.live.insert(id, entry);
        id
    }

    /// Drop `name` from the CURRENT map so `iter_current`/`resolve` no longer
    /// see it (its `live` entry + root are retained for fragments compiled
    /// against the old gen). Used when a pure decl of the same name supersedes
    /// a materialized value binding (cross-plane shadow, GHCi-environment
    /// model). No-op if `name` isn't current.
    pub fn remove_current(&mut self, name: &str) {
        self.current.remove(&BindingName(name.to_string()));
    }

    /// Resolve a name to its CURRENT binding (newest gen), or `None` if the name
    /// is not session-bound (the caller then falls through to normal Var
    /// resolution / the unresolved-var trap).
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<&BindingEntry> {
        let id = self.current.get(&BindingName(name.to_string()))?;
        self.live.get(id)
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

    /// The current (newest) bindings, as `(name, entry)` pairs — the `:bindings`
    /// view (shadowed older gens are excluded).
    pub fn iter_current(&self) -> impl Iterator<Item = (&BindingName, &BindingEntry)> {
        self.current
            .iter()
            .filter_map(|(name, id)| self.live.get(id).map(|e| (name, e)))
    }

    /// The set of live `Val.G<g>` modules to inject (`SessionScope.ssValIfaces`)
    /// before compiling a reference turn — one per still-rooted binding.
    pub fn live_modules(&self) -> impl Iterator<Item = SessionModule> + '_ {
        self.live.values().map(|e| e.module)
    }

    /// Build the `ExternalEnv` the JIT consults at a Var-miss: every live
    /// binding's `SessionVarId → RootSlot` slot address. The Var-miss site emits
    /// a per-fragment `load` through the slot to read the GC-current pointer, so
    /// seeding the slot *address* (never a snapshot pointer) is what keeps the
    /// read GC-safe.
    #[must_use]
    pub fn seed_external_env(&self) -> ExternalEnv {
        let mut env = ExternalEnv::new();
        for entry in self.live.values() {
            env.insert(entry.id.var(), entry.value.root().addr());
        }
        env
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

    #[test]
    fn seed_external_env_covers_every_live_binding() {
        let mut a: *mut u8 = std::ptr::null_mut();
        let mut b: *mut u8 = std::ptr::null_mut();
        let (sa, sb) = (fake_slot(&mut a), fake_slot(&mut b));
        let mut t = BindingTable::new();
        t.bind(entry("x", 1, (0xFE << 56) | 1, sa));
        t.bind(entry("y", 1, (0xFE << 56) | 3, sb));

        let env = t.seed_external_env();
        assert_eq!(env.len(), 2);
        assert!(env.get(VarId((0xFE << 56) | 1)).is_some());
        assert!(env.get(VarId((0xFE << 56) | 3)).is_some());
    }
}
