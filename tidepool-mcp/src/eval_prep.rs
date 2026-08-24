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
//! Two intentionally-excluded neighbours stay elsewhere: `build_preamble`
//! (`preamble.rs`) and `ensure_effects_core_module`/`ensure_effects_shim_module`
//! (`lib.rs`; they write to content-addressed dirs — IO, not pure — and only
//! wrap the pure `effects_core_module_source`/`effects_shim_module_source` here).
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
            (Llm,     llm_decl),
            (Git,     git_decl),
            (Time,    time_decl),
        }
    };
}

/// The ordinary one-shot MCP eval server's and the REPL's effect roster: the
/// base stack plus the interposed `Ask`/`RunLLMTurn` effects appended last,
/// in that order. Derived from the single-source [`base_effects!`] list — do
/// not hand-maintain a parallel order here.
///
/// **This is a per-surface roster, not a universal one** (vestigial-subsystems
/// review §4): it names exactly what the ordinary session engine's request
/// parser (`tidepool_runtime::session::engine::extract_ask_request`, shared
/// verbatim by the REPL) actually accepts — `AskWith` and `RunLLMTurnWith`,
/// nothing else. `EffectRoster::from_handlers` (`tidepool-mcp/src/server.rs`)
/// is the single production consumer for both surfaces, and appends this same
/// two-effect suffix independently (it builds from an arbitrary handler
/// stack, not from this decl-list function) — the two cannot desync because
/// both append the identical `[Ask, RunLLMTurn]` tail derived from the same
/// `ask_decl`/`runllmturn_decl` builders.
///
/// A surface that genuinely services more than this — the harness Agent turn
/// (`tidepool-harness::engine::agent_decls`) dispatches `ForkWith`/
/// `ForkAllWith` through its own `classify_hole`, which this engine's parser
/// does not — builds its own WIDER roster by appending to this one's output
/// explicitly (`agent_decls`'s doc has the details), rather than this
/// function growing a fourth interposed effect nothing here can service.
///
/// `RunLLMTurn` must stay in the roster (not opt-in): the generated
/// `Tidepool.Effects` only exports `runLLMTurn`/`runLLMTurnFork`/
/// `runLLMTurnFanout` when its decl is present. Both `Ask` and `RunLLMTurn`
/// are UNHANDLED (interposed) tags: no `tidepool-handlers` entry, serviced by
/// each server's own suspend machinery (see
/// `tidepool-codegen::jit_machine::drive_effect_loop`'s `suspend_tag`
/// threshold — every tag from the first interposed effect onward suspends,
/// so appending further interposed effects here needs no Rust-side dispatch
/// change).
pub fn standard_decls() -> Vec<EffectDecl> {
    macro_rules! std_decls_rows {
        ($(($name:ident, $decl:ident)),* $(,)?) => {
            vec![ $( $crate::$decl() ),*, $crate::ask_decl(), $crate::runllmturn_decl() ]
        };
    }
    crate::base_effects!(std_decls_rows)
}

/// Effects whose GADT genuinely cannot live in the stable Core module,
/// because at least one CONSTRUCTOR's field type mentions `M` directly —
/// unlike an ordinary row-closed HELPER (which [`EffectDecl::helpers_row_polymorphic`]
/// already tracks per-effect, and which every effect definition in this
/// codebase sets `true`), a GADT constructor's field type cannot be made
/// `Member`-polymorphic by rewording its signature: the wire contract needs
/// the closure typed at exactly the compile's own concrete row, not a choice
/// of phrasing. `Green`'s `AsyncSpawnWith :: Int -> (Int -> M ()) -> Green
/// Int` is the one known case (see its definition site,
/// `green_effect_def!` in `effect_defs.rs`, for the full "why" — the runtime
/// dispatches the thread body directly against the row it was spawned
/// under). Named explicitly rather than inferred (e.g. by scanning
/// constructor text for `M`) because inference here would be guessing at
/// author intent from a string; a real GADT declaration whose module this
/// changes is a rare, deliberate edit, not something that should silently
/// change behavior based on incidental text content.
///
/// A row-dependent effect is excluded from Core ([`effects_core_module_source`])
/// and instead declared directly in the per-window shim
/// ([`effects_shim_module_source`]) whenever it's in the ROW — see
/// [`crate::ensure_effects_module_with_vocab`]'s coordinating check for what
/// happens if one is vocab-only instead (a loud panic, not a silent drop).
pub const ROW_DEPENDENT_EFFECTS: &[&str] = &["Green"];

#[must_use]
pub(crate) fn is_row_dependent_effect(type_name: &str) -> bool {
    ROW_DEPENDENT_EFFECTS.contains(&type_name)
}

/// The head name of a `type_defs` entry (`"data AsyncStatus = ..."` ->
/// `Some(("AsyncStatus", true))`, `"type Foo = ..."` -> `Some(("Foo",
/// false))`), plus whether it takes the `Name(..)` export form (a
/// `data`/`newtype` — has constructors to export alongside the type) or the
/// bare `Name` form (a `type` synonym — no constructors of its own). Used
/// ONLY by [`effects_shim_module_source`] to export a ROW-DEPENDENT effect's
/// supporting types, which — unlike Core's implicit "export everything
/// declared here" — need to be named explicitly in the shim's export list.
/// `None` for a `type_defs` entry that isn't a `data`/`newtype`/`type` head
/// at all (e.g. a standalone `instance ...` — nothing to export).
fn type_def_head_name(td: &str) -> Option<(&str, bool)> {
    let (has_ctors, rest) = if let Some(r) = td.strip_prefix("data ") {
        (true, r)
    } else if let Some(r) = td.strip_prefix("newtype ") {
        (true, r)
    } else if let Some(r) = td.strip_prefix("type ") {
        (false, r)
    } else {
        return None;
    };
    let name = rest
        .split(|c: char| c.is_whitespace() || c == '=')
        .next()?
        .trim();
    if name.is_empty() {
        None
    } else {
        Some((name, has_ctors))
    }
}

