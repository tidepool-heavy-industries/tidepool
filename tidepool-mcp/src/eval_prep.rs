//! Pure, deterministic eval-preparation helpers, extracted from the
//! `tidepool-mcp` server shell (`lib.rs`) so they can be unit- and
//! property-tested directly instead of only through the async server.
//!
//! Everything here is a side-effect-free function of its inputs: effect-stack
//! and preamble string builders, the `code -> module` templating, the
//! quasi-quoter token probe, and error-payload rendering. They are
//! re-exported from `lib.rs` via `pub use eval_prep::*;`, so existing callers
//! (`tidepool_mcp::template_haskell`, `standard_decls`, ...) are unaffected.
//!
//! Two intentionally-excluded neighbours stay in `lib.rs`: `build_preamble`
//! (being edited in parallel) and `ensure_effects_module` (it writes a temp
//! dir — IO, not pure — and only wraps the pure `effects_module_source` here).
//!
//! Failure CLASSIFICATION (mapping an error to its class/phase) is NOT here — it
//! is the single classifier in `tidepool_runtime::failclass`, shared by both
//! servers. `format_error_with_source` only RENDERS an already-classified
//! `(class, phase)` into the error payload.

use crate::EffectDecl;
use tidepool_runtime::{FailureClass, Phase};

/// THE single ordered source of the base effect stack (Ask excluded here — it
/// is interposed separately, by the shared `tidepool_runtime::session::SessionEngine`
/// for the eval server and by `tidepool-repl`'s own dispatcher). Each row pairs
/// the Haskell effect type name with its [`EffectDecl`] builder, in the ONE
/// canonical order.
///
/// Both [`standard_decls`] (which derives the decl list, the `type M = Eff
/// '[…]` string, and the `:vocab`/discoverability surface) and
/// `tidepool_handlers::build_base_stack` (which builds the handler HList)
/// expand THIS macro. Because the effect-list order and the handler-dispatch
/// order come from the SAME sequence, they cannot desync — the freer-simple
/// union-tag ↔ handler correspondence (a Locked Decision: tags index the
/// effect list positionally) holds by construction, not by convention.
///
/// Invoke as `base_effects!(callback)`; the callback macro receives the rows
/// `(Name, decl_fn), …` and expands them into whatever it needs — a
/// `vec![…]` of decls, a `frunk::hlist![…]` of handlers, etc.
///
/// **Cutting / adding / reordering an effect is a single edit to THIS list.**
#[macro_export]
macro_rules! base_effects {
    ($callback:ident) => {
        $callback! {
            (Console, console_decl),
            (KV,      kv_decl),
            (Fs,      fs_decl),
            (Http,    http_decl),
            (Exec,    exec_decl),
            (Lsp,     lsp_decl),
            (Llm,     llm_decl),
            (Git,     git_decl),
            (Time,    time_decl),
        }
    };
}

/// All standard effects in canonical order (the base stack + the interposed
/// `Ask` effect appended last). Derived from the single-source [`base_effects!`]
/// list — do not hand-maintain a parallel order here.
pub fn standard_decls() -> Vec<EffectDecl> {
    macro_rules! std_decls_rows {
        ($(($name:ident, $decl:ident)),* $(,)?) => {
            vec![ $( $crate::$decl() ),*, $crate::ask_decl() ]
        };
    }
    crate::base_effects!(std_decls_rows)
}

