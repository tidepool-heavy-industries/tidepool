//! Session identifiers for the persistent declaration environment and binding
//! store in `tidepool-runtime`.
//!
//! Newtypes — never bare `u64`/`String` — so the invariants (monotonic
//! generation, the single gen-versioned module-name string) live on the type.
//! Declaration accumulation needs [`Generation`], [`SessionId`],
//! [`BindingName`], and [`SessionModule`]; the binding-store id
//! [`SessionVarId`] is the bridge between GHC's type-checking side and the
//! JIT's persistent binding store.

use std::fmt;

use crate::types::VarId;

/// Runtime authority installed for one execution entry. The pair is opaque to
/// Haskell: Rust handlers use it to distinguish an exact actor incarnation
/// from a later incarnation with the same stable identity.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct PrincipalId {
    pub identity: u64,
    pub incarnation: u64,
}

impl PrincipalId {
    /// Authority used by non-actor compatibility paths during migration.
    pub const SYSTEM: Self = Self {
        identity: 0,
        incarnation: 0,
    };

    #[must_use]
    pub const fn new(identity: u64, incarnation: u64) -> Self {
        Self {
            identity,
            incarnation,
        }
    }
}

/// Monotonic per-session generation counter (= GHCi's `ic_mod_index`).
///
/// Only ever bumped. `Generation(0)` is the empty session — no `Lib`/`Val`
/// module exists yet; the first declaration mints `Generation(1)`.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Generation(pub u64);

impl Generation {
    /// The next generation. Generations are only ever bumped, never reused.
    #[must_use]
    pub fn next(self) -> Generation {
        Generation(self.0 + 1)
    }
}

impl fmt::Display for Generation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Identity of a session. Distinct sessions never share cache entries or
/// session-library directories.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct SessionId(pub u64);

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The user-facing name of a binding ("x"). Distinct from any internal id.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct BindingName(pub String);

/// Which session module family a gen-versioned module belongs to.
///
/// - `Lib`: user-written declarations, accumulated as source text.
/// - `Val`: synthesized value-binding ifaces; construction lives in
///   `tidepool-runtime`, not in this crate.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SessionModuleKind {
    Val,
    Lib,
}

impl SessionModuleKind {
    fn tag(self) -> &'static str {
        match self {
            SessionModuleKind::Val => "Val",
            SessionModuleKind::Lib => "Lib",
        }
    }
}

/// A gen-versioned session module. **The one place** the module-name string
/// `"Tidepool.Session.{Val|Lib}.G<g>"` is constructed — render through this type
/// so no bare module strings drift across the codebase.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct SessionModule {
    pub kind: SessionModuleKind,
    pub gen: Generation,
}

impl SessionModule {
    /// A `Tidepool.Session.Lib.G<g>` declaration module.
    #[must_use]
    pub fn lib(gen: Generation) -> SessionModule {
        SessionModule {
            kind: SessionModuleKind::Lib,
            gen,
        }
    }

    /// A `Tidepool.Session.Val.G<g>` value-iface module.
    #[must_use]
    pub fn val(gen: Generation) -> SessionModule {
        SessionModule {
            kind: SessionModuleKind::Val,
            gen,
        }
    }

    /// The generation this module was minted at — the shadowing comparator
    /// (`BindingTable::bind`'s newest-gen-wins is a GEN comparison, not
    /// insertion order, because materialization can arrive out of mint order
    /// under any-order resume).
    #[must_use]
    pub fn gen(&self) -> Generation {
        self.gen
    }

    /// The fully-qualified module name, e.g. `"Tidepool.Session.Lib.G3"`.
    #[must_use]
    pub fn module_name(&self) -> String {
        format!("Tidepool.Session.{}.G{}", self.kind.tag(), self.gen.0)
    }

    /// The on-disk `.hs` file name relative to the session include dir,
    /// mirroring the module name's final component (`G3.hs`). The session
    /// dir mirrors the `Tidepool/Session/{Val,Lib}/` package directory layout.
    #[must_use]
    pub fn relative_hs_path(&self) -> String {
        format!("Tidepool/Session/{}/G{}.hs", self.kind.tag(), self.gen.0)
    }