/// Source of the STABLE `Tidepool.Effects.Core` module: every vocabulary
/// effect's `type_defs`, GADT, and helpers — a pure function of the effect
/// VOCABULARY ALONE (no row, no [`crate::RowArgs`]). This is the
/// session-durable half of the generated effects surface (see
/// `tidepool-mcp/CLAUDE.md`'s stable-effects-core section): its text depends
/// only on WHICH EFFECTS EXIST for a given compile family (the general Agent
/// stack, the self-iterating harness's answerer stack, …), never on which
/// hole type a particular window's `Finalize` is pinned to — so two windows
/// of the same session, or two turns of the same node, that share a
/// vocabulary get byte-identical Core text, hence the same content-addressed
/// dir (see [`crate::ensure_effects_core_module`]) and no tycon churn.
///
/// **An effect named in [`ROW_DEPENDENT_EFFECTS`] is excluded from Core
/// entirely** (its `type_defs`, GADT, AND helpers) — see that constant's doc
/// for exactly why (in one word: `Green`'s `AsyncSpawnWith` GADT constructor
/// has a FIELD TYPE that itself mentions `M`, which cannot typecheck in a
/// module with no `M` of its own). [`effects_shim_module_source`] picks up
/// the exact same exclusion for every ROW effect (never vocab-only; see
/// [`crate::ensure_effects_module_with_vocab`]'s coordinating check), so
/// nothing is silently dropped — it just declares per-window instead of
/// once-per-session, exactly as the whole surface did before this split.
///
/// **Emitted per vocabulary effect, deduplicated by `type_name`** (first
/// occurrence in `vocab_effects` order wins) so the same effect can never
/// double-declare its GADT — in a DETERMINISTIC order (`vocab_effects`'
/// own order, not a sort), because this text is a compile-cache key (see
/// [`crate::ensure_effects_core_module`]'s content-addressing): nondeterministic
/// ordering would silently destroy cache hits.
///
/// A parameterized effect (`Finalize`, non-empty `type_params`) is emitted
/// GENERICALLY (`data Finalize v a where …`, `v` a bare type variable) exactly
/// like every other effect — there is no "vocab-only parameterized effects are
/// unrepresentable" restriction any more: a generic GADT plus a
/// `Member`-polymorphic `finalize` needs no row application to have a
/// well-defined spelling, and it is exactly as nameable-but-possibly-
/// unexecutable as any other vocabulary effect (a comprehensible unsolved-
/// `Member` error at the use site, same as `RunLLMTurn`'s existing
/// nameable-everywhere policy — this replaces and generalizes it).
pub fn effects_core_module_source(vocab_effects: &[EffectDecl]) -> String {
    let mut seen = std::collections::HashSet::new();
    let effects: Vec<&EffectDecl> = vocab_effects
        .iter()
        .filter(|v| seen.insert(v.type_name))
        .filter(|v| !is_row_dependent_effect(v.type_name))
        .collect();

    let mut out = String::new();
    // Single source of truth: crate::preamble::EVAL_PRAGMAS (one dialect
    // everywhere). This used to be a hand-duplicated copy of that same list —
    // it had drifted (missing UndecidableInstances, PartialTypeSignatures,
    // QuasiQuotes), which is exactly the divergence a second copy invites.
    out.push_str(crate::preamble::EVAL_PRAGMAS);
    out.push('\n');
    out.push_str("-- GENERATED by the tidepool MCP server from its effect handler\n");
    out.push_str("-- declarations. Do not edit; regenerated (content-addressed) at startup.\n");
    out.push_str("-- STABLE module: a pure function of the effect VOCABULARY alone (no row,\n");
    out.push_str("-- no `type M`) — see tidepool-mcp/CLAUDE.md's stable-effects-core section.\n");
    // Orphan `MonadFail (Eff effs)` instance below (issue #331): both MonadFail
    // and Eff are defined elsewhere, so the instance is an orphan — silence the
    // warning. This is the home every eval + repl session imports (via the
    // per-window shim's re-export).
    out.push_str("{-# OPTIONS_GHC -Wno-orphans #-}\n");
    out.push_str("module Tidepool.Effects.Core where\n");
    out.push_str("import Tidepool.Prelude hiding (error)\n");
    out.push_str("import Control.Monad.Fail (MonadFail(..))\n");
    // Canonical `Data.Void.Void` — the uninhabited default `Finalize` answer
    // type (`finalize_effect_def!`'s `default_row_args ["Void"]`) for a turn
    // that isn't answering a typed hole. Imported unconditionally (like the
    // rest of this preamble) rather than gated on `Finalize`'s presence in
    // `effects`: it's a zero-cost base import, and gating it would need a
    // vocabulary check this function doesn't otherwise make per-effect.
    out.push_str("import Data.Void (Void)\n");
    out.push_str("import qualified Tidepool.Data.Text as T\n");
    out.push_str("import qualified Data.Map.Strict as Map\n");
    out.push_str("import qualified Tidepool.Aeson.KeyMap as KM\n");
    // Pure Myers-diff core: the `planUpdate` editing helper renders its review
    // diff via `Patch.genPatch`/`Patch.renderPatch`.
    out.push_str("import qualified Tidepool.Patch as Patch\n");
    out.push_str("import Control.Monad.Freer hiding (run)\n");
    // `Eff`'s own constructors, for scopes that INTERPOSE on the computation
    // they enclose rather than merely sending into it. PRD 19's `withHandler`
    // is the live consumer: it walks its body's freer structure to run a
    // handler before every effect the body performs, which is what lets the
    // author's closure be applied by ordinary Haskell application instead of
    // by a runtime closure-apply entry point that does not exist. `Eff` is the
    // SAME type re-exported by `Control.Monad.Freer`, so this import adds
    // constructors and the queue operations, and shadows nothing.
    // See plans/post-restart/worktree-lanes/L4-mechanism.md.
    out.push_str("import Control.Monad.Freer.Internal (Eff(..), qApp, tsingleton)\n");
    // runLLMTurn/runLLMTurnFork's hidden *Sited siblings (#R0) coerce the
    // ask reply back to the caller's answer type after extract has statically
    // checked it's monomorphic; a pure function type is allowed (an
    // effect-monad-mentioning type is rejected) — see ask_effect_def!'s
    // helper text (effect_defs.rs) for why that's safe.
    out.push_str("import Unsafe.Coerce (unsafeCoerce)\n");
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

    for eff in &effects {
        eff.type_defs.iter().for_each(|td| {
            out.push_str(td);
            out.push('\n');
        });
        out.push_str(&format!(
            "data {}{} a where\n",
            eff.type_name,
            eff.type_params
                .iter()
                .map(|p| format!(" {p}"))
                .collect::<String>()
        ));
        eff.constructors.iter().for_each(|ctor| {
            out.push_str(&format!("  {}\n", ctor));
        });
        out.push('\n');
    }

    // Every vocabulary effect's helpers, unconditionally: the assertion above
    // already guarantees every one of them is row-polymorphic, so there is no
    // row-vs-vocab gate left to apply here (the old `emits_helpers_for` is
    // retired — it existed only to keep a row-CLOSED helper from being emitted
    // outside its row, a hazard that cannot arise once every helper is
    // `Member`-polymorphic by construction).
    for eff in &effects {
        for h in eff.helpers {
            out.push_str(h);
            out.push('\n');
        }
    }
    out
}