/// Generate the Haskell module preamble that wraps user code in `eval` calls.
///
/// Emits: language pragmas, `module Expr`, standard imports (`Tidepool.Prelude`,
/// `Control.Monad.Freer`, qualified `Data.Text`/`Data.Map`/etc.), the user `Library`
/// import (if present), GADT declarations for each registered effect, the `type M`
/// alias over the full effect list, and thin helper functions (e.g. `say`, `kvGet`).
///
/// The Schema vocabulary + structured `ask`/`llm` come from the Ask/Llm effect
/// decls (`ask`/Schema always present; `llm` needs the Llm effect).
/// Source of the generated `Tidepool.Effects` module: effect type_defs,
/// GADTs, the `M` alias, the `error :: Text -> a` shadow, and the thin
/// send-wrapper helpers.
///
/// This exists as a REAL module (written to an include dir by
/// [`crate::ensure_effects_module`]) rather than text spliced into the eval
/// preamble, because GHC identifies types by `(module, name)`: with one
/// importable module, the eval module and `.tidepool/lib` user modules
/// share ONE set of effect types — so user libraries can define effectful
/// verbs (`census :: Text -> M Value`) that unify with eval code.
pub fn effects_module_source(effects: &[EffectDecl]) -> String {
    let mut out = String::new();
    out.push_str("{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, ScopedTypeVariables, ExtendedDefaultRules, LambdaCase, TupleSections, MultiWayIf, RecordWildCards, NamedFieldPuns, ViewPatterns, BangPatterns, TypeApplications, BlockArguments, NumericUnderscores, MultilineStrings, DeriveFunctor, DeriveFoldable, DeriveTraversable, DeriveGeneric, DeriveAnyClass, DuplicateRecordFields, OverloadedRecordDot #-}\n");
    out.push_str("-- GENERATED by the tidepool MCP server from its effect handler\n");
    out.push_str("-- declarations. Do not edit; regenerated (content-addressed) at startup.\n");
    // Orphan `MonadFail (Eff effs)` instance below (issue #331): both MonadFail
    // and Eff are defined elsewhere, so the instance is an orphan — silence the
    // warning. This is the home every eval + repl session imports.
    out.push_str("{-# OPTIONS_GHC -Wno-orphans #-}\n");
    out.push_str("module Tidepool.Effects where\n");
    out.push_str("import Tidepool.Prelude hiding (error)\n");
    out.push_str("import Control.Monad.Fail (MonadFail(..))\n");
    out.push_str("import qualified Tidepool.Data.Text as T\n");
    out.push_str("import qualified Data.Map.Strict as Map\n");
    out.push_str("import qualified Tidepool.Aeson.KeyMap as KM\n");
    // Pure Myers-diff core: the `planUpdate` editing helper renders its review
    // diff via `Patch.genPatch`/`Patch.renderPatch`.
    out.push_str("import qualified Tidepool.Patch as Patch\n");
    out.push_str("import Control.Monad.Freer hiding (run)\n");
    out.push_str("import qualified Prelude as P\n");
    out.push_str("default (Int, Double, Text)\n");
    out.push_str("error :: Text -> a\nerror = P.error . T.unpack\n");
    // MonadFail routed through our JIT-safe `error` (#331): a failed refutable
    // bind (`Just x <- e` when e is Nothing) aborts the eval with the desugared
    // message, exactly like `error`. `T.pack :: String -> Text` bridges GHC's
    // `String`-typed `fail` argument to our `Text`-typed `error`.
    out.push_str("instance MonadFail (Eff effs) where fail = error . T.pack\n");
    // The abort register for typed-failure verbs (#335): unwrap a Right or
    // abort rendering the Left (mtl's `liftEither` name; our error channel).
    out.push_str("liftEither :: P.Show e => Either e a -> Eff effs a\nliftEither = either (error . T.pack . P.show) pure\n");
    out.push('\n');

    for eff in effects {
        eff.type_defs.iter().for_each(|td| {
            out.push_str(td);
            out.push('\n');
        });
        out.push_str(&format!("data {} a where\n", eff.type_name));
        eff.constructors.iter().for_each(|ctor| {
            out.push_str(&format!("  {}\n", ctor));
        });
        out.push('\n');
    }

    // Type alias so helpers can write `M a` instead of `Eff '[Console, KV, Fs] a`
    if !effects.is_empty() {
        let names: Vec<&str> = effects.iter().map(|e| e.type_name).collect();
        out.push_str(&format!("type M = Eff '[{}]\n\n", names.join(", ")));
    }

    // Thin effect helpers (send-wrappers and recipes)
    for eff in effects {
        for h in eff.helpers {
            out.push_str(h);
            out.push('\n');
        }
    }
    out
}

/// Does eval source splice a tidepool quasi-quoter? Exact token match:
/// GHC's quote-open syntax is literally `[fmt|`/`[j|` — no whitespace is
/// permitted between bracket, quoter name, and bar — so substring search
/// cannot false-negative. A false positive (the token inside a string
/// literal) only costs the ~+385ms quoter-module import, never correctness.
pub fn uses_qq(src: &str) -> bool {
    src.contains("[fmt|") || src.contains("[j|") || src.contains("[patch|") || src.contains("[uri|")
}

pub fn build_effect_stack_type(effects: &[EffectDecl]) -> String {
    if effects.is_empty() {
        "'[]".to_string()
    } else {
        let names: Vec<&str> = effects.iter().map(|e| e.type_name).collect();
        format!("'[{}]", names.join(", "))
    }
}