    /// The on-disk `.hi` iface path relative to a `--session-root` dir,
    /// mirroring GHC's own `hiDir` layout (dots become slashes) — the Rust
    /// twin of Haskell's `Tidepool.Session.sessionHiPath`. Used by the
    /// compile memo to content-fingerprint a
    /// stable `--inject-val` module's iface, the same way an `--include`
    /// root is fingerprinted.
    #[must_use]
    pub fn relative_hi_path(&self) -> String {
        format!("Tidepool/Session/{}/G{}.hi", self.kind.tag(), self.gen.0)
    }
}

impl fmt::Display for SessionModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.module_name())
    }
}

/// The stable `VarId` of a session value binder — `Tidepool.Session.Val.G<g>.x`.
///
/// Always `0xFE`-tagged (a real external). The formula is
/// `Translate.stableVarId`'s `0xFE<<56 | fingerprintString("<module>:<occ>").hi64`
/// rule (`bridge/haskell/src/Tidepool/Identity.hs`). Most binders still carry
/// an id **minted exactly once, in the Haskell extract**, on the bind turn's
/// `BoundBinder.var_id`, which Rust stores and re-seeds into the `ExternalEnv`
/// for later reference turns without recomputing anything.
///
/// A binder minted for a host carrier's hand-written `Val.G<g>` source stub
/// (`tidepool-runtime`'s `HostCarrier::mount_carrier_in`, no bind turn, no
/// extract compile) has no extract mint to carry — Rust computes its id
/// directly with `tidepool_codegen::prepared_program::session_var_id`, which
/// reproduces `fingerprintString` bit-for-bit over GHC's own MD5 kernel
/// (`tidepool/codegen/src/prepared_program/md5_kernel.rs`) and is checked
/// against a real extract mint by a parity test
/// (`tidepool/runtime/tests/prepared_residency.rs`). Both mints agree because
/// they hash the identical `"<module>:<occ>"` string with the identical
/// algorithm — there is no cross-language drift risk, extract-minted or
/// Rust-minted.
///
/// Both a bind turn (the binder's `Name` in the synthesized `Val.G<g>` iface)
/// and every later reference turn (the imported `Name`, whether from that
/// injected iface or from a hand-written source stub compiled as a home
/// module) hash `"<module>:<occ>"` identically, so the reference Core's
/// `NVar` matches the stored id by raw equality — the persistent-binding key.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct SessionVarId(VarId);

impl SessionVarId {
    /// Wrap the extract-minted stable id (the `BoundBinder.var_id` raw `u64`).
    #[must_use]
    pub fn from_extract(raw: u64) -> SessionVarId {
        SessionVarId(VarId(raw))
    }

    /// Wrap an already-built [`VarId`] (e.g. from a fixture).
    #[must_use]
    pub fn from_var(var: VarId) -> SessionVarId {
        SessionVarId(var)
    }

    /// The underlying [`VarId`] — what seeds the `ExternalEnv` and what a
    /// reference turn's Core `NVar` carries.
    #[must_use]
    pub fn var(self) -> VarId {
        self.0
    }

    /// The raw 64-bit id.
    #[must_use]
    pub fn raw(self) -> u64 {
        self.0 .0
    }
}

impl fmt::Display for SessionVarId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:016x}", self.raw())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_var_id_is_external_tagged_when_minted_by_extract() {
        // A faithful extract-minted id (0xFE high byte); Rust stores it verbatim.
        let raw = (0xFEu64 << 56) | 0x0123456789abcd;
        let id = SessionVarId::from_extract(raw);
        assert_eq!(id.raw(), raw);
        assert_eq!(id.var().kind(), crate::types::VarKind::External);
    }

    #[test]
    fn module_name_is_gen_versioned() {
        assert_eq!(
            SessionModule::lib(Generation(3)).module_name(),
            "Tidepool.Session.Lib.G3"
        );
        assert_eq!(
            SessionModule::val(Generation(0)).module_name(),
            "Tidepool.Session.Val.G0"
        );
    }

    #[test]
    fn relative_path_mirrors_module() {
        assert_eq!(
            SessionModule::lib(Generation(7)).relative_hs_path(),
            "Tidepool/Session/Lib/G7.hs"
        );
    }

    #[test]
    fn relative_hi_path_mirrors_module() {
        assert_eq!(
            SessionModule::val(Generation(0)).relative_hi_path(),
            "Tidepool/Session/Val/G0.hi"
        );
    }

    #[test]
    fn generation_is_monotonic() {
        assert_eq!(Generation(0).next(), Generation(1));
        assert!(Generation(1) > Generation(0));
    }
}