/// Source of the per-window `Tidepool.Effects` SHIM module: a re-export of
/// the stable [`effects_core_module_source`] plus the one thing that
/// genuinely varies per compile — `type M = Eff <row_effects>` — and the
/// author-module imports a parameterized row entry's applied type needs
/// (`row.imports()`, e.g. the harness's `HarnessTypes` for a `Finalize
/// Decision` row).
///
/// Model-visible spelling is UNCHANGED: `import Tidepool.Effects` and `M`
/// still resolve exactly as before — this module re-exports every name Core
/// declares, so an eval/session import of `Tidepool.Effects` alone still sees
/// the whole effect vocabulary plus `M`. Only the identity of WHERE each name
/// is nominally declared moved (to Core), which is invisible to model-facing
/// code and is exactly what makes it safe for a declaration mentioning those
/// names to persist across a session-bind (see
/// `haskell/src/Tidepool/Translate.hs`'s narrowed `typeMentionsEffectMonad`).
///
/// Content is a pure function of `row_effects` + `row` alone — small, and it
/// changes every time a window's `Finalize` (or another parameterized
/// effect's) row application changes, which is exactly why it stays split
/// out of Core: recompiling a few lines per window is cheap, recompiling
/// every effect GADT + helper per window (today's behavior) is not, and only
/// the small shim's tycons (none — see below) are ever per-window-nominal.
///
/// Declares no `data`/GADT of its own: `type M` is a type SYNONYM (GHC looks
/// through it before `typeMentionsEffectMonad` ever inspects a tycon), so
/// nothing here has its own nominal identity for that guard to catch — the
/// module exists to be tiny and cheap, not to hide anything.
pub fn effects_shim_module_source(row_effects: &[EffectDecl], row: &crate::RowArgs) -> String {
    // Row-dependent effects ([`ROW_DEPENDENT_EFFECTS`]) declared directly in
    // THIS module (see below) must ALSO be named in its export list — unlike
    // `module Tidepool.Effects.Core`, which re-exports Core's whole surface
    // via the module-wildcard form, a name this module DEFINES itself is
    // only exported if explicitly listed. Computed up front so the module
    // header (which needs the full export list before anything else is
    // emitted) can include them.
    let mut seen_for_exports = std::collections::HashSet::new();
    let row_dependent: Vec<&EffectDecl> = row_effects
        .iter()
        .filter(|e| seen_for_exports.insert(e.type_name))
        .filter(|e| is_row_dependent_effect(e.type_name))
        .collect();
    let mut extra_exports = String::new();
    for eff in &row_dependent {
        // Supporting `type_defs` (e.g. Green's `data AsyncStatus = ...`) need
        // their own export entry too — unlike the GADT/helpers below, these
        // are plain `data`/`newtype`/`type` decls this function doesn't
        // otherwise name anywhere, so the export list is the only place to
        // extract each one's head name.
        for td in eff.type_defs {
            if let Some((name, has_ctors)) = type_def_head_name(td) {
                if has_ctors {
                    extra_exports.push_str(&format!(", {name}(..)"));
                } else {
                    extra_exports.push_str(&format!(", {name}"));
                }
            }
        }
        extra_exports.push_str(&format!(", {}(..)", eff.type_name));
        for h in eff.helpers {
            if let Some(name) = crate::describe::helper_name(h) {
                extra_exports.push_str(&format!(", {name}"));
            }
        }
    }
    // `Void` needs the SAME explicit export-list treatment as a row-dependent
    // effect's own `type_defs` above, for a DIFFERENT reason: it isn't
    // declared by this module or by Core (`Data.Void`'s own base import), so
    // neither the `module Tidepool.Effects.Core` wildcard entry nor an
    // implicit no-export-list rule ever re-exports it. Without this, a TURN
    // module's own `import Tidepool.Effects` (unqualified, no import list)
    // cannot see `Void` even though the shim's `type M` line resolves it fine
    // internally (see the import above) — and the turn module needs it too:
    // `result :: Eff <promoted-row> Value`, spliced directly by the turn
    // TEMPLATE (not `type M`), names `Finalize Void` verbatim whenever this
    // row's `Finalize` entry falls back to its default. Gated on the same
    // `Finalize`-presence check as the import above.
    if row_effects.iter().any(|e| e.type_name == "Finalize") {
        extra_exports.push_str(", Void");
    }

    let mut out = String::new();
    out.push_str(crate::preamble::EVAL_PRAGMAS);
    out.push('\n');
    out.push_str("-- GENERATED by the tidepool MCP server from its effect handler\n");
    out.push_str("-- declarations. Do not edit; regenerated (content-addressed) at startup.\n");
    out.push_str("-- PER-WINDOW shim: re-exports the stable Tidepool.Effects.Core and adds\n");
    out.push_str("-- only `type M`, this compile's own effect row — see that module and\n");
    out.push_str("-- tidepool-mcp/CLAUDE.md's stable-effects-core section.\n");
    // `M` is only exportable when it is actually declared below (a non-empty
    // row) — an empty-row compile (no effects at all) never had `type M`
    // either, before this split.
    if row_effects.is_empty() {
        out.push_str("module Tidepool.Effects (module Tidepool.Effects.Core) where\n");
    } else {
        out.push_str(&format!(
            "module Tidepool.Effects (module Tidepool.Effects.Core, M{extra_exports}) where\n"
        ));
    }
    out.push_str("import Tidepool.Effects.Core\n");
    // `Eff`/`Member`/`send` for `type M`'s own RHS below and for any
    // ROW-DEPENDENT effect's GADT/helpers spliced in below (e.g. `Green`'s
    // `Member Green effs`), and `pure` for `__shimProbe`'s body: Core IMPORTS
    // all of these (from freer-simple and `Tidepool.Prelude` respectively)
    // but, having no explicit export list re-exporting them, does not itself
    // re-export them — a module's implicit export list covers only what it
    // DEFINES, never what it merely imports. Needed here regardless of
    // Core's own export shape, so this is not a fragile assumption about
    // Core's internals.
    if !row_effects.is_empty() {
        out.push_str("import Control.Monad.Freer hiding (run)\n");
        out.push_str("import Tidepool.Prelude hiding (error)\n");
    }
    // `Data.Void`'s `Void` rides the SAME re-export hazard as the pair above,
    // but only when `Finalize` is actually in the vocabulary: Core imports it
    // (for `Finalize`'s uninhabited default row arg) but cannot re-export it
    // either, and `type M`'s own RHS below is exactly where an unpinned
    // `Finalize` row spells `Finalize Void` — without this import that
    // reference is "Not in scope: type constructor 'Void'", which cascades
    // into every downstream module reporting confusing fallout ("does not
    // export", "which is not loaded") via extract's own non-topological
    // diagnostic-recovery pass (see `validate_finalize_row`'s doc for that
    // same recovery-pass hazard, there for a PINNED row). Gated on
    // `Finalize`'s presence (unlike the pair above, needed by any row-
    // dependent effect generally) so a vocabulary without `Finalize` never
    // carries an unused import.
    if row_effects.iter().any(|e| e.type_name == "Finalize") {
        out.push_str("import Data.Void (Void)\n");
    }
    // Author modules defining the types this row is applied to (e.g. the
    // harness's `HarnessTypes` for a `Finalize Decision` row).
    for m in row.imports() {
        out.push_str(&format!("import {m}\n"));
    }
    out.push('\n');

    // ROW-DEPENDENT effects ([`ROW_DEPENDENT_EFFECTS`] — excluded from Core
    // entirely, see that constant's doc): declared HERE instead, per window,
    // exactly as the whole generated surface used to work before the split
    // (and named in the export list above, computed from this SAME set).
    for eff in &row_dependent {
        eff.type_defs.iter().for_each(|td| {
            out.push_str(td);
            out.push('\n');
        });
        out.push_str(&format!(
            "data {}{} a where\n",
            eff.type_name,
            eff.type_params
                .iter()
                .map(|p| format!(" {p}"))
                .collect::<String>()
        ));
        eff.constructors.iter().for_each(|ctor| {
            out.push_str(&format!("  {}\n", ctor));
        });
        out.push('\n');
        for h in eff.helpers {
            out.push_str(h);
            out.push('\n');
        }
    }

    // Type alias so helpers can write `M a` instead of `Eff '[Console, KV, Fs] a`.
    if !row_effects.is_empty() {
        out.push_str(&format!("type M = Eff {}\n", row_type(row_effects, row)));
        // A throwaway value binding that forces `M` (hence the whole applied
        // row, including a pinned `Finalize <T>`'s answer type) to resolve —
        // the probe target `tidepool-harness::engine::validate_finalize_row`
        // extracts to catch an unresolved row application HERE rather than as
        // a confusing cascade in whatever turn module happens to import this
        // shim (see that function's doc). Never called; dead by construction.
        out.push_str("__shimProbe :: M ()\n__shimProbe = pure ()\n");
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
    row_type(effects, &crate::RowArgs::default())
}

/// [`build_effect_stack_type`] with the row's parameterized effects applied to
/// explicit type arguments. A turn module's own `result :: Eff <stack> a` must
/// name the SAME row as the generated `type M` it compiles against.
pub fn build_effect_stack_type_at(effects: &[EffectDecl], row: &crate::RowArgs) -> String {
    row_type(effects, row)
}

/// The promoted effect row (`'[Console, KV, Finalize Decision]`) — one entry
/// per decl, each parameterized effect applied to its row arguments.
fn row_type(effects: &[EffectDecl], row: &crate::RowArgs) -> String {
    if effects.is_empty() {
        return "'[]".to_string();
    }
    let entries: Vec<String> = effects.iter().map(|e| crate::row_entry(e, row)).collect();
    format!("'[{}]", entries.join(", "))
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

/// How a turn's terminal `_r` renders to the caller — see [`TurnTemplate::render`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Render {
    /// The stateless eval server's machine-JSON contract.
    #[default]
    ToJson,
    /// The REPL's Show-default contract: `Text` renders bare (not
    /// show-quoted), `Value` passes through as structured JSON, any other
    /// `Show a` renders via `show`. The `ToWire` class is always emitted by
    /// [`crate::build_preamble`] / [`crate::build_preamble_non_interactive`].
    ToWire,
}

/// Options for [`TurnTemplate::render`] — the wrap-`code`-in-a-module
/// template shared by [`template_haskell`]/[`template_haskell_anchored`]/
/// [`template_haskell_show_default`] (thin named wrappers kept below because
/// callers read better with a name). Construct this directly for a
/// combination none of them name, e.g. `render: Render::ToWire` +
/// `anchor_result: true` (a REPL turn against a real `Finalize` row).
///
/// `anchor_result`: when `true`, the result binding routes `_r` through a
/// generated `__anchor :: P.Show a => a -> a; __anchor = P.id` before
/// rendering it. This is ADDITIVE, not a type pin: `__anchor` is `id`, so it
/// never forces `_r`'s type to anything — it only adds a `Show a0` constraint
/// alongside the render call's `ToJSON`/`ToWire` one.
///
/// Why that's needed at all: GHC's defaulting (even under
/// `ExtendedDefaultRules`, even with an explicit `default (...)` list naming
/// a type with a matching instance) requires the ambiguous variable's
/// constraint set to carry at least one class from GHC's own fixed "standard"
/// set — the GHC User's Guide's `ExtendedDefaultRules` section states rule 3
/// as relaxed to "at least one of the classes Ci is numeric, or is Show, Eq,
/// or Ord" (a relaxation of the anchor requirement, never its removal). A
/// solitary `ToJSON a0`/`ToWire a0` — both ordinary library classes with no
/// superclass — never qualifies, so `finalize`'s intentionally free result
/// tyvar (`finalize :: forall v a effs. Member (Finalize v) effs => v -> Eff
/// effs a`, `a` unconstrained by design) is permanently unreachable by
/// defaulting on its own. Confirmed empirically, not by reasoning about the
/// docs: a minimal `IO`-do-block repro and a real `freer-simple` `Eff`-row
/// repro fail IDENTICALLY under identical pragmas (ruling out any
/// `Eff`-row/`MonoLocalBinds`/implication-specific explanation), and adding a
/// bare `Num` constraint on the SAME otherwise-ambiguous tyvar, nothing else
/// changed, makes defaulting fire — that is what `__anchor` supplies, in the
/// one turn shape that needs it, without touching a type any concretely-typed
/// result (an ordinary eval, or an answerer turn whose block doesn't reach
/// `finalize`) already has.
///
/// `__anchor` only becomes load-bearing when `_r`'s type is otherwise
/// ambiguous — which happens ONLY for a turn compiled against a real
/// `Finalize T` row whose block's result is `finalize`'s free `a`. Any
/// concretely-typed result (an ordinary eval's `Value`/`Int`/record, or an
/// answerer turn that suspends on `askUser` without finalizing) already has a
/// resolved type with its own `Show` instance available, so `__anchor`
/// resolves trivially and changes nothing observable — this is why it's safe
/// to add without threading the hole's concrete answer type through at all,
/// only a per-turn boolean (see `tidepool-harness::engine::template_turn_for`).
#[derive(Debug, Clone, Copy, Default)]
pub struct TurnTemplate<'a> {
    pub preamble: &'a str,
    pub effect_stack: &'a str,
    pub code: &'a str,
    pub imports: &'a str,
    pub helpers: &'a str,
    pub input: Option<&'a serde_json::Value>,
    pub budget: Option<u32>,
    pub render: Render,
    pub anchor_result: bool,
    /// Additional top-level entries beyond the primary `result`, as `(name,
    /// code)` pairs — `name` becomes the entry's own top-level binder (an
    /// extract `--targets` name a caller can compile alongside `result` in
    /// the SAME module), rendered through the identical shared path
    /// [`Self::render_entry_wrapper`]/[`Self::render_entry_body`] `result`
    /// itself uses, so the two are identical by construction rather than a
    /// hand-copied second shape that can drift from the template it imitates
    /// (`tidepool-harness`'s render+loop fusion is the first caller). Empty
    /// by default (`..Default::default()`), so every pre-existing caller's
    /// output is byte-identical to before this field existed — see
    /// `template_haskell_pin` in this module's tests. Only the PRIMARY entry
    /// (`result`, from `code`) ever emits the `-- [user-lines] S:E` marker
    /// (`tidepool-runtime/src/diag.rs` maps a GHC error back to the user's
    /// block by its FIRST occurrence, and there is only ever one user block)
    /// — an extra entry's code is never treated as user-authored input.
    pub extra_entries: &'a [(&'a str, &'a str)],
    /// When `true`, entry bodies render `toJSON _r`/`toWire _r` with NO
    /// `paginateResult` wrapper. Pagination is DISPLAY semantics — a stub
    /// replaces oversize structure — so any caller that round-trips an
    /// entry's JSON back into a typed value (the self-harness state crossing)
    /// must opt out or watch its state decode fail once it outgrows the page
    /// bound (an `ideas` array stubbed to a string was the live failure).
    /// Default `false`: every pre-existing caller's bytes are unchanged.
    pub unpaginated: bool,
    /// PRD 21 C5: when `true`, every entry's result binding applies
    /// `Tidepool.Agent.Delegate.runDelegate` to its own binder — `_r <-
    /// runDelegate <binder>` instead of `_r <- <binder>` — so the entry's
    /// body (`code`) compiles at the narrow `Delegate ': effs` row
    /// `runDelegate` peels back from, while `self.effect_stack` (this
    /// entry's own signature) keeps naming the REAL, dispatched OUTER row.
    /// This is the "wrap lives in the template's RESULT position" mechanism
    /// (`tidepool-harness::engine::delegate_aware_preamble`'s doc has the
    /// full story, including why `M` is redefined locally rather than
    /// touched here) — `code` itself is never textually rewritten. Default
    /// `false`: every pre-existing caller's bytes are unchanged.
    pub delegate_wrap: bool,
}

