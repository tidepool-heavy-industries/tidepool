//! Session binding table — `name → (valueId, root_slot, type_string)`.
//!
//! SCAFFOLD ONLY (Wave 0, component J). This freezes the shape the
//! tidepool-repl session manager (Wave 3) populates; the method bodies land in
//! Wave 3 (J1/J2 + I + K). See `plans/ghci-swarm-orchestration.md` §Wave 3 and
//! `plans/ghci-session-persistence.md` (round-3 H1/R3 — the two-layer naming).
//!
//! The two-layer design (Unison-derived, round-3): a mutable `name → valueId`
//! map over an append-only set of `valueId → root` entries. Rebinding a name
//! mints a *fresh* `valueId` and repoints only the name map, so old roots stay
//! reachable from captures and the `DataConTable::insert_checked` collision
//! guard is structurally never tripped. `valueId`s are tagged `0xFD` in the high
//! byte (parallel to externals' `0xFE` / the error sentinel's `0x45`), so the
//! existing high-byte Var dispatch in `emit/expr.rs` resolves them by adding one
//! arm. The counter must start high enough not to alias a real fingerprint
//! external id (round-3 open tension #2; audit vs `TIDEPOOL_VARID_AUDIT`).

use tidepool_repr::VarId;

/// One resolved session binding.
///
/// Placeholder field types (Wave 0): `value_id`/`root_slot`/`type_string` are
/// the three things a using fragment needs — the stable id GHC/JIT agree on, the
/// GC root holding the live heap value, and the captured GHC type used to inject
/// the synthetic `x :: T` decl so a later turn typechecks the reference.
pub struct BindingEntry {
    /// Stable, `0xFD`-tagged session value id. Minted fresh per bind; shared by
    /// the binding fragment and every later using fragment (unlike `localVarId`,
    /// which mixes in a session-local GHC unique and cannot be re-referenced).
    pub value_id: VarId,
    /// GC root slot holding the (tenured, strict-forced) heap value. Registered
    /// via [`crate::jit_machine::JitEffectMachine::register_persistent_root`] so
    /// the copying GC updates it in place across collections.
    ///
    /// # Safety invariant
    /// This slot carries the same GC-liveness contract as
    /// `register_persistent_root`: it must be non-null, point to a valid
    /// `*mut u8`, and stay valid + dereferenceable until the session machine is
    /// dropped — the GC rewrites `*root_slot` in place on every collection.
    /// Wave 3 code reading this field therefore needs `unsafe` and must uphold
    /// that invariant; do not dereference a slot from a dropped binding.
    pub root_slot: *mut *mut u8,
    /// Captured GHC type of the binding (component H), rendered to a string for
    /// the synthetic-decl typing path. `None` until type capture is wired.
    pub type_string: Option<String>,
}

/// Session binding table: the `name → (valueId, root_slot, type_string)` map.
///
/// Wave 0 freezes the surface; Wave 3 fills the bodies. The trait/struct split
/// is deliberately minimal — one concrete struct plus the operations the session
/// manager calls (resolve a name for a using fragment, bind/rebind on
/// `x <- e` / `let x = e`).
#[derive(Default)]
pub struct BindingTable {
    // Wave 3: a mutable `FxHashMap<String, BindingEntry>` over append-only roots.
    // Left empty in the scaffold so the type is constructible and Clone-free.
    _private: (),
}

impl BindingTable {
    /// Create an empty binding table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve a name to its current binding for a using fragment, or `None` if
    /// the name is not session-bound (the JIT then falls through to its normal
    /// Var resolution / unresolved-var trap).
    ///
    /// Wave 3 (J2): consulted in the seeded `external_env` build before emission.
    #[allow(unused_variables)]
    pub fn resolve(&self, name: &str) -> Option<&BindingEntry> {
        todo!("Wave 3: name → current BindingEntry (component J2)")
    }

    /// Bind (or rebind) `name` to a freshly strict-forced, tenured value rooted
    /// at `root_slot`, with captured type `type_string`. Mints a fresh `0xFD`
    /// `value_id` and repoints the name map; any prior entry's root stays live.
    ///
    /// Wave 3 (J1 + I): the `x <- e` / `let x = e` bind path.
    ///
    /// # Safety
    /// Carries the same GC-liveness contract as
    /// [`crate::jit_machine::JitEffectMachine::register_persistent_root`]: the
    /// caller guarantees `root_slot` is non-null, points to a valid `*mut u8`,
    /// and remains valid + dereferenceable until the session machine is dropped
    /// — the GC rewrites `*root_slot` in place on every collection while the
    /// binding lives. `unsafe` is frozen into the Wave-0 surface so Wave 3
    /// callers get a compile-time signal (upgrading a safe fn later would be a
    /// breaking change).
    #[allow(unused_variables)]
    pub unsafe fn bind(
        &mut self,
        name: &str,
        root_slot: *mut *mut u8,
        type_string: Option<String>,
    ) -> VarId {
        todo!("Wave 3: mint 0xFD valueId, repoint name map, keep old root (component J1)")
    }
}
