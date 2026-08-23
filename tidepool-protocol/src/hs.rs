//! The closed Haskell type language.
//!
//! Every Haskell type that appears anywhere in the effect contract today — each
//! verb argument, each result, each error-ADT field, across all twenty effects —
//! falls inside this enum. It is deliberately CLOSED: a type that cannot be
//! spelled here is not smuggled in as a string, it becomes a reviewed schema
//! feature (PRD 22's first hard rule).
//!
//! Two renderings, because Haskell needs two and the current hand-written
//! strings switch between them by convention:
//!
//! - [`HsType::render`] — bare. Correct in ARROW-argument position, where
//!   application binds tighter than `->`: `Text -> Maybe Value -> Value -> …`.
//! - [`HsType::render_app_arg`] — parenthesized when compound. Correct in
//!   TYPE-APPLICATION-argument position, where a bare `Maybe Text` would be
//!   read as two arguments: `Exec (Either ExecError Proc)`, `Meta (Maybe (Int,
//!   Int))`.
//!
//! Getting this right by structure is the point. In the hand-written registry
//! the parentheses are remembered per call site, which is exactly the kind of
//! by-convention protocol PRD 22 exists to delete.

/// One Haskell type in the effect contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HsType {
    /// `()` — the result of a verb that returns nothing.
    Unit,
    /// `Text`.
    Text,
    /// `Int`.
    Int,
    /// `Bool`.
    Bool,
    /// `Value` — the vendored aeson JSON value.
    Value,
    /// A named type resolved elsewhere: a bridged record (`Proc`, `Commit`), a
    /// type declared in the effect's own `type_defs` (`FileRead`), or a type
    /// from the Haskell stdlib (`UTCTime`).
    Named(&'static str),
    /// A type VARIABLE (`v`, `a`) — only parameterized effects have these.
    Var(&'static str),
    /// `[T]`.
    List(Box<HsType>),
    /// `Maybe T`.
    Maybe(Box<HsType>),
    /// `Either E A`.
    Either(Box<HsType>, Box<HsType>),
    /// `(A, B, …)`.
    Tuple(Vec<HsType>),
}

impl HsType {
    /// `[T]`.
    #[must_use]
    pub fn list(inner: HsType) -> Self {
        HsType::List(Box::new(inner))
    }

    /// `Maybe T`.
    #[must_use]
    pub fn maybe(inner: HsType) -> Self {
        HsType::Maybe(Box::new(inner))
    }

    /// `Either E A`.
    #[must_use]
    pub fn either(err: HsType, ok: HsType) -> Self {
        HsType::Either(Box::new(err), Box::new(ok))
    }

    /// Bare rendering. Use in arrow-argument position and at the top level.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            HsType::Unit => "()".to_string(),
            HsType::Text => "Text".to_string(),
            HsType::Int => "Int".to_string(),
            HsType::Bool => "Bool".to_string(),
            HsType::Value => "Value".to_string(),
            HsType::Named(n) | HsType::Var(n) => (*n).to_string(),
            HsType::List(t) => format!("[{}]", t.render()),
            HsType::Maybe(t) => format!("Maybe {}", t.render_app_arg()),
            HsType::Either(e, a) => {
                format!("Either {} {}", e.render_app_arg(), a.render_app_arg())
            }
            HsType::Tuple(ts) => {
                let inner: Vec<String> = ts.iter().map(HsType::render).collect();
                format!("({})", inner.join(", "))
            }
        }
    }

    /// Rendering for type-application-argument position: parenthesized exactly
    /// when the type is an application itself.
    ///
    /// `[T]` and tuples are already self-delimiting, so they are NOT wrapped —
    /// which is what makes `Meta (Maybe (Int, Int))` come out with two levels
    /// of parens rather than three.
    #[must_use]
    pub fn render_app_arg(&self) -> String {
        match self {
            HsType::Maybe(_) | HsType::Either(_, _) => format!("({})", self.render()),
            _ => self.render(),
        }
    }
}

/// Render a curried Haskell signature: `A -> B -> <head> <result>`.
///
/// `args` are rendered bare (arrow-argument position); `result` is rendered as
/// a type-application argument, because it is applied to `head` (`Exec`, or the
/// `M` alias in a helper signature).
#[must_use]
pub fn render_signature(args: &[HsType], head: &str, result: &HsType) -> String {
    let mut out = String::new();
    for a in args {
        out.push_str(&a.render());
        out.push_str(" -> ");
    }
    out.push_str(head);
    out.push(' ');
    out.push_str(&result.render_app_arg());
    out
}