/// Wrap a bare statement sequence as an explicit do-block. The
/// expression-first contract (template_haskell emits `code` as a real
/// top-level binding) means multi-statement payloads must be do-blocks;
/// this is the mechanical migration for test fixtures written in the
/// old lines-into-a-do dialect.
pub fn wrap_do(code: &str) -> String {
    format!(
        "do\n{}",
        code.lines()
            .map(|l| format!("  {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

pub fn template_haskell(
    preamble: &str,
    effect_stack: &str,
    code: &str,
    imports: &str,
    helpers: &str,
    input: Option<&serde_json::Value>,
    budget: Option<u32>,
) -> String {
    template_haskell_impl(
        preamble,
        effect_stack,
        code,
        imports,
        helpers,
        input,
        budget,
        false,
    )
}

/// Like [`template_haskell`] but uses `toWire` instead of `toJSON` for the
/// result — enabling Show-default rendering (REPL mode). `Text` renders bare
/// (not show-quoted); `Value` passes through as structured JSON; any other
/// `Show a` renders via `show`. The `ToWire` class is always emitted by
/// [`crate::build_preamble`] / [`crate::build_preamble_non_interactive`].
///
/// Used by `tidepool-repl` `session_eval` turns; the stateless eval server
/// uses [`template_haskell`] (preserves the toJSON contract for machine callers).
pub fn template_haskell_show_default(
    preamble: &str,
    effect_stack: &str,
    code: &str,
    imports: &str,
    helpers: &str,
    input: Option<&serde_json::Value>,
    budget: Option<u32>,
) -> String {
    template_haskell_impl(
        preamble,
        effect_stack,
        code,
        imports,
        helpers,
        input,
        budget,
        true,
    )
}

#[allow(clippy::too_many_arguments)] // shared impl behind template_haskell / _show_default
fn template_haskell_impl(
    preamble: &str,
    effect_stack: &str,
    code: &str,
    imports: &str,
    helpers: &str,
    input: Option<&serde_json::Value>,
    budget: Option<u32>,
    show_default: bool,
) -> String {
    let mut out = String::new();

    // Preamble contains: pragmas, module header, standard imports, default decl,
    // data declarations, type alias. User imports must go after standard imports
    // (after "import Control.Monad.Freer\n") and before "default".
    if !imports.is_empty() {
        let insert_point = preamble.find("default (Int").unwrap_or(preamble.len());
        out.push_str(&preamble[..insert_point]);
        for imp in imports.lines().map(str::trim).filter(|l| !l.is_empty()) {
            out.push_str(&format!("import {}\n", imp));
        }
        out.push_str(&preamble[insert_point..]);
    } else {
        out.push_str(preamble);
    }

    // Marker for user code section (used by error formatting to trim preamble)
    out.push_str("-- [user]\n");

    if !helpers.is_empty() {
        out.push_str(helpers);
        if !helpers.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }

    // Inject input binding if provided
    out.push_str(&input_binding_source(input));

    // User code is a real binding (single EXPRESSION; explicit `do` for
    // sequencing; trailing `where` legal) embedded VERBATIM — no indentation
    // transform. Explicit `let { }` brackets suspend the layout algorithm
    // (Report rule L, explicit context), so unindented user lines are legal
    // and quasiquote payloads keep byte-exact fidelity (indenting them was
    // the "+2 corrupts multi-line QQ" bug class). `__b` is local to this RHS.
    out.push_str("__user = let {\n __b =\n");
    // 1-based inclusive line range of the user's own `code` text within this
    // module: `start` is the line right after this bracket (where `code`'s
    // first line lands); `end` follows from `code`'s own newline count. Riding
    // this on the closing-bracket line (rather than a standalone comment line)
    // keeps every downstream line number byte-identical to before this range
    // was computed — no line is inserted, only appended text on an existing one.
    let start_line = out.matches('\n').count() + 1;
    out.push_str(code);
    if !code.ends_with('\n') {
        out.push('\n');
    }
    let content_lines = if code.is_empty() {
        1
    } else if code.ends_with('\n') {
        code.matches('\n').count()
    } else {
        code.matches('\n').count() + 1
    };
    let end_line = start_line + content_lines - 1;
    out.push_str(&format!(
        " }} in __b  -- [user-lines] {start_line}:{end_line}\n"
    ));
    out.push('\n');

    // render_call: toWire in REPL (Show-default), toJSON in stateless server.
    let render_call = if show_default { "toWire" } else { "toJSON" };

    out.push_str(&format!("result :: Eff {} Value\n", effect_stack));
    out.push_str("result = do\n");
    if budget.is_some() {
        out.push_str("  kvSet \"__sayChars\" (toJSON (0 :: Int))\n");
    }
    out.push_str("  _r <- __user\n");
    if let Some(b) = budget {
        out.push_str("  _scV <- kvGet \"__sayChars\"\n");
        out.push_str("  let _sayC = case _scV of { Just b -> case b ^? _Int of { Just n -> n; _ -> 0 }; Nothing -> 0 }\n");
        out.push_str(&format!(
            "  paginateResult (max 100 ({} - _sayC)) ({render_call} _r)\n",
            b
        ));
    } else {
        out.push_str(&format!("  paginateResult 4096 ({render_call} _r)\n"));
    }

    out
}

/// Escape a string for inclusion in a generated Haskell string literal.
/// Control characters matter: an unescaped newline in the payload is a
/// LEXICAL ERROR in the generated module (bit the eval `input` channel
/// for every multi-line payload).
fn escape_haskell_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:x};", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Render the `input :: Aeson.Value` top-level binding for injection into a
/// generated module, or the empty string when there is no payload. Shared by
/// the eval template here and the `tidepool-repl` session wraps so every code
/// path that can reference `input` injects it identically.
pub fn input_binding_source(input: Option<&serde_json::Value>) -> String {
    match input {
        Some(val) => format!("input :: Aeson.Value\ninput = {}\n\n", json_to_haskell(val)),
        None => String::new(),
    }
}

/// Render a serde_json::Value as a Haskell aeson literal expression.
fn json_to_haskell(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::Null => "Aeson.Null".into(),
        serde_json::Value::Bool(b) => {
            format!("Aeson.Bool {}", if *b { "True" } else { "False" })
        }
        serde_json::Value::Number(n) => {
            // Exact for ints AND floats: decompose the token into an integer
            // coefficient × 10^exponent and build the aeson Scientific directly.
            let (coeff, exp) = tidepool_eval::shapes::parse_decimal_token(&n.to_string());
            format!("Aeson.Number (Aeson.scientific ({coeff}) ({exp}))")
        }
        serde_json::Value::String(s) => {
            format!("Aeson.String \"{}\"", escape_haskell_string(s))
        }
        serde_json::Value::Array(arr) => {
            let elems: Vec<String> = arr.iter().map(json_to_haskell).collect();
            format!("toJSON [{}]", elems.join(", "))
        }
        serde_json::Value::Object(map) => {
            let pairs: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("\"{}\" .= {}", escape_haskell_string(k), json_to_haskell(v)))
                .collect();
            format!("object [{}]", pairs.join(", "))
        }
    }
}

pub(crate) fn format_error_with_source(
    class: FailureClass,
    phase: Phase,
    title: &str,
    error: &str,
    diagnostics: Option<&[tidepool_runtime::diag::ExtractDiag]>,
    source: &str,
) -> String {
    // Anchor on the verbatim-embedding bracket — the user's `code` starts on
    // the very next line (byte-exact; see the `__user = let {` emission), so
    // offsets/echo align with the snippet the caller wrote. The closing
    // bracket and the `result ::` wrapper below are dropped from the echo:
    // teaching callers the generated plumbing is the wrong dialect.
    const MARKER: &str = "__user = let {\n __b =\n";
    let marker_pos = source.find(MARKER);
    let user_section = marker_pos.map_or(source, |pos| &source[pos + MARKER.len()..]);
    let user_section = user_section
        .find("\n } in __b")
        .map_or(user_section, |pos| &user_section[..pos])
        .trim_end();
    // Remap `Expr.hs:<n>` line numbers so they count from the user's first code
    // line (= `__user =` line + 1) instead of the ~200-line generated preamble;
    // a reported line then lands 1-based on the echoed User Code below.
    let offset = marker_pos
        .map(|pos| source[..pos + MARKER.len()].matches('\n').count())
        .unwrap_or(0);
    // When the caller has structured diagnostics (a real GHC compile failure),
    // render them item-relative via span arithmetic. Otherwise (timeout/crash/
    // a non-Diagnostics CompileError path) there are no GHC coordinates to
    // remap — use `error` verbatim.
    let body = match diagnostics {
        Some(diags) => {
            let user_lines = tidepool_runtime::diag::extract_user_code_lines(source);
            tidepool_runtime::diag::render_diagnostics(
                diags,
                &tidepool_runtime::diag::RenderOpts {
                    anchor: "Expr.hs",
                    label: "<expr>",
                    user_lines,
                    line_offset: offset,
                    col_indent: 0,
                    drop_foreign_gen_warnings_except: None,
                    source,
                },
            )
        }
        None => error.to_string(),
    };
    let mut out = format!(
        "## {}\n**failure-class:** `{}`  **phase:** `{}`\n\n{}\n\n## User Code\n```haskell\n{}\n```",
        title,
        class.tag(),
        phase.tag(),
        body,
        user_section
    );
    // When GHC can't infer the concrete type of the eval result (e.g. `pure 42`
    // without a type sig), it reports an "Ambiguous type variable" error at the
    // `toJSON _r` call site. Surface a targeted hint so the caller knows what to fix.
    if ambiguous_tojson_result(error) {
        out.push_str(
            "\n\n**Hint:** the result type is ambiguous — GHC cannot choose a concrete type to serialize. \
Add a type annotation to your expression, e.g. `pure (x :: Int)` or `pure (x :: Text)`.",
        );
    }
    // A `not in scope` on a canonical Prelude/Data.List name that lives under a
    // qualifier carries its reach-path (see `tidepool://capabilities`).
    for hint in scope_reach_hints(error) {
        out.push_str("\n\n**Hint:** ");
        out.push_str(&hint);
    }
    out
}

/// For each `… not in scope: <name>` in a GHC error whose `<name>` is a
/// canonical Prelude/Data.List name reached through a qualifier, the reach-path
/// fact ([`crate::resources::exclusion_reason`]). Empty when nothing matches.
fn scope_reach_hints(error: &str) -> Vec<String> {
    const NEEDLE: &str = "not in scope:";
    let lower = error.to_ascii_lowercase();
    let mut seen = std::collections::HashSet::new();
    let mut hints = Vec::new();
    let mut from = 0;
    while let Some(rel) = lower[from..].find(NEEDLE) {
        let start = from + rel + NEEDLE.len();
        from = start;
        // The name is the next identifier token, in the error's original case.
        let name: String = error[start..]
            .trim_start()
            .trim_start_matches('`')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '\'')
            .collect();
        if name.is_empty() || !seen.insert(name.clone()) {
            continue;
        }
        if let Some(reason) = crate::resources::exclusion_reason(&name) {
            hints.push(format!("`{name}` lives under a qualifier — {reason}."));
        }
    }
    hints
}

