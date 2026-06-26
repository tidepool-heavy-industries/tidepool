//! Session identifiers for the `tidepool-repl` planes (domain model §1–2).
//!
//! Newtypes — never bare `u64`/`String` — so the invariants (monotonic
//! generation, the single gen-versioned module-name string) live on the type.
//! Lane A (declaration accumulation) only needs [`Generation`], [`SessionId`],
//! [`BindingName`], and [`SessionModule`]; the value-plane ids (`SessionVarId`,
//! `VarKind`) are Wave-3 and deliberately omitted here.

use std::fmt;

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

#[cfg(test)]
mod tests {
    use super::*;

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
