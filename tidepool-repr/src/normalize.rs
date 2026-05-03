//! Core normalization pass: `CoreExpr` → `CoreExpr` canonicalization.
//!
//! Rationale: GHC Core post-optimizer shape varies across compilation modes.
//! Cross-module inlining via `resolveExternals` happens *after* GHC's
//! optimizer would normally collapse certain shapes (e.g. boxed primitive
//! Cons over `Lit(Word, n)`, `case unsafeEqualityProof of UnsafeRefl -> _`,
//! redundant DataCon wrappers). Consumers of [`CoreExpr`] (codegen,
//! `effect_machine`, `heap_bridge`) historically grew ad-hoc peeling
//! branches to handle the unoptimized variants. This pass centralizes
//! that canonicalization so each consumer can assert a single canonical
//! shape via `debug_assert!`.
//!
//! # Properties
//!
//! - **Idempotent.** `normalize(normalize(x)) == normalize(x)` for all `x`.
//! - **Semantics-preserving.** A normalized expression evaluates to the
//!   same value as the input under the JIT's evaluation rules.
//! - **Total.** Every well-formed [`CoreExpr`] has a canonical form.
//!
//! # Status
//!
//! At present the pass is the identity function. Rules are added by
//! follow-up work; see `docs/normalize-audit.md` for the inventory of
//! shapes that should be canonicalized.

use crate::{CoreExpr, DataConTable};

/// Canonicalize a [`CoreExpr`] by applying all normalization rules to
/// fixpoint.
///
/// Currently the identity function. The signature is stable so that
/// downstream callers (`tidepool-codegen`, `tidepool-runtime`) can wire
/// normalization into their compilation pipelines without further churn
/// when concrete rules are added.
pub fn normalize(expr: &CoreExpr, _table: &DataConTable) -> CoreExpr {
    // Rules are applied to fixpoint by future leaves. The identity-only
    // body trivially satisfies idempotence.
    expr.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Alt, AltCon, Literal, VarId};
    use crate::{CoreFrame, RecursiveTree};

    fn lit_int_tree(n: i64) -> CoreExpr {
        RecursiveTree {
            nodes: vec![CoreFrame::Lit(Literal::LitInt(n))],
        }
    }

    fn small_program() -> CoreExpr {
        // `case x of { 0# -> 1; _ -> x }`
        RecursiveTree {
            nodes: vec![
                CoreFrame::Var(VarId(1)),           // 0
                CoreFrame::Lit(Literal::LitInt(1)), // 1
                CoreFrame::Var(VarId(1)),           // 2
                CoreFrame::Case {
                    scrutinee: 0,
                    binder: VarId(2),
                    alts: vec![
                        Alt {
                            con: AltCon::LitAlt(Literal::LitInt(0)),
                            binders: vec![],
                            body: 1,
                        },
                        Alt {
                            con: AltCon::Default,
                            binders: vec![],
                            body: 2,
                        },
                    ],
                },
            ],
        }
    }

    #[test]
    fn identity_on_lit() {
        let table = DataConTable::new();
        let expr = lit_int_tree(42);
        assert_eq!(normalize(&expr, &table), expr);
    }

    #[test]
    fn identity_on_small_program() {
        let table = DataConTable::new();
        let expr = small_program();
        assert_eq!(normalize(&expr, &table), expr);
    }

    #[test]
    fn idempotent_on_lit() {
        let table = DataConTable::new();
        let expr = lit_int_tree(42);
        let once = normalize(&expr, &table);
        let twice = normalize(&once, &table);
        assert_eq!(once, twice);
    }

    #[test]
    fn idempotent_on_small_program() {
        let table = DataConTable::new();
        let expr = small_program();
        let once = normalize(&expr, &table);
        let twice = normalize(&once, &table);
        assert_eq!(once, twice);
    }
}