/// Returns true when the GHC error is an ambiguous-type-variable failure
/// at the `toJSON _r` call that wraps the user's result.
fn ambiguous_tojson_result(error: &str) -> bool {
    let lower = error.to_lowercase();
    lower.contains("ambiguous type") && lower.contains("tojson")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EffectDecl;
    use proptest::prelude::*;

    // ---- Property tests for the extracted pure helpers ----
    //
    // These functions were previously only reachable through the async server
    // and exercised by integration tests. Pulled into a sibling module, they
    // can be hit directly: determinism and structural invariants. Failure
    // classification moved to `tidepool_runtime::failclass` (tested there).

    /// The four quasi-quoter open-tokens `uses_qq` must recognize.
    const QQ_TOKENS: &[&str] = &["[fmt|", "[j|", "[patch|", "[uri|"];

    proptest! {
        /// Any text containing a quoter open-token is detected as QQ, no matter
        /// what surrounds it.
        #[test]
        fn prop_uses_qq_detects_every_token(
            tok in 0usize..4,
            prefix in "[a-zA-Z0-9 ]{0,40}",
            suffix in "[a-zA-Z0-9 ]{0,40}",
        ) {
            let src = format!("{prefix}{}{suffix}", QQ_TOKENS[tok]);
            prop_assert!(uses_qq(&src), "token {:?} not detected in {:?}", QQ_TOKENS[tok], src);
        }

        /// Every quoter token opens with `[`, so text with no `[` can never be a
        /// quote-open and must classify as non-QQ.
        #[test]
        fn prop_uses_qq_rejects_tokenless(s in "[a-zA-Z0-9 ()<>=:|]{0,80}") {
            prop_assert!(!uses_qq(&s));
        }

        /// `template_haskell` is a pure function: identical inputs yield
        /// byte-identical output.
        #[test]
        fn prop_template_haskell_deterministic(code in "[a-zA-Z0-9 \n]{0,40}") {
            let pre = "module Expr where\ndefault (Int)\n";
            let a = template_haskell(pre, "'[Console]", &code, "", "", None, None);
            let b = template_haskell(pre, "'[Console]", &code, "", "", None, None);
            prop_assert_eq!(a, b);
        }

        /// The user code rides into the rendered module BYTE-VERBATIM inside
        /// the explicit let-bracket embedding (no indent transform).
        #[test]
        fn prop_template_haskell_embeds_user_code(code in "[a-zA-Z0-9 ]{1,40}") {
            let pre = "module Expr where\ndefault (Int)\n";
            let src = template_haskell(pre, "'[Console]", &code, "", "", None, None);
            let verbatim = format!("__user = let {{\n __b =\n{code}\n }} in __b");
            prop_assert!(src.contains(&verbatim));
        }

        /// `wrap_do` prefixes a `do`, indents every line two spaces, and keeps
        /// each original line intact.
        #[test]
        fn prop_wrap_do_indents_and_preserves(
            lines in proptest::collection::vec("[a-zA-Z0-9 ]{1,20}", 1..6),
        ) {
            let code = lines.join("\n");
            let wrapped = wrap_do(&code);
            prop_assert!(wrapped.starts_with("do\n"));
            for (orig, out) in code.lines().zip(wrapped.lines().skip(1)) {
                let expected = format!("  {orig}");
                prop_assert_eq!(out, expected.as_str());
            }
        }

        /// `build_effect_stack_type` names every decl, in a promoted list, and
        /// is itself pure.
        #[test]
        fn prop_stack_type_lists_every_decl(n in 0usize..=8) {
            let all = standard_decls();
            let decls = &all[..n];
            let stack = build_effect_stack_type(decls);
            prop_assert!(stack.starts_with("'["));
            prop_assert!(stack.ends_with(']'));
            for d in decls {
                prop_assert!(stack.contains(d.type_name), "{} missing from {}", d.type_name, stack);
            }
            prop_assert_eq!(&stack, &build_effect_stack_type(decls));
        }
    }

    #[test]
    fn build_effect_stack_type_empty_is_nil() {
        assert_eq!(build_effect_stack_type(&[]), "'[]");
    }

    #[test]
    fn standard_decls_is_stable() {
        let a: Vec<&str> = standard_decls().iter().map(|d| d.type_name).collect();
        let b: Vec<&str> = standard_decls().iter().map(|d| d.type_name).collect();
        assert_eq!(a, b);
        // Canonical order is load-bearing: handlers are tag-indexed by it.
        assert_eq!(a.first(), Some(&"Console"));
        assert_eq!(a.last(), Some(&"Ask"));
        assert_eq!(a.len(), 10);
        // SG was cut (friction #37); the stack must NOT contain it.
        assert!(
            !a.contains(&"SG"),
            "SG should have been removed from the stack"
        );
    }

    #[test]
    fn test_uses_qq_detection() {
        // quoter tokens
        assert!(uses_qq("pure [fmt|hi {x}|]"));
        assert!(uses_qq("case v of [j|{\"k\": $x}|] -> pure x"));
        // wave-4 quoters: patch + the validators (glob omitted — see Validate.hs)
        assert!(uses_qq("apply [patch|--- a/x|]"));
        assert!(uses_qq("pure [uri|https://x|]"));
        // dropped quoters are NOT special: glob (removed) and sg (cut with the
        // SG effect) must both classify as non-QQ.
        assert!(!uses_qq("pure [glob|src/*.rs|]"));
        assert!(!uses_qq("pure [sg|fn $NAME|]"));
        // list comprehensions with conventional spacing are NOT tokens
        assert!(!uses_qq("pure [x | x <- xs]"));
        assert!(!uses_qq("pure [ fmt | fmt <- fs ]"));
        assert!(!uses_qq("plain code"));
        // `[j|j<-js]` IS the quote-open token — same ambiguity GHC has
        // once QuasiQuotes is on; detection mirrors the parser exactly
        // (documented dialect tradeoff: space before `|` in comprehensions)
        assert!(uses_qq("[j|j<-js]"));
    }

    #[test]
    fn test_build_effect_stack_type() {
        let effects = vec![
            EffectDecl {
                type_name: "Console",
                description: "",
                constructors: &[],
                type_defs: &[],
                helpers: &[],
            },
            EffectDecl {
                type_name: "KV",
                description: "",
                constructors: &[],
                type_defs: &[],
                helpers: &[],
            },
            EffectDecl {
                type_name: "Fs",
                description: "",
                constructors: &[],
                type_defs: &[],
                helpers: &[],
            },
        ];
        assert_eq!(build_effect_stack_type(&effects), "'[Console, KV, Fs]");
        assert_eq!(build_effect_stack_type(&[]), "'[]");
    }

    #[test]
    fn test_json_to_haskell_escapes_control_chars() {
        let v = serde_json::json!({"multi\nline\tkey": "line1\nline2\twith\rcontrols"});
        let rendered = json_to_haskell(&v);
        assert!(!rendered.contains('\n'), "raw newline leaked: {rendered}");
        assert!(rendered.contains("\\n"), "{rendered}");
        assert!(rendered.contains("\\t"), "{rendered}");
    }

    #[test]
    fn test_json_to_haskell() {
        let val = serde_json::json!({
            "str": "hello",
            "bool": true,
            "null": null,
            "num": 42,
            "arr": [1, 2],
            "obj": {"a": 1}
        });
        let haskell = json_to_haskell(&val);
        assert!(haskell.contains("\"str\" .= Aeson.String \"hello\""));
        assert!(haskell.contains("\"bool\" .= Aeson.Bool True"));
        assert!(haskell.contains("\"null\" .= Aeson.Null"));
        assert!(haskell.contains("\"num\" .= Aeson.Number (Aeson.scientific (42) (0))"));
        assert!(haskell.contains(
            "\"arr\" .= toJSON [Aeson.Number (Aeson.scientific (1) (0)), Aeson.Number (Aeson.scientific (2) (0))]"
        ));
        assert!(haskell
            .contains("\"obj\" .= object [\"a\" .= Aeson.Number (Aeson.scientific (1) (0))]"));
    }

    #[test]
    fn test_format_error_with_source() {
        let title = "Error";
        let error = "Type mismatch";
        let source = "preamble stuff\n-- [user]\nhelper :: Int\nhelper = 7\n\n__user = let {\n __b =\npure helper\n } in __b\n\nresult :: Eff '[] Value\nresult = do\n  _r <- __user\n  paginateResult 4096 (toJSON _r)\n";
        let formatted = format_error_with_source(
            FailureClass::UserHaskell,
            Phase::Compile,
            title,
            error,
            None,
            source,
        );

        assert!(formatted.contains("## Error"));
        assert!(formatted.contains("**failure-class:** `user-haskell`  **phase:** `compile`"));
        assert!(formatted.contains("Type mismatch"));
        assert!(formatted.contains("## User Code"));
        // The user's code (after `__user =`) is echoed, so reported line numbers
        // count 1-based from the snippet…
        assert!(formatted.contains("pure helper"));
        // …and everything before it (preamble, helper scaffold, the `__user =`
        // wrapper) plus the budget plumbing below is trimmed.
        assert!(!formatted.contains("preamble stuff"));
        assert!(!formatted.contains("__user ="));
        assert!(!formatted.contains("result ="));
        assert!(!formatted.contains("paginateResult"));
    }

    #[test]
    fn test_format_error_no_marker_shows_full() {
        let formatted = format_error_with_source(
            FailureClass::UserHaskell,
            Phase::Compile,
            "Error",
            "oops",
            None,
            "full source",
        );
        assert!(formatted.contains("full source"));
    }

    #[test]
    fn test_format_error_with_source_multiline() {
        let title = "Compile Error";
        let error = "Variable not in scope: x";
        let source = "module Test where\n-- [user]\n__user = let {\n __b =\ngo x y z\n  where go a b c = print [a,b,c]\n } in __b\n\nresult :: Eff '[] Value\nresult = do\n  _r <- __user\n";
        let formatted = format_error_with_source(
            FailureClass::UserHaskell,
            Phase::Compile,
            title,
            error,
            None,
            source,
        );

        assert!(formatted.contains("## Compile Error"));
        assert!(formatted.contains("Variable not in scope: x"));
        assert!(formatted.contains("## User Code"));
        // Multi-line user code (after `__user =`) echoes; scaffold is trimmed.
        assert!(formatted.contains("go x y z"));
        assert!(formatted.contains("where go a b c = print [a,b,c]"));
        assert!(!formatted.contains("module Test where"));
        assert!(!formatted.contains("result ="));
    }

    #[test]
    fn test_format_error_empty_source() {
        let formatted = format_error_with_source(
            FailureClass::UserHaskell,
            Phase::Compile,
            "Error",
            "msg",
            None,
            "",
        );
        assert!(formatted.contains("## Error"));
        assert!(formatted.contains("msg"));
        assert!(formatted.contains("## User Code"));
    }

    #[test]
    fn test_format_error_ambiguous_type_hint() {
        // GHC reports this when the user's result has a polymorphic type that
        // `toJSON _r` can't resolve (e.g. `pure 42` with no annotation).
        let ghc_error = "Expr.hs:3:3: error:\n    \
            • Ambiguous type variable 'a0' arising from a use of 'toJSON'\n    \
              prevents the constraint '(ToJSON a0)' from being solved.\n";
        let formatted = format_error_with_source(
            FailureClass::UserHaskell,
            Phase::Compile,
            "Compile Error",
            ghc_error,
            None,
            "",
        );
        assert!(
            formatted.contains("Hint:"),
            "expected ambiguous-type hint in output"
        );
        assert!(formatted.contains("type annotation"));
    }

    #[test]
    fn test_format_error_no_hint_for_unrelated_ambiguous() {
        // An ambiguous type error NOT involving toJSON should not append the hint.
        let ghc_error = "Expr.hs:3:3: error:\n    \
            • Ambiguous type variable 'a0' arising from a use of 'show'\n";
        let formatted = format_error_with_source(
            FailureClass::UserHaskell,
            Phase::Compile,
            "Compile Error",
            ghc_error,
            None,
            "",
        );
        assert!(
            !formatted.contains("Hint:"),
            "should not hint for non-toJSON ambiguous type"
        );
    }

    #[test]
    fn orchestrate_emits_towire_class() {
        // ToWire moved into the generated Tidepool.Orchestrate module (always
        // emitted, no effect dependency); the expr-module preamble imports it.
        let orch = crate::orchestrate_module_source(&[]);
        assert!(
            orch.contains("class ToWire a"),
            "ToWire class missing from orchestrate module"
        );
        assert!(
            orch.contains("instance {-# OVERLAPPABLE #-} Show a => ToWire a"),
            "Show fallback instance missing from orchestrate module"
        );
        assert!(
            orch.contains("instance ToWire Text"),
            "Text instance missing from orchestrate module"
        );
        assert!(
            orch.contains("instance ToWire Value"),
            "Value instance missing from orchestrate module"
        );
        // Container instances (KEEP the Show floor — leaves stay Show-strings,
        // only containers gain structure).
        assert!(
            orch.contains("instance {-# OVERLAPPING #-} ToWire [Char]"),
            "[Char] overlap instance missing from orchestrate module"
        );
        assert!(
            orch.contains("instance ToWire a => ToWire [a]"),
            "[a] list instance missing from orchestrate module"
        );
        assert!(
            orch.contains("instance ToWire a => ToWire (Maybe a)"),
            "Maybe instance missing from orchestrate module"
        );
        assert!(
            orch.contains("instance (ToWire a, ToWire b) => ToWire (a, b)"),
            "tuple instance missing from orchestrate module"
        );
    }

    #[test]
    fn template_haskell_show_default_uses_towire() {
        let pre = crate::build_preamble(&[], false);
        let src =
            template_haskell_show_default(&pre, "'[]", "pure (42 :: Int)", "", "", None, None);
        assert!(src.contains("toWire _r"), "show_default must use toWire _r");
        assert!(
            !src.contains("toJSON _r"),
            "show_default must not use toJSON _r"
        );
    }

    #[test]
    fn template_haskell_default_uses_tojson() {
        let pre = crate::build_preamble(&[], false);
        let src = template_haskell(&pre, "'[]", "pure (42 :: Int)", "", "", None, None);
        assert!(src.contains("toJSON _r"), "default must use toJSON _r");
        assert!(!src.contains("toWire _r"), "default must not use toWire _r");
    }

    /// GHC not available outside `nix develop` — the same skip-gate every
    /// GHC-heavy test in this workspace uses (see `tidepool-runtime/src/lib.rs`).
    fn ghc_available() -> bool {
        std::process::Command::new("ghc")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// A REAL compile error, through the REAL `template_haskell` wrapping (the
    /// preamble + effects module + a multi-line `helpers`/`code` payload), must
    /// have its `Expr.hs:<n>` line rebased onto the caller's own source — this
    /// is the eval/repl feedback-loop guarantee (`format_error_with_source` +
    /// `tidepool_runtime::diag::render_diagnostics`), not just a property of
    /// the synthetic strings the other tests in this module use.
    #[test]
    fn format_error_with_source_rebases_real_compile_error_line() {
        if !ghc_available() {
            eprintln!("Skipping: GHC not available (run inside `nix develop`)");
            return;
        }
        let decls = crate::standard_decls();
        let preamble = crate::build_preamble(&decls, false);
        let stack = crate::build_effect_stack_type(&decls);
        // A 3-line user expression: the type error ( `ok + True`, no `Num
        // Bool` instance) sits on line 2 of the SNIPPET the caller wrote.
        let code = "let ok = (1 :: Int)\n    bad = ok + True\n in pure bad";
        let source = template_haskell(&preamble, &stack, code, "", "", None, None);

        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let prelude_dir = manifest_dir.parent().unwrap().join("haskell/lib");
        let user_lib_dir = manifest_dir.parent().unwrap().join(".tidepool/lib");
        assert!(
            user_lib_dir.join("Library.hs").exists(),
            ".tidepool/lib/Library.hs not found"
        );
        let effects_dir = crate::ensure_effects_module(&decls).expect("write effects module");
        let include = [
            prelude_dir.as_path(),
            user_lib_dir.as_path(),
            effects_dir.as_path(),
        ];

        let err = tidepool_runtime::compile_haskell(&source, "result", &include)
            .expect_err("expected a type error for `ok + True`");
        let diags = match err {
            tidepool_runtime::CompileError::Diagnostics(diags) => diags,
            other => panic!("expected Diagnostics, got: {other:?}"),
        };
        // Sanity check the fixture actually reproduces the intended failure
        // (guards against a future GHC/Prelude change silently no-oping it).
        assert!(
            diags
                .iter()
                .any(|d| d.span.as_ref().is_some_and(|s| s.file.ends_with("Expr.hs"))),
            "expected a located GHC diagnostic, got: {diags:?}"
        );
        let raw_line = diags
            .iter()
            .find_map(|d| d.span.as_ref())
            .map(|s| s.start_line)
            .expect("a diagnostic span");
        // The snippet is only 3 lines; the wrapper preamble is far larger, so
        // the raw line number is necessarily > 2.
        assert!(
            raw_line > 2,
            "expected a wrapper-shifted raw line, got {raw_line}"
        );

        let error_text: String = diags
            .iter()
            .map(|d| d.message.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let formatted = format_error_with_source(
            FailureClass::UserHaskell,
            Phase::Compile,
            "Compile Error",
            &error_text,
            Some(&diags),
            &source,
        );

        // Rebased: line 2 of the snippet (`bad = ok + True`), NOT the raw
        // ~150+ line offset into the generated wrapper module.
        assert!(
            formatted.contains("<expr>:2:"),
            "expected the error rebased to <expr>:2 (the snippet's own line \
             2), got:\n{formatted}"
        );
        assert!(
            !formatted.contains(&format!("Expr.hs:{raw_line}:")),
            "the wrapper's raw (un-rebased) line number leaked into the \
             formatted output:\n{formatted}"
        );
        assert!(formatted.contains("bad = ok + True"));
    }
}
