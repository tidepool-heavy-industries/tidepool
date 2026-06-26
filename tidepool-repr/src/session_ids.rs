//! Session identifiers for the `tidepool-repl` planes (domain model §1–2).
//!
//! Newtypes — never bare `u64`/`String` — so the invariants (monotonic
//! generation, the single gen-versioned module-name string) live on the type.
//! Lane A (declaration accumulation) needs [`Generation`], [`SessionId`],
//! [`BindingName`], and [`SessionModule`]; the value-plane id [`SessionVarId`]
//! is the Wave-3b bridge between the GHC type plane and the JIT value plane.

use std::fmt;

use crate::types::VarId;

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

/// Which session plane a gen-versioned module belongs to.
///
/// - `Lib`: user-written declarations (Lane A) accumulated as source text.
/// - `Val`: synthesized value-binding ifaces (Option C, Wave 3 — not built here).
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
/// so no bare module strings drift across the codebase (domain model §2).
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

    /// A `Tidepool.Session.Val.G<g>` value-iface module (Wave 3).
    #[must_use]
    pub fn val(gen: Generation) -> SessionModule {
        SessionModule {
            kind: SessionModuleKind::Val,
            gen,
        }
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
}

impl fmt::Display for SessionModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.module_name())
    }
}

/// The stable `VarId` of a session value binder — `Tidepool.Session.Val.G<g>.x`.
///
/// Always `0xFE`-tagged (a real external under Option C). **The hash is minted
/// exactly once, in the Haskell extract** (`Translate.stableVarId`, the
/// `0xFE<<56 | fingerprintString("<module>:<occ>").hi64` rule), and carried to
/// Rust on the bind turn's `BoundBinder.var_id`. Rust **stores** that id and
/// re-seeds it into the `ExternalEnv` for later reference turns — it never
/// recomputes the MD5 fingerprint, so there is no cross-language drift risk
/// (this is the deliberate single-source refinement of domain model §3's
/// `SessionVarId::of`: the one place is the Haskell stable-id rule).
///
/// Both the bind turn (the binder's `Name` in the synthesized `Val.G<g>` iface)
/// and every later reference turn (the imported `Name` from that injected iface)
/// hash `"<module>:<occ>"` identically, so the reference Core's `NVar` matches
/// the stored id by raw equality — the value-plane key.
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
    fn generation_is_monotonic() {
        assert_eq!(Generation(0).next(), Generation(1));
        assert!(Generation(1) > Generation(0));
    }
}
