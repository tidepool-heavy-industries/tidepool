//! Canonical Haskell language dialect for resident workbench modules.
//!
//! Frontends may add imports and effect rows, but they do not own competing
//! extension lists. Parse-only templates intentionally use a documented
//! subset because they never rename or typecheck.

/// LANGUAGE block for model-authored expressions and ordinary actor/session
/// workbench modules. No trailing newline; source assemblers add their own.
//
// `NoMonomorphismRestriction` is deliberately absent here: expression
// wrappers need the monomorphism restriction so their result binds specialize
// against the surrounding `do` block. Declaration modules add NMR below so
// authored top-level binds generalize.
pub const EVAL_PRAGMAS: &str = "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, UndecidableInstances, GADTs, KindSignatures, RankNTypes, PartialTypeSignatures, ScopedTypeVariables, ExtendedDefaultRules, LambdaCase, TupleSections, MultiWayIf, RecordWildCards, NamedFieldPuns, ViewPatterns, BangPatterns, TypeApplications, BlockArguments, NumericUnderscores, MultilineStrings, DeriveFunctor, DeriveFoldable, DeriveTraversable, DeriveGeneric, DeriveAnyClass, QuasiQuotes, DuplicateRecordFields, OverloadedRecordDot, OverloadedLabels #-}";

/// [`EVAL_PRAGMAS`] plus `NoMonomorphismRestriction` for persistent
/// declaration modules, where authored top-level bindings must generalize.
#[must_use]
pub fn declaration_pragmas() -> String {
    EVAL_PRAGMAS.replacen(
        "NoImplicitPrelude,",
        "NoImplicitPrelude, NoMonomorphismRestriction,",
        1,
    )
}

/// Declaration dialect for the standalone lens-free surface, whose only
/// intentional difference is relying on the implicit `Prelude` import.
#[must_use]
pub fn standalone_declaration_pragmas() -> String {
    declaration_pragmas().replacen("NoImplicitPrelude, ", "", 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_declaration_dialects_have_only_their_named_deltas() {
        let declarations = declaration_pragmas();
        assert!(declarations.contains("NoImplicitPrelude, NoMonomorphismRestriction,"));

        let standalone = standalone_declaration_pragmas();
        assert!(!standalone.contains("NoImplicitPrelude"));
        assert!(standalone.contains("NoMonomorphismRestriction"));
        assert!(standalone.contains("RankNTypes"));
    }
}
