//! Single-source-of-truth for Rust↔Haskell "bridged records".
//!
//! A record type mirrored on both sides of the boundary (a Haskell result
//! record like `Commit` and its Rust wire-struct `GitCommit`) used to be
//! hand-written twice — the Haskell `data` decl and the Rust `#[derive(ToCore)]`
//! struct — with nothing tying field order/name/arity together. They drifted
//! silently (the `LspNode`→`Node` outage, friction #25).
//!
//! [`CoreRecord`] closes that class: the Rust struct is the source of truth and
//! the Haskell `data` decl is GENERATED from it (via the `CoreRecord` derive in
//! `tidepool-bridge-derive`). Every deriving type also registers itself in an
//! [`inventory`] so a generator can collect the whole set with one call.
//!
//! Field ORDER is the wire contract: `ToCore` builds the `Con` in Rust struct
//! field order and the extract assigns field positions from the Haskell `data`
//! decl order, so the generated decl's field order (= Rust struct order) is,
//! by construction, exactly the order the bridge encodes.

/// A Rust type that mirrors a Haskell record/enum and can render the exact
/// Haskell `data` declaration it corresponds to.
///
/// Derive it with `#[derive(CoreRecord)]` alongside `ToCore`/`FromCore`; the
/// derive resolves each Rust field type to its Haskell counterpart and applies
/// `#[core(hs = "...")]` / `#[core(hs_type = "...")]` field overrides.
pub trait CoreRecord {
    /// The Haskell `data` declaration this type mirrors, e.g.
    /// `data Commit = Commit { sha :: Text, ... } deriving (Show, Eq)`.
    fn haskell_decl() -> String;
}

/// One bridged record, registered via [`inventory`] so [`all_record_decls`]
/// can gather every decl regardless of which crate defines the type.
pub struct RegisteredRecord {
    /// The Haskell type name — a stable sort key for deterministic output.
    pub name: &'static str,
    /// Renders the Haskell `data` declaration (`<T as CoreRecord>::haskell_decl`).
    pub decl: fn() -> String,
}

inventory::collect!(RegisteredRecord);

/// Every registered bridged-record decl, sorted by Haskell type name so the
/// output is deterministic (inventory link order is unspecified).
pub fn all_record_decls() -> Vec<String> {
    let mut items: Vec<&'static RegisteredRecord> =
        inventory::iter::<RegisteredRecord>.into_iter().collect();
    items.sort_by_key(|r| r.name);
    items.into_iter().map(|r| (r.decl)()).collect()
}