impl TurnTemplate<'_> {
    pub fn render(&self) -> String {
        let mut out = String::new();

        // Preamble contains: pragmas, module header, standard imports, default decl,
        // data declarations, type alias. User imports must go after standard imports
        // (after "import Control.Monad.Freer\n") and before "default".
        if !self.imports.is_empty() {
            let insert_point = self
                .preamble
                .find("default (Int")
                .unwrap_or(self.preamble.len());
            out.push_str(&self.preamble[..insert_point]);
            for imp in self
                .imports
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
            {
                out.push_str(&format!("import {}\n", imp));
            }
            out.push_str(&self.preamble[insert_point..]);
        } else {
            out.push_str(self.preamble);
        }

        // Marker for user code section (used by error formatting to trim preamble)
        out.push_str("-- [user]\n");

        if !self.helpers.is_empty() {
            out.push_str(self.helpers);
            if !self.helpers.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
        }

        // Inject input binding if provided
        out.push_str(&input_binding_source(self.input));

        // The primary entry: `__user`/`result`, from `self.code` — the ONLY
        // entry that emits the `-- [user-lines]` marker.
        self.render_entry_wrapper(&mut out, "__user", self.code, true);

        // The defaulting anchor (see this struct's doc): `id` under a `Show`
        // constraint, so it never forces `_r`'s type — only adds the standard-
        // class anchor `ToJSON`/`ToWire` alone can never supply. Emitted only
        // when `anchor_result` is set (a real `Finalize T` row); every other
        // caller's `_r` is rendered exactly as before. Declared ONCE, ahead of
        // every entry body (including any extra entries), since it is a single
        // top-level binding shared module-wide, not a per-entry one.
        if self.anchor_result {
            out.push_str("__anchor :: P.Show a => a -> a\n__anchor = P.id\n\n");
        }
        self.render_entry_body(&mut out, "__user", "result");

        // Each extra entry gets its OWN wrapper binder (derived from its name,
        // so it cannot collide with `__user` or a sibling extra entry) and is
        // rendered through the SAME two helpers `result` used above — the
        // `_r <- <binder>` bind, budget conditionals, `__anchor`,
        // `paginateResult`, `toJSON`/`toWire` are therefore shared code, never
        // a hand-maintained copy. One blank line separates consecutive entries;
        // none trails the last, so output is byte-identical to before this
        // field existed whenever `extra_entries` is empty.
        for (i, (name, code)) in self.extra_entries.iter().enumerate() {
            if i == 0 {
                out.push('\n');
            }
            let binder = format!("{name}Impl");
            self.render_entry_wrapper(&mut out, &binder, code, false);
            self.render_entry_body(&mut out, &binder, name);
            if i + 1 < self.extra_entries.len() {
                out.push('\n');
            }
        }

        out
    }

    /// Emit one entry's wrapper binding: `<binder> = let { __b = <code> } in
    /// __b`, embedding `code` VERBATIM (no indentation transform — see
    /// `render`'s doc on why) followed by exactly one blank line. `__b` is
    /// local to this binding's own `let`, so it never collides across
    /// entries even though every entry uses the same local name. When
    /// `emit_user_lines` is set, the closing bracket line also carries the
    /// `-- [user-lines] S:E` 1-based line range of `code` within the module
    /// built so far (`tidepool-runtime/src/diag.rs` reads the FIRST such
    /// marker) — reserved for the PRIMARY entry only; an extra entry's code
    /// is runtime-generated, not user-authored, so it never carries one.
    fn render_entry_wrapper(
        &self,
        out: &mut String,
        binder: &str,
        code: &str,
        emit_user_lines: bool,
    ) {
        out.push_str(&format!("{binder} = let {{\n __b =\n"));
        // 1-based inclusive line range of `code` within this module: `start` is
        // the line right after this bracket (where `code`'s first line lands);
        // `end` follows from `code`'s own newline count.
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
        if emit_user_lines {
            out.push_str(&format!(
                " }} in __b  -- [user-lines] {start_line}:{end_line}\n"
            ));
        } else {
            out.push_str(" } in __b\n");
        }
        out.push('\n');
    }

    /// Emit one entry's top-level function: `<name> :: Eff <stack>
    /// Value; <name> = do { _r <- <binder>; …; paginateResult … (<render_call>
    /// <rendered>) }` — the budget/anchor conditionals and the `toJSON`/`toWire`
    /// render call are identical for every entry, sourced from `self` alone.
    fn render_entry_body(&self, out: &mut String, binder: &str, name: &str) {
        // render_call: toWire in REPL (Show-default), toJSON in stateless server.
        let render_call = if self.render == Render::ToWire {
            "toWire"
        } else {
            "toJSON"
        };
        let rendered = if self.anchor_result {
            "(__anchor _r)"
        } else {
            "_r"
        };
        let source = if self.delegate_wrap {
            format!("runDelegate {binder}")
        } else {
            binder.to_string()
        };

        out.push_str(&format!("{name} :: Eff {} Value\n", self.effect_stack));
        out.push_str(&format!("{name} = do\n"));
        if self.budget.is_some() {
            out.push_str("  kvSet \"__sayChars\" (toJSON (0 :: Int))\n");
        }
        out.push_str(&format!("  _r <- {source}\n"));
        if self.unpaginated {
            out.push_str(&format!("  pure ({render_call} {rendered})\n"));
        } else if let Some(b) = self.budget {
            out.push_str("  _scV <- kvGet \"__sayChars\"\n");
            out.push_str("  let _sayC = case _scV of { Just b -> case b ^? _Int of { Just n -> n; _ -> 0 }; Nothing -> 0 }\n");
            out.push_str(&format!(
                "  paginateResult (max 100 ({} - _sayC)) ({render_call} {rendered})\n",
                b
            ));
        } else {
            out.push_str(&format!(
                "  paginateResult 4096 ({render_call} {rendered})\n"
            ));
        }
    }
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
    TurnTemplate {
        preamble,
        effect_stack,
        code,
        imports,
        helpers,
        input,
        budget,
        ..Default::default()
    }
    .render()
}

