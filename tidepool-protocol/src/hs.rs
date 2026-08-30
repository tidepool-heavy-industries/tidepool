//! The closed Haskell type language.
//!
//! Every Haskell type that appears anywhere in the effect contract today — each
//! verb argument, each result, each error-ADT field, across all twenty effects —
//! falls inside this enum. It is deliberately CLOSED: a type that cannot be
//! spelled here is not smuggled in as a string, it becomes a reviewed schema
//! feature.
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
//! by-convention protocol this schema exists to delete.

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
    /// A named type constructor or saturated type resolved elsewhere: a
    /// bridged record (`Proc`, `Commit`), a
    /// type declared in the effect's own `type_defs` (`FileRead`), or a type
    /// from the Haskell stdlib (`UTCTime`).
    Named(&'static str),
    /// A type VARIABLE (`v`, `a`) — only parameterized effects have these.
    Var(&'static str),
    /// General type application, `F A`. Most common constructors retain
    /// dedicated variants below because their structure matters elsewhere;
    /// this is for genuinely higher-kinded contract types such as
    /// `Eff bodyEffs ()`.
    App(Box<HsType>, Box<HsType>),
    /// `[T]`.
    List(Box<HsType>),
    /// `Maybe T`.
    Maybe(Box<HsType>),
    /// `Either E A`.
    Either(Box<HsType>, Box<HsType>),
    /// `(A, B, …)`.
    Tuple(Vec<HsType>),
    /// `A -> B` — a function type. Green's async body is currently the sole
    /// function-typed field in the effect contract.
    Fn(Box<HsType>, Box<HsType>),
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

    /// `A -> B`.
    #[must_use]
    pub fn func(arg: HsType, ret: HsType) -> Self {
        HsType::Fn(Box::new(arg), Box::new(ret))
    }

    /// `F A`.
    #[must_use]
    pub fn app(head: HsType, arg: HsType) -> Self {
        HsType::App(Box::new(head), Box::new(arg))
    }

    /// Bare rendering. Use in arrow-argument position and at the top level —
    /// EXCEPT for an argument that is itself [`HsType::Fn`], which still
    /// needs parenthesizing there (`->` is right-associative, so an
    /// unparenthesized function-typed argument reassociates into a longer
    /// curried chain instead of one higher-order argument) — see
    /// [`render_signature`], the one caller with a function-typed argument.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            HsType::Unit => "()".to_string(),
            HsType::Text => "Text".to_string(),
            HsType::Int => "Int".to_string(),
            HsType::Bool => "Bool".to_string(),
            HsType::Value => "Value".to_string(),
            HsType::Named(n) | HsType::Var(n) => (*n).to_string(),
            HsType::App(f, x) => format!("{} {}", f.render_app_head(), x.render_app_arg()),
            HsType::List(t) => format!("[{}]", t.render()),
            HsType::Maybe(t) => format!("Maybe {}", t.render_app_arg()),
            HsType::Either(e, a) => {
                format!("Either {} {}", e.render_app_arg(), a.render_app_arg())
            }
            HsType::Tuple(ts) => {
                let inner: Vec<String> = ts.iter().map(HsType::render).collect();
                format!("({})", inner.join(", "))
            }
            HsType::Fn(a, b) => format!("{} -> {}", a.render_arrow_left(), b.render()),
        }
    }

    /// Rendering for type-application-argument position: parenthesized when
    /// required to keep the argument grouped as one type.
    ///
    /// `[T]` and tuples are already self-delimiting, so they are NOT wrapped —
    /// which is what makes `Meta (Maybe (Int, Int))` come out with two levels
    /// of parens rather than three.
    #[must_use]
    pub fn render_app_arg(&self) -> String {
        match self {
            HsType::App(_, _) | HsType::Maybe(_) | HsType::Either(_, _) | HsType::Fn(_, _) => {
                format!("({})", self.render())
            }
            _ => self.render(),
        }
    }

    fn render_app_head(&self) -> String {
        match self {
            HsType::Fn(_, _) => format!("({})", self.render()),
            _ => self.render(),
        }
    }

    /// Rendering for the LEFT side of an outer `->`: parenthesized exactly
    /// when `self` is itself [`HsType::Fn`] (`(A -> B) -> C`, never `A -> B
    /// -> C`, which would parse as `A -> (B -> C)`). Every other shape is
    /// already unambiguous there, same as [`Self::render`].
    fn render_arrow_left(&self) -> String {
        match self {
            HsType::Fn(_, _) => format!("({})", self.render()),
            _ => self.render(),
        }
    }
}