/// Render a ROW-POLYMORPHIC curried Haskell signature: `forall effs. Member
/// <effect> effs => A -> B -> Eff effs <result>` — the stable-effects-core
/// shape every migrated effect's helper now uses instead of a concrete `M`
/// head, so the helper's compiled body can live in the vocabulary-only,
/// session-stable `Tidepool.Effects.Core` module (which has no `M` alias of
/// its own to write against — `M` is a per-agent-session shim concept).
#[must_use]
pub fn render_member_signature(args: &[HsType], effect: &str, result: &HsType) -> String {
    let mut out = format!("forall effs. Member {effect} effs => ");
    for a in args {
        out.push_str(&a.render());
        out.push_str(" -> ");
    }
    out.push_str("Eff effs ");
    out.push_str(&result.render_app_arg());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The closed language must reproduce the type strings the hand-written
    /// registry uses today — BYTE for byte, and across every effect, not just
    /// the migrated one. This table is the evidence that the language is
    /// actually closed, and it is what licenses a later lane to describe its
    /// effect's types here instead of as strings.
    ///
    /// Sourced by census of every `args`/`ret`/error-`fields` type string in
    /// `tidepool-mcp/src/effect_defs.rs`.
    #[test]
    fn closed_language_reproduces_every_registry_type_string() {
        // (type, bare rendering, application-argument rendering)
        let cases: Vec<(HsType, &str, &str)> = vec![
            (HsType::Unit, "()", "()"),
            (HsType::Text, "Text", "Text"),
            (HsType::Int, "Int", "Int"),
            (HsType::Bool, "Bool", "Bool"),
            (HsType::Value, "Value", "Value"),
            (HsType::Named("Proc"), "Proc", "Proc"),
            (HsType::Named("Commit"), "Commit", "Commit"),
            (HsType::Var("a"), "a", "a"),
            (HsType::list(HsType::Text), "[Text]", "[Text]"),
            (
                HsType::list(HsType::Named("Commit")),
                "[Commit]",
                "[Commit]",
            ),
            (
                HsType::list(HsType::Named("StatusEntry")),
                "[StatusEntry]",
                "[StatusEntry]",
            ),
            (
                HsType::list(HsType::Named("FileDelta")),
                "[FileDelta]",
                "[FileDelta]",
            ),
            (HsType::list(HsType::Named("Hit")), "[Hit]", "[Hit]"),
            (
                HsType::list(HsType::Named("FileRead")),
                "[FileRead]",
                "[FileRead]",
            ),
            (
                HsType::list(HsType::Named("LspNode")),
                "[LspNode]",
                "[LspNode]",
            ),
            (HsType::list(HsType::Named("Diag")), "[Diag]", "[Diag]"),
            (HsType::list(HsType::Named("Watch")), "[Watch]", "[Watch]"),
            (
                HsType::list(HsType::Named("RepositoryEvent")),
                "[RepositoryEvent]",
                "[RepositoryEvent]",
            ),
            (
                HsType::list(HsType::Named("WorktreeSummary")),
                "[WorktreeSummary]",
                "[WorktreeSummary]",
            ),
            // Maybe: bare in arrow position, parenthesized applied to the head.
            (HsType::maybe(HsType::Text), "Maybe Text", "(Maybe Text)"),
            (HsType::maybe(HsType::Value), "Maybe Value", "(Maybe Value)"),
            (
                HsType::maybe(HsType::Named("FileMeta")),
                "Maybe FileMeta",
                "(Maybe FileMeta)",
            ),
            (
                HsType::maybe(HsType::Named("LspNode")),
                "Maybe LspNode",
                "(Maybe LspNode)",
            ),
            // Tuples are self-delimiting: no second layer of parens.
            (
                HsType::maybe(HsType::Tuple(vec![HsType::Int, HsType::Int])),
                "Maybe (Int, Int)",
                "(Maybe (Int, Int))",
            ),
            (
                HsType::list(HsType::Tuple(vec![HsType::Text, HsType::Int])),
                "[(Text, Int)]",
                "[(Text, Int)]",
            ),
            // Either, including the two awkward shapes in the registry today.
            (
                HsType::either(HsType::Named("ExecError"), HsType::Named("Proc")),
                "Either ExecError Proc",
                "(Either ExecError Proc)",
            ),
            (
                HsType::either(
                    HsType::Named("GitError"),
                    HsType::list(HsType::Named("Commit")),
                ),
                "Either GitError [Commit]",
                "(Either GitError [Commit])",
            ),
            (
                HsType::either(HsType::Value, HsType::Unit),
                "Either Value ()",
                "(Either Value ())",
            ),
            (
                HsType::either(HsType::maybe(HsType::Text), HsType::Unit),
                "Either (Maybe Text) ()",
                "(Either (Maybe Text) ())",
            ),
        ];
        for (ty, bare, applied) in cases {
            assert_eq!(ty.render(), bare, "bare rendering of {ty:?}");
            assert_eq!(
                ty.render_app_arg(),
                applied,
                "application-argument rendering of {ty:?}"
            );
        }
    }

    /// The three Exec constructor signatures, as the hand-written registry
    /// spells them today.
    #[test]
    fn signature_rendering_matches_exec_constructors() {
        let ok = HsType::either(HsType::Named("ExecError"), HsType::Named("Proc"));
        assert_eq!(
            render_signature(&[HsType::Text], "Exec", &ok),
            "Run :: Text -> Exec (Either ExecError Proc)"
                .split_once(":: ")
                .unwrap()
                .1
        );
        assert_eq!(
            render_signature(&[HsType::Text, HsType::Text], "Exec", &ok),
            "Text -> Text -> Exec (Either ExecError Proc)"
        );
        assert_eq!(
            render_signature(&[HsType::list(HsType::Text)], "Exec", &ok),
            "[Text] -> Exec (Either ExecError Proc)"
        );
        // A nullary verb renders with no arrows at all (GitStatus's shape).
        assert_eq!(
            render_signature(&[], "Git", &HsType::list(HsType::Named("StatusEntry"))),
            "Git [StatusEntry]"
        );
    }
}