/// Like [`template_haskell`], but the result binding carries an extra `Show`
/// anchor (see [`TurnTemplate`]'s `anchor_result` doc) so a turn compiled
/// against a real `Finalize T` row can resolve `finalize`'s free result
/// tyvar. Every OTHER caller keeps using [`template_haskell`] /
/// [`template_haskell_show_default`] unpinned — this is additive to a single
/// turn's own module, never a change to the shared template those callers see.
pub fn template_haskell_anchored(
    preamble: &str,
    effect_stack: &str,
    code: &str,
    imports: &str,
    helpers: &str,
    input: Option<&serde_json::Value>,
    budget: Option<u32>,
) -> String {
    TurnTemplate {
        preamble,
        effect_stack,
        code,
        imports,
        helpers,
        input,
        budget,
        anchor_result: true,
        ..Default::default()
    }
    .render()
}

/// Like [`template_haskell`] but uses `toWire` instead of `toJSON` for the
/// result — see [`Render::ToWire`].
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
    TurnTemplate {
        preamble,
        effect_stack,
        code,
        imports,
        helpers,
        input,
        budget,
        render: Render::ToWire,
        ..Default::default()
    }
    .render()
}

/// Escape a string for inclusion in a generated Haskell string literal (the
/// BODY only — no surrounding quotes). Control characters matter: an
/// unescaped newline in the payload is a LEXICAL ERROR in the generated module
/// (bit the eval `input` channel for every multi-line payload). Shared with
/// the self-iterating harness's `State`/compaction splice (J2), so every
/// code path that emits a Haskell string literal escapes it identically.
pub fn escape_haskell_string(s: &str) -> String {
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

    /// The quasi-quoter open-tokens `uses_qq` must recognize.
    const QQ_TOKENS: &[&str] = &["[fmt|", "[j|", "[patch|", "[uri|"];

    proptest! {
        /// Any text containing a quoter open-token is detected as QQ, no matter
        /// what surrounds it.
        #[test]
        fn prop_uses_qq_detects_every_token(
            // Bound by the table's ACTUAL length — a literal here once drifted
            // below a 5-element table, so the last token was never generated.
            tok in 0usize..QQ_TOKENS.len(),
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
        // Ask and RunLLMTurn are both interposed (WS-B split runLLMTurn out
        // of Ask), appended in that order after the base stack. Fork is NOT
        // here: the ordinary session engine's request parser never accepts
        // ForkWith/ForkAllWith (vestigial-subsystems review §4) — the
        // harness Agent turn's own roster (`agent_decls`) adds Fork on top
        // of this one explicitly.
        assert_eq!(a[8], "Ask");
        assert_eq!(a.last(), Some(&"RunLLMTurn"));
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
        // dropped quoters are NOT special: glob (removed), sg (cut with the
        // SG effect), and form (deleted with the forms one-algebra
        // consolidation) must all classify as non-QQ.
        assert!(!uses_qq("pure [glob|src/*.rs|]"));
        assert!(!uses_qq("pure [sg|fn $NAME|]"));
        assert!(!uses_qq("askUserRaw (toJSON [form|choice ok?: yes no|])"));
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
                extra_imports: &[],
                helpers: &[],
                type_params: &[],
                default_row_args: &[],
                prompt_card: None,
                helpers_row_polymorphic: false,
            },
            EffectDecl {
                type_name: "KV",
                description: "",
                constructors: &[],
                type_defs: &[],
                extra_imports: &[],
                helpers: &[],
                type_params: &[],
                default_row_args: &[],
                prompt_card: None,
                helpers_row_polymorphic: false,
            },
            EffectDecl {
                type_name: "Fs",
                description: "",
                constructors: &[],
                type_defs: &[],
                extra_imports: &[],
                helpers: &[],
                type_params: &[],
                default_row_args: &[],
                prompt_card: None,
                helpers_row_polymorphic: false,
            },
        ];
        assert_eq!(build_effect_stack_type(&effects), "'[Console, KV, Fs]");
        assert_eq!(build_effect_stack_type(&[]), "'[]");
    }

    /// `Finalize` is type-indexed: its GADT head carries `v`, and its row entry
    /// is APPLIED — to the hole's answer type when the compile supplies one, to
    /// the uninhabited canonical `Void` when it doesn't. `Member (Finalize T)`
    /// is then the whole pin: no shim, no hiding, no per-compile symbol
    /// manipulation.
    /// The GADT itself lives in the STABLE Core text — only `type M` (the
    /// shim) instantiates it per row.
    #[test]
    fn finalize_row_entry_is_applied_to_its_answer_type() {
        let decls = vec![crate::askuser_decl(), crate::finalize_decl()];

        let core = effects_core_module_source(&decls);
        assert!(
            core.contains("module Tidepool.Effects.Core where"),
            "{core}"
        );
        assert!(core.contains("data Finalize v a where"), "{core}");
        assert!(
            core.contains("FinalizeWith :: Int -> v -> Finalize v a"),
            "{core}"
        );
        assert!(core.contains("import Data.Void (Void)"), "{core}");
        assert!(
            core.contains(
                "finalize :: forall v a effs. Member (Finalize v) effs => v -> Eff effs a"
            ),
            "{core}"
        );
        // Core never DECLARES `type M` — that's the shim's whole reason to
        // exist. Checked as `"type M ="` (the declaration form), not a bare
        // `"type M"` substring: Core's own header comment explains this
        // property in prose ("...no `type M`)...") and a bare substring
        // check false-positives on that comment — pre-existing, unrelated to
        // this test's own subject, caught incidentally while verifying the
        // `Void` import fix below.
        assert!(!core.contains("type M ="), "{core}");

        // No contract: the shim's row names the uninhabited default, so a turn
        // that isn't answering a typed hole simply cannot finalize.
        let shim = effects_shim_module_source(&decls, &crate::RowArgs::default());
        assert!(
            shim.contains("type M = Eff '[AskUser, Finalize Void]"),
            "{shim}"
        );
        // The shim must import `Void` itself — Core imports it too, but (like
        // `Eff`/`Member`/`send` from freer-simple) Core's own implicit export
        // list re-exports only what it DEFINES, never what it merely imports,
        // so `type M`'s own `Finalize Void` reference needs its own import
        // here or it's "Not in scope: Void" — a real, previously-undetected
        // regression this project's own GHC-heavy acceptance tier caught.
        assert!(shim.contains("import Data.Void (Void)"), "{shim}");
        // AND the shim must RE-EXPORT `Void` too (not just import it for its
        // own internal `type M` use): a TURN module's own `import
        // Tidepool.Effects` (unqualified, no import list) needs `Void` in
        // scope for its OWN `result :: Eff <promoted-row> Value` signature —
        // the turn template splices the promoted row directly, independent
        // of `type M` — and an import brings in only what the imported
        // module itself EXPORTS.
        assert!(
            shim.contains("module Tidepool.Effects (module Tidepool.Effects.Core, M, Void)"),
            "{shim}"
        );

        // With a contract: the row is instantiated at the answer type, and the
        // shim imports the author module that defines it — Core is untouched.
        let row = crate::RowArgs::at("Finalize", ["Decision"]).importing(["HarnessTypes"]);
        let pinned = effects_shim_module_source(&decls, &row);
        assert!(
            pinned.contains("type M = Eff '[AskUser, Finalize Decision]"),
            "{pinned}"
        );
        assert!(pinned.contains("\nimport HarnessTypes\n"), "{pinned}");
        assert_eq!(
            build_effect_stack_type_at(&decls, &row),
            "'[AskUser, Finalize Decision]"
        );

        // Two answer types must not share a generated SHIM (hence not a
        // staging dir either — it is content-addressed on this source) — but
        // DO share the identical Core text, since neither touches vocabulary.
        let other = crate::RowArgs::at("Finalize", ["Contribution"]).importing(["HarnessTypes"]);
        assert_ne!(pinned, effects_shim_module_source(&decls, &other));
        assert_eq!(core, effects_core_module_source(&decls));
    }

    /// A row entry stays ONE element: a function or applied answer type is
    /// parenthesized (`Finalize (Int -> Int)`), never split across the list.
    #[test]
    fn compound_answer_type_is_parenthesized_in_the_row() {
        let decls = vec![crate::finalize_decl()];
        let row = crate::RowArgs::at("Finalize", ["Int -> Int"]);
        assert_eq!(
            build_effect_stack_type_at(&decls, &row),
            "'[Finalize (Int -> Int)]"
        );
    }

    /// A vocabulary-only PARAMETERIZED effect (`Finalize`, not in the row at
    /// all here) still gets a fully generic GADT + `Member`-polymorphic
    /// `finalize` in Core — nameable everywhere, executable only where its row
    /// entry actually appears (a comprehensible unsolved-`Member` error
    /// elsewhere). This generalizes the old RunLLMTurn-only "nameable
    /// everywhere" carve-out to every vocabulary effect, since Core no longer
    /// has a row-closed-helper hazard to protect against (every helper is
    /// Member-polymorphic by construction — see `effects_core_module_source`'s
    /// own assertion).
    #[test]
    fn vocab_only_parameterized_effect_is_generic_and_nameable() {
        let vocab = vec![crate::askuser_decl(), crate::finalize_decl()];
        let core = effects_core_module_source(&vocab);
        assert!(core.contains("data Finalize v a where"), "{core}");
        assert!(
            core.contains(
                "finalize :: forall v a effs. Member (Finalize v) effs => v -> Eff effs a"
            ),
            "{core}"
        );

        // A shim whose OWN row omits Finalize entirely still resolves `finalize`
        // as a name (it's in Core) — only `Member (Finalize v) effs` fails to
        // solve, which is a GHC compile concern this pure test doesn't reach;
        // `tidepool-harness/tests/agent_stack_scoping.rs` is the real-compile
        // proof of that half.
        let shim = effects_shim_module_source(&[crate::askuser_decl()], &crate::RowArgs::default());
        assert!(shim.contains("type M = Eff '[AskUser]"), "{shim}");
    }

    /// Dedup by `type_name`: the same effect listed twice in the vocabulary
    /// renders its GADT exactly once.
    #[test]
    fn core_dedups_vocabulary_by_type_name() {
        let vocab = vec![crate::askuser_decl(), crate::askuser_decl()];
        let core = effects_core_module_source(&vocab);
        assert_eq!(core.matches("data AskUser a where").count(), 1, "{core}");
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

    /// A REAL compile error, through the REAL `template_haskell` wrapping (the
    /// preamble + effects module + a multi-line `helpers`/`code` payload), must
    /// have its `Expr.hs:<n>` line rebased onto the caller's own source — this
    /// is the eval/repl feedback-loop guarantee (`format_error_with_source` +
    /// `tidepool_runtime::diag::render_diagnostics`), not just a property of
    /// the synthetic strings the other tests in this module use.
    #[test]
    fn format_error_with_source_rebases_real_compile_error_line() {
        tidepool_testing::eval_harness::require_extract();
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
        let effects_dirs = crate::ensure_effects_module(&decls).expect("write effects module");
        let include = [
            prelude_dir.as_path(),
            user_lib_dir.as_path(),
            effects_dirs.core.as_path(),
            effects_dirs.shim.as_path(),
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

/// Pin test: byte-exact output of the `template_haskell*` wrapper paths.
/// Same discipline as `preamble.rs`'s `import_gating_pin`: hardcoded literal
/// expected text, not a call back into any production string-builder, so a
/// refactor of the templating internals cannot keep this green while
/// silently changing the emitted bytes. `TurnTemplate::render`'s output is a
/// compile-cache input
/// (it feeds `compile_haskell`'s source, salted independently of
/// `ensure_effects_module_at`'s own cache — see that fn's doc), so drift here
/// is a real regression, not cosmetic.
#[cfg(test)]
mod template_haskell_pin {
    use super::*;

    const PRE: &str = "module Expr where\ndefault (Int)\n";
    const STACK: &str = "'[Console]";
    const CODE: &str = "pure 1";

    #[test]
    fn plain_wrapper_pin() {
        let src = template_haskell(PRE, STACK, CODE, "", "", None, None);
        assert_eq!(
            src,
            "module Expr where\ndefault (Int)\n-- [user]\n__user = let {\n __b =\npure 1\n } in __b  -- [user-lines] 6:6\n\nresult :: Eff '[Console] Value\nresult = do\n  _r <- __user\n  paginateResult 4096 (toJSON _r)\n",
            "template_haskell output changed — this is a compile-cache input, see TurnTemplate::render"
        );
    }

    #[test]
    fn anchored_wrapper_pin() {
        let src = template_haskell_anchored(PRE, STACK, CODE, "", "", None, None);
        assert_eq!(
            src,
            "module Expr where\ndefault (Int)\n-- [user]\n__user = let {\n __b =\npure 1\n } in __b  -- [user-lines] 6:6\n\n__anchor :: P.Show a => a -> a\n__anchor = P.id\n\nresult :: Eff '[Console] Value\nresult = do\n  _r <- __user\n  paginateResult 4096 (toJSON (__anchor _r))\n",
            "template_haskell_anchored output changed — this is a compile-cache input, see TurnTemplate::render"
        );
    }

    #[test]
    fn show_default_wrapper_pin() {
        let src = template_haskell_show_default(PRE, STACK, CODE, "", "", None, None);
        assert_eq!(
            src,
            "module Expr where\ndefault (Int)\n-- [user]\n__user = let {\n __b =\npure 1\n } in __b  -- [user-lines] 6:6\n\nresult :: Eff '[Console] Value\nresult = do\n  _r <- __user\n  paginateResult 4096 (toWire _r)\n",
            "template_haskell_show_default output changed — this is a compile-cache input, see TurnTemplate::render"
        );
    }

    /// The `show_default=true, anchor_result=true` combination (a REPL turn
    /// compiled against a real `Finalize T` row) has no named wrapper —
    /// construct [`TurnTemplate`] directly for it.
    #[test]
    fn show_default_anchored_combination_is_expressible_and_pinned() {
        let src = TurnTemplate {
            preamble: PRE,
            effect_stack: STACK,
            code: CODE,
            render: Render::ToWire,
            anchor_result: true,
            ..Default::default()
        }
        .render();
        assert_eq!(
            src,
            "module Expr where\ndefault (Int)\n-- [user]\n__user = let {\n __b =\npure 1\n } in __b  -- [user-lines] 6:6\n\n__anchor :: P.Show a => a -> a\n__anchor = P.id\n\nresult :: Eff '[Console] Value\nresult = do\n  _r <- __user\n  paginateResult 4096 (toWire (__anchor _r))\n",
            "the newly-expressible show_default+anchor_result combination changed"
        );
    }

    /// An explicit empty `extra_entries` renders BYTE-IDENTICAL to the field
    /// not being set at all (the `..Default::default()` path every
    /// pre-existing caller takes) — same literal as `plain_wrapper_pin`,
    /// pinning the additive-field claim directly rather than only by
    /// implication.
    #[test]
    fn empty_extra_entries_is_byte_identical_to_default() {
        let src = TurnTemplate {
            preamble: PRE,
            effect_stack: STACK,
            code: CODE,
            extra_entries: &[],
            ..Default::default()
        }
        .render();
        assert_eq!(
            src,
            "module Expr where\ndefault (Int)\n-- [user]\n__user = let {\n __b =\npure 1\n } in __b  -- [user-lines] 6:6\n\nresult :: Eff '[Console] Value\nresult = do\n  _r <- __user\n  paginateResult 4096 (toJSON _r)\n",
        );
    }

    /// Two entries (the render+loop fusion shape): the primary `result` and
    /// ONE extra entry, sharing one module. Exactly one `-- [user-lines]`
    /// marker (only `result`'s), the extra entry's own wrapper/body rendered
    /// through the same shared helpers, and a blank line separating the two
    /// entries.
    #[test]
    fn one_extra_entry_shares_module_with_one_marker() {
        let src = TurnTemplate {
            preamble: PRE,
            effect_stack: STACK,
            code: CODE,
            extra_entries: &[("__loopEntry", "Loaded.loop __selfHarnessState")],
            ..Default::default()
        }
        .render();
        assert_eq!(
            src,
            "module Expr where\ndefault (Int)\n-- [user]\n__user = let {\n __b =\npure 1\n } in __b  -- [user-lines] 6:6\n\nresult :: Eff '[Console] Value\nresult = do\n  _r <- __user\n  paginateResult 4096 (toJSON _r)\n\n__loopEntryImpl = let {\n __b =\nLoaded.loop __selfHarnessState\n } in __b\n\n__loopEntry :: Eff '[Console] Value\n__loopEntry = do\n  _r <- __loopEntryImpl\n  paginateResult 4096 (toJSON _r)\n",
        );
        assert_eq!(
            src.matches("[user-lines]").count(),
            1,
            "exactly one user-lines marker per module, regardless of entry count"
        );
    }
}