/// Render a curried Haskell signature: `A -> B -> <head> <result>`.
///
/// `args` are rendered bare (arrow-argument position) — EXCEPT a function-typed
/// argument, which still needs parenthesizing there (see
/// [`HsType::render_arrow_left`]'s doc); `result` is rendered as a
/// type-application argument, because it is applied to `head` (`Exec`, or the
/// `M` alias in a helper signature).
#[must_use]
pub fn render_signature(args: &[HsType], head: &str, result: &HsType) -> String {
    let mut out = String::new();
    for a in args {
        out.push_str(&a.render_arrow_left());
        out.push_str(" -> ");
    }
    out.push_str(head);
    out.push(' ');
    out.push_str(&result.render_app_arg());
    out
}

/// Render a row-polymorphic curried Haskell signature: `forall effs. Member
/// <effect> effs => A -> B -> Eff effs <result>`. Generated helpers use this
/// shape so their bodies live in universal `Tidepool.Effects.Core`; `M`
/// remains an actor-local shim alias.
#[must_use]
pub fn render_member_signature(args: &[HsType], effect: &str, result: &HsType) -> String {
    render_member_signature_with(&[], args, effect, result)
}

/// [`render_member_signature`], generalized for an OPAQUE substrate helper
/// whose `Member` row entry is a PARAMETERIZED effect head (`Finalize v`,
/// needing `Member (Finalize v) effs`, not `Member Finalize v effs`) and/or
/// which forall's its own extra type variables ahead of `effs` (`finalize ::
/// forall v a effs. …` — `v` doubles as the head's own applied parameter,
/// `a` is free).
///
/// `effect_head` is parenthesized in the `Member` clause exactly when it is
/// an application (contains a space) — the same rule
/// [`HsType::render_app_arg`] applies to a real `HsType`, spelled out here
/// because the head is a plain rendered string (`Effect::head()`), not an
/// `HsType`.
#[must_use]
pub fn render_member_signature_with(
    extra_tyvars: &[&str],
    args: &[HsType],
    effect_head: &str,
    result: &HsType,
) -> String {
    let mut out = String::from("forall ");
    for tv in extra_tyvars {
        out.push_str(tv);
        out.push(' ');
    }
    out.push_str("effs. Member ");
    out.push_str(&paren_if_applied(effect_head));
    out.push_str(" effs => ");
    for a in args {
        out.push_str(&a.render_arrow_left());
        out.push_str(" -> ");
    }
    out.push_str("Eff effs ");
    out.push_str(&result.render_app_arg());
    out
}

/// Parenthesize a rendered effect head exactly when it is an application
/// (`Finalize v`) rather than a single atom (`RunLLMTurn`) — correct in
/// `Member` position, the same rule [`HsType::render_app_arg`] applies to a
/// real type, spelled out for a plain string because [`Effect::head`]
/// renders one, not an [`HsType`].
///
/// [`Effect::head`]: crate::schema::Effect::head
#[must_use]
fn paren_if_applied(head: &str) -> String {
    if head.contains(' ') {
        format!("({head})")
    } else {
        head.to_string()
    }
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
            (
                HsType::app(
                    HsType::app(HsType::Named("Eff"), HsType::Var("effs")),
                    HsType::Unit,
                ),
                "Eff effs ()",
                "(Eff effs ())",
            ),
            (
                HsType::app(
                    HsType::func(HsType::Var("a"), HsType::Var("b")),
                    HsType::Var("c"),
                ),
                "(a -> b) c",
                "((a -> b) c)",
            ),
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
                HsType::list(HsType::Named("CommitDeltas")),
                "[CommitDeltas]",
                "[CommitDeltas]",
            ),
            (
                HsType::list(HsType::Named("FileMeta")),
                "[FileMeta]",
                "[FileMeta]",
            ),
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
                HsType::maybe(HsType::Named("CommitDeltas")),
                "Maybe CommitDeltas",
                "(Maybe CommitDeltas)",
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
