//! Single-source effect definitions.
//!
//! Each effect is defined ONCE as an exported `<effect>_effect_def!` macro — the
//! same callback idiom as [`crate::base_effects!`], applied per effect. The
//! definition carries every hand-locked fact about the effect as structured
//! tokens: the GADT constructors (with each argument's Haskell type string AND
//! Rust bridge type), return types, helper-verb docstrings, and supporting
//! `type_defs`. Two projections consume it:
//!
//! 1. [`effect_decl_projection!`] (this crate) — generates the `pub fn
//!    *_decl() -> EffectDecl` builder, producing output identical to the
//!    hand-written builders in `effect_decls.rs`.
//! 2. `effect_rust_projection!` (`tidepool-handlers/src/effect_glue.rs`) —
//!    generates the `#[derive(FromCore)] enum *Req`, the `DescribeEffect`
//!    impl, and the `EffectHandler` dispatch match whose arms call
//!    hand-written inherent methods on the handler struct.
//!
//! Because both projections expand the SAME definition, the GADT constructor
//! string, the Rust request variant, and the dispatch arm cannot desync — the
//! constructor↔Req↔handler-arm↔docstring drift class is closed by
//! construction, exactly as `base_effects!` closes the stack-order class.
//!
//! ## Definition grammar
//!
//! ```text
//! effect      <Name>,            Haskell GADT type name (== Rust-side prefix)
//! handler     <HandlerStruct>,   hand-written handler struct in tidepool-handlers
//! req         <ReqEnum>,         generated request enum name
//! decl_fn     <decl_fn_name>,    generated EffectDecl builder name
//! description [<lit>, ...],      concat!-joined effect description
//! type_defs   [<lit>, ...],      raw Haskell support decls, emitted before the GADT
//! verbs [
//!   { ctor <Ctor>,               GADT constructor == Req variant name (1:1, no rename)
//!     method <method_name>,      hand-written inherent method the dispatch arm calls
//!     args { a: "Text" as String, n: "Int" as i64 },   Haskell type / Rust type pairs
//!     ret "<haskell result>"     result type inside the effect (e.g. "()", "[Hit]",
//!                                "(Either Text Text)")
//!     [, errors <RustErrorEnum>] typed per-verb failure (#335) — see the errors block below
//!   }, ...
//! ],
//! helpers [
//!   { name <verb>, sig "<type>", doc [<line>, ...], body pointfree <Ctor> },
//!   { name <verb>, sig "<type>", doc [<line>, ...], body nullary <Ctor> },
//!   { name <verb>, sig "<type>", doc [<line>, ...], body applied <Ctor>(a, b) },
//!   { raw [<haskell line>, ...] },        escape hatch: arbitrary helper Haskell
//!   { raw substrate [<haskell line>, ...] }, same, but tagged substrate — an
//!                                        implementation detail excluded from the
//!                                        derived model-facing index (describe.rs)
//! ],
//! ```
//!
//! The three `body` forms generate the thin send-wrapper text
//! (`v = send . C` / `v = send C` / `v a b = send (C a b)`); `raw` passes
//! newline-joined Haskell through for non-thin helpers.
//!
//! ## The `errors` block (#335 — typed per-verb failure)
//!
//! An effect declares its failure ADT ONCE, as a top-level block between
//! `type_defs` and `verbs`:
//!
//! ```text
//! errors FsError [
//!   { ctor FsNotFound, fields { path: "Text" as String }, doc "path does not exist" },
//!   { ctor FsIo,       fields { detail: "Text" as String }, doc "other I/O failure" },
//! ],
//! ```
//!
//! and tags each fallible verb with `errors FsError` in its row. Both
//! projections consume the block:
//!
//! - **Decl projection** ([`effect_decl_projection!`]) appends
//!   `data FsError = FsNotFound Text | FsIo Text deriving (Show, Eq)` to the
//!   effect's `type_defs` (via [`error_decl_text!`]), and renders an
//!   `errors`-tagged verb's result as `<Effect> (Either FsError <ret>)` (via
//!   [`ctor_sig!`]). The `Either` threads through the generated helper sigs.
//! - **Rust projection** (`effect_rust_projection!`) emits
//!   `#[derive(ToCore, FromCore, Debug, PartialEq, Eq)] pub enum FsError {
//!   FsNotFound(String), … }` (ToCore/FromCore use plain name+arity lookup, like
//!   the bridged records — the variant names are unique), and an
//!   `errors`-tagged dispatch arm calls a hand-written method
//!   returning `Result<T, FsError>` and wraps it with `cx.respond` (Ok→Right,
//!   Err→Left) — so the handler is total by construction. Only untagged verbs
//!   keep the `Result<Response, EffectError>` method shape.
//!
//! The error ADT's `data` decl is single-sourced from the same block that
//! generates the Rust enum, so the two cannot drift.
//!
//! **Eager-list invariant.** Errors-tagging a LIST-returning verb makes its
//! result EAGER: a verb-level `Left` is decided at the boundary, and
//! `Right(lazy-stream)` is inexpressible with the current `Response` enum — so
//! the whole `Either <Err> [T]` is a `Complete` value (stack-safe and capped,
//! but not lazily streamed). Per-item failure verbs (the `Either` rides
//! inside each element, e.g. `FsReadGlob`'s `[FileRead]`) stay lazy. The
//! machine-level gap is tracked in #341 — don't `errors`-tag a list verb that
//! needs laziness. This is why Exec/Http/Git/Llm (scalar/`Value`/record
//! results) are fine to tag.

/// The sentinel [`crate::describe::helper_is_substrate`] checks for,
/// verbatim, as a helper's rendered text's first line. Emitted only by
/// `helper_text!`'s `raw substrate [...]` form (below) — the single place
/// that decides whether a helper is substrate, replacing a hand-maintained
/// per-(effect, helper-name) allowlist that lived in `describe.rs` and had
/// to be updated by hand every time a new substrate helper was added
/// elsewhere. A plain Haskell line comment: inert in the compiled module,
/// and already skipped by `helper_sig`/`helper_name` (both ignore
/// `--`-prefixed lines), so marking a helper substrate changes nothing
/// about whether it compiles or is callable — only whether the derived
/// model-facing index lists it. Defined as a macro (not a `const`) because
/// it is spliced into other macros' `concat!` calls, which require literal
/// tokens.
macro_rules! substrate_marker {
    () => {
        "-- @substrate-helper@"
    };
}
pub(crate) use substrate_marker;

/// Render one helper entry of a definition to its Haskell source string.
///
/// Kept separate from [`effect_decl_projection!`] so each `{ ... }` helper
/// group (captured as a single `tt`) can be re-matched against the per-form
/// arms. All output is assembled with `concat!` — the decl builder stays a
/// `&'static str` table exactly like the hand-written ones.
macro_rules! helper_text {
    // Point-free thin wrapper over a unary constructor: `v = send . Ctor`.
    ({ name $n:ident, sig $sig:literal,
       doc [$d0:literal $(, $d:literal)* $(,)?],
       body pointfree $ctor:ident $(,)? }) => {
        concat!(
            "-- | ", $d0, $("\n-- ", $d,)*
            "\n", stringify!($n), " :: ", $sig,
            "\n", stringify!($n), " = send . ", stringify!($ctor)
        )
    };
    // Nullary constructor: `v = send Ctor`.
    ({ name $n:ident, sig $sig:literal,
       doc [$d0:literal $(, $d:literal)* $(,)?],
       body nullary $ctor:ident $(,)? }) => {
        concat!(
            "-- | ", $d0, $("\n-- ", $d,)*
            "\n", stringify!($n), " :: ", $sig,
            "\n", stringify!($n), " = send ", stringify!($ctor)
        )
    };
    // Multi-arg constructor: `v a b = send (Ctor a b)`.
    ({ name $n:ident, sig $sig:literal,
       doc [$d0:literal $(, $d:literal)* $(,)?],
       body applied $ctor:ident ( $($a:ident),+ $(,)? ) $(,)? }) => {
        concat!(
            "-- | ", $d0, $("\n-- ", $d,)*
            "\n", stringify!($n), " :: ", $sig,
            "\n", stringify!($n), $(" ", stringify!($a),)+ " = send (",
            stringify!($ctor), $(" ", stringify!($a),)+ ")"
        )
    };
    // Escape hatch, SUBSTRATE: identical to `raw` below, but the rendered
    // text carries `substrate_marker!()` as its first line — an
    // implementation detail a model should not call directly (the typed
    // wrapper's own doc names it). Still fully compiled and importable;
    // only `describe.rs`'s derived model-facing index excludes it.
    ({ raw substrate [$l0:literal $(, $l:literal)* $(,)?] $(,)? }) => {
        concat!($crate::effect_defs::substrate_marker!(), "\n", $l0 $(, "\n", $l)*)
    };
    // Escape hatch: arbitrary Haskell, newline-joined.
    ({ raw [$l0:literal $(, $l:literal)* $(,)?] $(,)? }) => {
        concat!($l0 $(, "\n", $l)*)
    };
}
pub(crate) use helper_text;

/// Render one GADT constructor signature string. An `errors`-tagged verb wraps
/// its result in `Either <ErrEnum>` (the #335 typed-failure surface); a plain
/// verb renders the bare result. A separate macro so the optional per-verb
/// `errors` tag selects the arm.
macro_rules! ctor_sig {
    // errors-tagged: `<Ctor> :: <args> -> <Eff> <tyParams…> (Either <Err> <ret>)`.
    ({ $eff:ident, [ $($tp:ident),* $(,)? ], $ctor:ident, [ $($ah:literal),* $(,)? ], $ret:literal, errors $everr:ident }) => {
        concat!(
            stringify!($ctor), " :: ",
            $($ah, " -> ",)*
            stringify!($eff), $(" ", stringify!($tp),)* " (Either ", stringify!($everr), " ", $ret, ")"
        )
    };
    // plain: `<Ctor> :: <args> -> <Eff> <tyParams…> <ret>`.
    ({ $eff:ident, [ $($tp:ident),* $(,)? ], $ctor:ident, [ $($ah:literal),* $(,)? ], $ret:literal }) => {
        concat!(
            stringify!($ctor), " :: ",
            $($ah, " -> ",)*
            stringify!($eff), $(" ", stringify!($tp),)* " ", $ret
        )
    };
}
pub(crate) use ctor_sig;

/// Render a definition's `type_params [v]` group as the `&'static [&'static
/// str]` [`crate::EffectDecl::type_params`] field.
macro_rules! ty_param_names {
    ([ $($tp:ident),* $(,)? ]) => { &[ $(stringify!($tp)),* ] };
}
pub(crate) use ty_param_names;

/// Render an optional `helpers_row_polymorphic <bool>` grammar token to its
/// `bool` value, defaulting to `false` when the definition omits the clause
/// entirely (every effect except `RunLLMTurn`, today).
macro_rules! opt_bool_or_false {
    () => {
        false
    };
    ($b:tt) => {
        $b
    };
}
pub(crate) use opt_bool_or_false;

/// Render an optional `prompt_card [...]` grammar token to
/// [`crate::EffectDecl::prompt_card`]'s `Option<&'static str>`, defaulting to
/// `None` when the definition omits the clause (every effect whose
/// `description` is already compact enough to re-send every turn).
macro_rules! opt_prompt_card_or_none {
    () => {
        None
    };
    ([$($pc:literal),* $(,)?]) => {
        Some(concat!($($pc),*))
    };
}
pub(crate) use opt_prompt_card_or_none;

/// The [`crate::EffectDecl::extra_imports`] table: companion `import` lines a
/// generated effect's helpers need beyond the fixed eval surface
/// (`preamble::eval_import_lines`) — `Git`'s helpers build on the Git verbs
/// (`Tidepool.Git`), `AskUser`'s `Tidepool.Form` on `askUserRaw`. Every other
/// effect needs nothing beyond the fixed surface.
///
/// A MIGRATED effect does not appear here at all: its companion imports are
/// schema data in `tidepool-protocol` and are emitted straight into its
/// generated decl. `Exec` was the first to leave.
///
/// Matched on the effect's own identifier (`$eff` from the `_effect_def!`
/// call site) rather than added as a token to the shared `_effect_def!`
/// grammar: that grammar is ALSO consumed by `tidepool-handlers`'s
/// `effect_rust_projection!`, which has no `extra_imports` slot — keeping the
/// lookup here, matched only from [`effect_decl_projection!`]'s own
/// expansion, means adding a companion import never touches that sibling
/// crate. This is the ONE place per effect a companion import is declared —
/// see `preamble.rs`'s import fold, which is the other half of the old
/// two-planes-must-be-kept-in-sync-by-hand gate (friction #23).
macro_rules! extra_imports_for {
    (Git) => {
        &["import qualified Tidepool.Git as Git"]
    };
    // AskUser was migrated to the `tidepool-protocol` schema; its
    // `extra_imports` (`import Tidepool.Form`) is schema data now, emitted
    // straight into its generated decl. See
    // `tidepool-protocol/src/effects/ask_user.rs`.
    //
    // `Ask` stays hand-carried (see `ask_effect_def!`'s doc and
    // `tidepool-protocol/src/effects/ask.rs`'s module doc for why it can't
    // flip onto the generator), but its pure `Schema`/`schemaToValue`/
    // `isOpt`/`innerSchema` vocabulary moved OUT of the decl's `type_defs`
    // and into `Tidepool.Form.Schema` (issue #24's first act) — a SEPARATE
    // module from `Tidepool.Form` proper, because `Ask` (unlike the gated
    // `AskUser`) is always in the ordinary roster, so this import must be
    // unconditional too; `Tidepool.Form` itself only compiles in a row
    // containing `AskUser`. `Tidepool.Form.Schema` deliberately does NOT
    // declare `llm` (a Llm-only roster without Ask, or an Ask-only roster
    // without Llm, both exist — `build_minimal_stack`'s Console-only rosters
    // are the latter) — see `Llm`'s own arm below and `Tidepool.Llm`'s module
    // doc for the regression this split fixes.
    (Ask) => {
        &["import Tidepool.Form.Schema"]
    };
    // `Llm`'s composed `llm` (built on `schemaToValue`, `Tidepool.Form.
    // Schema`'s vocabulary) lives in its OWN module, `Tidepool.Llm` —
    // independent of `Ask`'s arm above, because `Llm` and `Ask` are
    // independently-gated effects (see `Tidepool.Llm`'s module doc). `Llm`
    // is NOT a base effect (unlike `Entropy`/`KV` below): it is present only
    // in stacks that wire an `LlmHandler` (`build_base_stack`), so this
    // import is conditional on `Llm` actually being in the roster, exactly
    // like `Ask`'s arm is conditional on `Ask`.
    (Llm) => {
        &["import Tidepool.Llm"]
    };
    //
    // `Tidepool.Random`'s `mkStdGen`/`randomR`/`randoms`/`split` (pure) and
    // `newStdGen`/`randomRIO` (built on `entropySeed`) — same
    // built-on-the-raw-substrate-verb shape as `AskUser`/`Tidepool.Form`.
    // `Entropy` is a BASE effect (always in `base_effects!`, unlike the
    // gated AskUser/Fork), so this import is unconditionally live. It
    // cannot instead be reached through `Tidepool.Prelude`: the generated
    // `Tidepool.Effects` module itself imports `Tidepool.Prelude`, so a
    // Prelude re-export of anything importing `Tidepool.Effects` (as
    // `newStdGen`/`randomRIO` must) is a module cycle.
    (Entropy) => {
        &["import Tidepool.Random"]
    };
    // `Tidepool.Kv`'s `kvGetAs` builds on `KV`'s own `kvGet` (`Tidepool.Effects`),
    // same built-on-the-raw-substrate-verb shape as `Entropy`/`Tidepool.Random`
    // above. `KV` is a BASE effect (always in `base_effects!`), so this import
    // is unconditionally live, same reasoning as `Entropy`'s arm.
    (KV) => {
        &["import Tidepool.Kv"]
    };
    // Fork was migrated to the `tidepool-protocol` schema; its
    // `extra_imports` (`import Tidepool.Fork` — `fork`/`forkAll`/`forkMap`/
    // `forkCata` build on `Fork`'s own `forkSited`/`forkAllSited`) is schema
    // data now, emitted straight into its generated decl. See
    // `tidepool-protocol/src/effects/fork.rs`.
    //
    // `renderSpawnError` is DEFINED in `haskell/lib/Tidepool/Agent/Spawn.hs`
    // rather than emitted into the generated module, because it calls
    // `renderWorktreeError` — authored library code in `Tidepool.Worktree`,
    // which the generated `Tidepool.Effects` cannot import (that module imports
    // IT). A forced cross-row edit; see the scaffold doc §11.12.
    //
    // NARROW on purpose. The module's other exports (`spawnAgent`,
    // `spawnAsync`, `awaitAgent`, …) are reached by an explicit import today
    // and must keep being, or this row would silently WIDEN the eval surface
    // this migration promised to leave byte-identical.
    //
    // `Tidepool.Agent.Delegate`'s narrow surface rides the SAME
    // row-gate: it only compiles in a row containing `Subagent` (its
    // interpreter lowers onto `Subagent`'s own GADT), so it is auto-imported
    // here rather than through a second identifier — a row admitting
    // `Subagent` directly already has strictly MORE surface than one only
    // admitting `Delegate`, so widening this one arm cannot leak the narrow
    // effect's promise (unnameable `Worktree`/`Subagent`) anywhere the model
    // doesn't already have the wider surface it protects against.
    (Subagent) => {
        &[
            "import Tidepool.Agent.Spawn (renderSpawnError)",
            "import Tidepool.Agent.Delegate (Delegate, DelegateBrief (..), DelegateResult (..), DelegateRun (..), DelegateError (..), delegate, delegateTyped, delegateTypedFrom, delegateTypedIn, renderDelegateError, runDelegate)",
        ]
    };
    // Journal was migrated to the `tidepool-protocol` schema; its
    // `extra_imports` (`import qualified Tidepool.Resume as Resume` — the
    // READ half of the run journal) is schema data now, emitted
    // straight into its generated decl. See `tidepool-protocol/src/effects/journal.rs`.
    //
    // Green was migrated to the `tidepool-protocol` schema; its
    // `extra_imports` (`import Tidepool.Async` — the green-thread surface
    // rides `Green`'s substrate verbs) is schema data now, emitted straight
    // into its generated decl. See `tidepool-protocol/src/effects/green.rs`.
    ($other:ident) => {
        &[]
    };
}
pub(crate) use extra_imports_for;

/// Render one variant of an `errors` ADT to `<Ctor> <hsField> …` (Haskell
/// field types come from each field's `"<hs>" as <rust>` pair).
macro_rules! error_variant_text {
    ({ ctor $c:ident, fields { $($efn:ident : $efh:literal as $efr:ty),* $(,)? }, doc $d:literal $(,)? }) => {
        concat!(stringify!($c) $(, " ", $efh)*)
    };
}
pub(crate) use error_variant_text;

/// Render one `case` arm of a hand-templated `ToJSON` instance for an
/// `errors` ADT: `<Ctor> <fieldNames…> -> object ["tag" .= "<Ctor>",
/// "<fieldName>" .= <fieldName>, …]`. Field names double as the bound
/// pattern variables — reuses the same tokens `error_variant_text!` parses.
/// The vendored `ToJSON`'s generic-deriving default only covers
/// single-constructor records (see `Tidepool/Aeson/Value.hs`), so every
/// multi-constructor `errors` ADT needs an explicit instance; this macro is
/// what keeps that hand-templating single-sourced across all six effects.
macro_rules! error_variant_json_arm {
    ({ ctor $c:ident, fields { $($efn:ident : $efh:literal as $efr:ty),* $(,)? }, doc $d:literal $(,)? }) => {
        concat!(
            stringify!($c),
            $(" ", stringify!($efn),)*
            " -> object [\"tag\" .= (\"", stringify!($c), "\" :: Text)",
            $(", \"", stringify!($efn), "\" .= ", stringify!($efn),)*
            "]"
        )
    };
}
pub(crate) use error_variant_json_arm;

/// Render the Haskell `data <Err> = … deriving (Show, Eq)` decl PLUS a
/// hand-templated `instance ToJSON <Err>` for an `errors` block, so a raw
/// unhandled `Left <Err>` returned from an eval renders as tagged JSON
/// instead of crashing the render step. Peels the first variant so `|`
/// separators land between (not before) constructors.
macro_rules! error_decl_text {
    ($errname:ident, $first:tt $(, $rest:tt)* $(,)?) => {
        concat!(
            "data ", stringify!($errname), " = ",
            crate::effect_defs::error_variant_text!($first),
            $( " | ", crate::effect_defs::error_variant_text!($rest), )*
            " deriving (Show, Eq)",
            "\ninstance ToJSON ", stringify!($errname), " where\n",
            "  toJSON e = case e of\n",
            "    ", crate::effect_defs::error_variant_json_arm!($first), "\n",
            $("    ", crate::effect_defs::error_variant_json_arm!($rest), "\n",)*
            ""
        )
    };
}
pub(crate) use error_decl_text;

/// The tidepool-mcp projection: expand a definition into its `EffectDecl`
/// builder fn. Constructor strings render as
/// `<Ctor> :: <arg-hs> -> ... -> <Effect> <ret>`; helpers render via
/// [`helper_text!`]. The `handler`/`req`/`method` facts (and the reserved
/// `errors` slot) are Rust-side and ignored here.
macro_rules! effect_decl_projection {
    // `stable_errors true` variant of the unparameterized-effect wrapper arm
    // below — ONLY matched when a definition carries the extra
    // `stable_errors true,` marker (today: just `fs_effect_def!`). Forwards
    // to the `stable_errors` main arm instead of the normal one, so every
    // OTHER effect's invocation (no `stable_errors` token) still matches the
    // ORIGINAL arms further down, byte-for-byte unchanged.
    (
        effect $eff:ident,
        handler $handler:ident,
        req $req:ident,
        decl_fn $decl_fn:ident,
        $(prompt_card $pc:tt,)?
        $(helpers_row_polymorphic $hrp:tt,)?
        description $desc:tt,
        type_defs $td:tt,
        errors $errname:ident $evariants:tt,
        stable_errors true,
        verbs $verbs:tt,
        helpers $helpers:tt $(,)?
    ) => {
        crate::effect_defs::effect_decl_projection! {
            effect $eff,
            handler $handler,
            req $req,
            decl_fn $decl_fn,
            type_params [] default_row_args [],
            $(prompt_card $pc,)?
            $(helpers_row_polymorphic $hrp,)?
            description $desc,
            type_defs $td,
            errors $errname $evariants,
            stable_errors true,
            verbs $verbs,
            helpers $helpers,
        }
    };
    // `stable_errors true` main arm: identical to the normal main arm below,
    // except `type_defs` omits the `errors` block's inline `data`/`ToJSON`
    // text. These pre-schema domain types are already exported by
    // `Tidepool.Records.Stable` and the Prelude; Core must reuse that nominal
    // type rather than declare a duplicate. `fs_stable.rs` carries the same
    // `errname` block into the committed Haskell module.
    // The Rust-side projection (`effect_rust_projection!`) is unaffected —
    // it still generates the error enum from the same `errors` block exactly
    // as before; only where the HASKELL decl text lands changes.
    (
        effect $eff:ident,
        handler $handler:ident,
        req $req:ident,
        decl_fn $decl_fn:ident,
        type_params $tps:tt default_row_args [$($dra:literal),* $(,)?],
        $(prompt_card $pc:tt,)?
        $(helpers_row_polymorphic $hrp:tt,)?
        description [$($desc:literal),* $(,)?],
        type_defs [$($td:literal),* $(,)?],
        errors $errname:ident [
            $($evariant:tt),* $(,)?
        ],
        stable_errors true,
        verbs [
            $({ ctor $ctor:ident,
                method $method:ident,
                args { $($an:ident : $ah:literal as $ar:ty),* $(,)? },
                ret $ret:literal
                $(, errors $everr:ident)?
                $(,)?
            }),* $(,)?
        ],
        helpers [ $($helper:tt),* $(,)? ] $(,)?
    ) => {
        pub fn $decl_fn() -> $crate::EffectDecl {
            $crate::EffectDecl {
                type_name: stringify!($eff),
                description: concat!($($desc),*),
                prompt_card: crate::effect_defs::opt_prompt_card_or_none!($($pc)?),
                constructors: &[
                    $( crate::effect_defs::ctor_sig!(
                        { $eff, $tps, $ctor, [ $($ah),* ], $ret $(, errors $everr)? }
                    ) ),*
                ],
                // `errors $errname`'s decl text is deliberately NOT appended
                // here — it lives in the stable module instead (see the arm
                // doc comment above). Any OTHER literal `$td` entries still
                // ride along normally.
                type_defs: &[ $($td,)* ],
                extra_imports: crate::effect_defs::extra_imports_for!($eff),
                helpers: &[ $( crate::effect_defs::helper_text!($helper) ),* ],
                type_params: crate::effect_defs::ty_param_names!($tps),
                default_row_args: &[ $($dra),* ],
                helpers_row_polymorphic: crate::effect_defs::opt_bool_or_false!($($hrp)?),
            }
        }
    };
    // An unparameterized effect (all but `Finalize`): forward to the arm below
    // with an empty type-parameter list. Two arms rather than one optional
    // slot because the parameters are consumed INSIDE the per-constructor
    // repetition (each constructor's result type is the applied head,
    // `Finalize v a`), and macro_rules cannot nest an optional group there.
    (
        effect $eff:ident,
        handler $handler:ident,
        req $req:ident,
        decl_fn $decl_fn:ident,
        $(prompt_card $pc:tt,)?
        $(helpers_row_polymorphic $hrp:tt,)?
        description $desc:tt,
        type_defs $td:tt,
        $(errors $errname:ident $evariants:tt,)?
        verbs $verbs:tt,
        helpers $helpers:tt $(,)?
    ) => {
        crate::effect_defs::effect_decl_projection! {
            effect $eff,
            handler $handler,
            req $req,
            decl_fn $decl_fn,
            type_params [] default_row_args [],
            $(prompt_card $pc,)?
            $(helpers_row_polymorphic $hrp,)?
            description $desc,
            type_defs $td,
            $(errors $errname $evariants,)?
            verbs $verbs,
            helpers $helpers,
        }
    };
    (
        effect $eff:ident,
        handler $handler:ident,
        req $req:ident,
        decl_fn $decl_fn:ident,
        type_params $tps:tt default_row_args [$($dra:literal),* $(,)?],
        $(prompt_card $pc:tt,)?
        $(helpers_row_polymorphic $hrp:tt,)?
        description [$($desc:literal),* $(,)?],
        type_defs [$($td:literal),* $(,)?],
        $(errors $errname:ident [
            $($evariant:tt),* $(,)?
        ],)?
        verbs [
            $({ ctor $ctor:ident,
                method $method:ident,
                args { $($an:ident : $ah:literal as $ar:ty),* $(,)? },
                ret $ret:literal
                $(, errors $everr:ident)?
                $(,)?
            }),* $(,)?
        ],
        helpers [ $($helper:tt),* $(,)? ] $(,)?
    ) => {
        pub fn $decl_fn() -> $crate::EffectDecl {
            $crate::EffectDecl {
                type_name: stringify!($eff),
                description: concat!($($desc),*),
                prompt_card: crate::effect_defs::opt_prompt_card_or_none!($($pc)?),
                constructors: &[
                    $( crate::effect_defs::ctor_sig!(
                        { $eff, $tps, $ctor, [ $($ah),* ], $ret $(, errors $everr)? }
                    ) ),*
                ],
                type_defs: &[
                    $($td,)*
                    $( crate::effect_defs::error_decl_text!($errname $(, $evariant)*) )?
                ],
                extra_imports: crate::effect_defs::extra_imports_for!($eff),
                helpers: &[ $( crate::effect_defs::helper_text!($helper) ),* ],
                type_params: crate::effect_defs::ty_param_names!($tps),
                default_row_args: &[ $($dra),* ],
                helpers_row_polymorphic: crate::effect_defs::opt_bool_or_false!($($hrp)?),
            }
        }
    };
}
pub(crate) use effect_decl_projection;

/// Console effect — THE single definition.
///
/// Everything about Console derives from this table: `console_decl()` (via
/// [`effect_decl_projection!`], in `effect_decls.rs`) and `ConsoleReq` /
/// `DescribeEffect` / `EffectHandler` dispatch (via `effect_rust_projection!`,
/// in `tidepool-handlers/src/handlers/console.rs`). Only the handler METHOD
/// BODY (`ConsoleHandler::print`) is hand-written.
#[macro_export]
macro_rules! console_effect_def {
    ($project:path) => {
        $project! {
            effect Console,
            handler ConsoleHandler,
            req ConsoleReq,
            decl_fn console_decl,
            helpers_row_polymorphic true,
            description ["Print text output."],
            type_defs [],
            verbs [
                { ctor Print, method print,
                  args { msg: "Text" as String },
                  ret "()" },
            ],
            helpers [
                { name say, sig "forall effs. Member Console effs => Text -> Eff effs ()",
                  doc ["Emit a line of console output. Thin wrapper over the Print effect",
                       "so chains never need `send (Print …)`."],
                  body pointfree Print },
            ],
        }
    };
}

/// Time effect — single definition.
///
/// `UTCTime` and its helpers live in `Tidepool.Data.Time` (re-exported by
/// `Tidepool.Prelude`), so the generated Effects module needs no `type_defs`.
#[macro_export]
macro_rules! time_effect_def {
    ($project:path) => {
        $project! {
            effect Time,
            handler TimeHandler,
            req TimeReq,
            decl_fn time_decl,
            helpers_row_polymorphic true,
            description [
                "UTC wall-clock access (epoch milliseconds). ",
                "`getCurrentTime` returns an opaque `UTCTime` value. ",
                "`formatISO8601` renders it as ISO-8601 (e.g. \"2024-02-29T00:00:00Z\"). ",
                "`diffUTCTime a b` gives seconds between two times; `addUTCTime secs t` adds seconds. ",
                "`epochMillis t` exposes the raw epoch-millisecond integer.",
            ],
            type_defs [],
            verbs [
                { ctor TimeNow, method time_now,
                  args { },
                  ret "Int" },
            ],
            helpers [
                { raw ["-- | Current UTC time as an opaque UTCTime (epoch-millisecond resolution).",
                       "getCurrentTime :: forall effs. Member Time effs => Eff effs UTCTime",
                       "getCurrentTime = UTCTime <$> send TimeNow"] },
            ],
        }
    };
}

/// Entropy effect — single definition.
///
/// ONE dumb verb, per the Mechanism Index's "code owns process state; models
/// answer bounded semantic questions": fresh OS entropy as a 64-bit seed. The
/// canonical `System.Random` vocabulary a model actually calls —
/// `mkStdGen`/`randomR`/`randoms`/`split` (pure), `newStdGen`/`randomRIO`
/// (seeded from this verb) — is ordinary Haskell in `Tidepool.Random`,
/// auto-imported whenever `Entropy` is in the row (always, since it is a
/// base effect) via `extra_imports_for!` — never a second effect wire. The
/// raw verb is marked `substrate`: a model calls `newStdGen`/`randomRIO`,
/// never `entropySeed` directly. Shape mirrors `Time` (nullary verb, `Int`
/// result).
#[macro_export]
macro_rules! entropy_effect_def {
    ($project:path) => {
        $project! {
            effect Entropy,
            handler EntropyHandler,
            req EntropyReq,
            decl_fn entropy_decl,
            helpers_row_polymorphic true,
            description [
                "Randomness. Seeded, deterministic generation is pure Haskell — no effect ",
                "needed: `mkStdGen :: Int -> StdGen`, `randomR :: (a, a) -> StdGen -> (a, ",
                "StdGen)`, `randoms :: StdGen -> [a]`, `split :: StdGen -> (StdGen, StdGen)` ",
                "(Int and Double instances; each `randomR` draw stays within the given ",
                "bounds, inclusive; the same seed always replays the same sequence). For ",
                "non-deterministic values seeded from OS entropy: `newStdGen :: M StdGen` and ",
                "`randomRIO :: (a, a) -> M a`, e.g. `n <- randomRIO (1 :: Int, 100)`.",
            ],
            type_defs [],
            verbs [
                { ctor EntropySeed, method entropy_seed,
                  args { },
                  ret "Int" },
            ],
            helpers [
                { raw substrate ["entropySeed :: forall effs. Member Entropy effs => Eff effs Int",
                       "entropySeed = send EntropySeed"] },
            ],
        }
    };
}

/// Meta effect — single definition (debug-path self-mirror).
///
/// NOTE: the hand-written decl aligned constructor names with padding
/// ("MetaLookupCon    :: …"); the projection renders single-space signatures,
/// a deliberate one-time normalization of the generated Effects module.
#[macro_export]
macro_rules! meta_effect_def {
    ($project:path) => {
        $project! {
            effect Meta,
            handler MetaHandler,
            req MetaReq,
            decl_fn meta_decl,
            helpers_row_polymorphic true,
            description [
                "Self-mirror for the runtime. Query constructors, primops, effects, diagnostics.",
            ],
            type_defs [],
            verbs [
                { ctor MetaConstructors, method meta_constructors,
                  args { },
                  ret "[(Text, Int)]" },
                { ctor MetaLookupCon, method meta_lookup_con,
                  args { name: "Text" as String },
                  ret "(Maybe (Int, Int))" },
                { ctor MetaPrimOps, method meta_primops,
                  args { },
                  ret "[Text]" },
                { ctor MetaEffects, method meta_effects,
                  args { },
                  ret "[Text]" },
                { ctor MetaDiagnostics, method meta_diagnostics,
                  args { },
                  ret "[Text]" },
                { ctor MetaVersion, method meta_version,
                  args { },
                  ret "Text" },
                { ctor MetaHelp, method meta_help,
                  args { },
                  ret "[Text]" },
            ],
            helpers [
                { name metaConstructors, sig "forall effs. Member Meta effs => Eff effs [(Text, Int)]",
                  doc ["Constructor table: (name, arity) pairs."],
                  body nullary MetaConstructors },
                { name metaLookupCon, sig "forall effs. Member Meta effs => Text -> Eff effs (Maybe (Int, Int))",
                  doc ["Look up a constructor by name: (tag, arity)."],
                  body pointfree MetaLookupCon },
                { name metaPrimOps, sig "forall effs. Member Meta effs => Eff effs [Text]",
                  doc ["Names of the JIT-implemented primops."],
                  body nullary MetaPrimOps },
                { name metaEffects, sig "forall effs. Member Meta effs => Eff effs [Text]",
                  doc ["Effect type names in the running stack."],
                  body nullary MetaEffects },
                { name metaDiagnostics, sig "forall effs. Member Meta effs => Eff effs [Text]",
                  doc ["Drain pending runtime diagnostics."],
                  body nullary MetaDiagnostics },
                { name metaVersion, sig "forall effs. Member Meta effs => Eff effs Text",
                  doc ["Server crate version."],
                  body nullary MetaVersion },
                { name metaHelp, sig "forall effs. Member Meta effs => Eff effs [Text]",
                  doc ["Helper-verb signatures of the running stack."],
                  body nullary MetaHelp },
            ],
        }
    };
}

/// Http effect — single definition.
// `crate::effect_glue::JsonArg` is resolved at the EXPANSION site
// (tidepool-handlers' `effect_rust_projection!`), which is the only consumer
// of the Rust arg types — `$crate` (= tidepool_mcp, no `effect_glue`) would
// not compile there.
#[allow(clippy::crate_in_macro_def)]
#[macro_export]
macro_rules! http_effect_def {
    ($project:path) => {
        $project! {
            effect Http,
            handler HttpHandler,
            req HttpReq,
            decl_fn http_decl,
            helpers_row_polymorphic true,
            description [
                "JSON I/O. Fetch JSON from HTTP endpoints (returns Value). ",
                "Parsing JSON Text is PURE — `eitherDecode` (aeson-style, ",
                "serde_json underneath), no effect involved.",
            ],
            type_defs [],
            // #335 typed-failure ADT. The HTTP status code is a FIELD on
            // `HttpStatus` (not folded into a message) so callers can dispatch
            // on it, e.g. `Left (HttpStatus 404 _)`.
            // STABLE home (`stable_errors true`, below) — see the same note on
            // `errors GitError` in `git_effect_def!` / `tidepool-mcp/src/fs_stable.rs`.
            errors HttpError [
                { ctor HttpInvalidUrl, fields { detail: "Text" as String },              doc "the URL is malformed or uses an unsupported scheme" },
                { ctor HttpRestricted, fields { detail: "Text" as String },              doc "the URL targets a sandboxed/internal address" },
                { ctor HttpNetwork,    fields { detail: "Text" as String },              doc "a network-level failure (connect/timeout/read)" },
                { ctor HttpStatus,     fields { code: "Int" as i64, body: "Text" as String }, doc "a non-2xx HTTP response" },
                { ctor HttpTooLarge,   fields { nodes: "Int" as i64 },                   doc "the JSON response exceeds the materialization cap (node count); narrow the query" },
            ],
            stable_errors true,
            verbs [
                { ctor HttpGet, method http_get,
                  args { url: "Text" as String },
                  ret "Value", errors HttpError },
                { ctor HttpPost, method http_post,
                  args { url: "Text" as String, body: "Value" as crate::effect_glue::JsonArg },
                  ret "Value", errors HttpError },
            ],
            helpers [
                { name httpGet, sig "forall effs. Member Http effs => Text -> Eff effs (Either HttpError Value)",
                  doc ["Fetch JSON from an HTTP endpoint. Failure is TYPED (#335): `Left",
                       "(HttpStatus code body)` on a non-2xx response, `Left (HttpNetwork _)`",
                       "on a network failure. Unwrap with `Right v <- httpGet url` or `>>= liftEither`."],
                  body pointfree HttpGet },
                { name httpPost, sig "forall effs. Member Http effs => Text -> Value -> Eff effs (Either HttpError Value)",
                  doc ["POST a JSON body; returns the response Value (see `httpGet` for the",
                       "failure shape)."],
                  body applied HttpPost(url, body) },
            ],
        }
    };
}

/// Git effect — single definition.
///
/// `Commit`/`StatusEntry`/`FileDelta` are defined in `Tidepool.Records` and
/// re-exported by `Tidepool.Prelude`, so no `type_defs` here. (The
/// hand-written decl aligned constructor signatures with padding; the
/// projection renders single-space signatures — same one-time normalization
/// as Meta.)
#[macro_export]
macro_rules! git_effect_def {
    ($project:path) => {
        $project! {
            effect Git,
            handler GitHandler,
            req GitReq,
            decl_fn git_decl,
            helpers_row_polymorphic true,
            description [
                "Read-only git repository queries. Returns typed records parsed Rust-side ",
                "from machine-format git output — no text-splitting needed. ",
                "`gitLog n` → last N commits newest-first; `gitStatus` → working-tree status; ",
                "`gitDiffStat rev` → per-file diff stats, WORKING TREE vs `rev` (NOT commit-vs-parent — ",
                "pass a range like \"sha~1..sha\" for that; a clean tree ⇒ `[]`, correct data, not an error); ",
                "`gitLogNumstat n` → last N commits EACH PAIRED WITH ITS OWN numstat deltas, one ",
                "subprocess for all N — the substrate for any git-history investigation, in place of ",
                "`gitLog` + a per-commit `mapM gitDiffStat`; `gitShow rev` → one commit. ",
                "Every list verb returns typed records: `Commit {sha,subject,author,date,files}`, ",
                "`StatusEntry {path,state}` (state = 2-char XY porcelain code), ",
                "`FileDelta {path,adds,dels,binary}`, `CommitDeltas {commit,deltas}`.",
            ],
            type_defs [],
            // #335 typed-failure ADT. `GitBadRevspec` covers an unknown/ambiguous
            // revspec (also a `gitShow` with zero matching commits); `GitFailed`
            // is the residual (git exited nonzero for another reason, or the git
            // binary itself couldn't be spawned — exit code -1 in that case).
            // Reuse the pre-schema `GitError` exported by
            // `Tidepool.Records.Stable`; do not declare a duplicate in Core.
            errors GitError [
                { ctor GitBadRevspec, fields { detail: "Text" as String },                     doc "unknown or ambiguous revspec" },
                { ctor GitFailed,     fields { code: "Int" as i64, detail: "Text" as String },  doc "git exited nonzero (or could not be spawned)" },
            ],
            stable_errors true,
            verbs [
                { ctor GitLog, method git_log,
                  args { n: "Int" as i64 },
                  ret "[Commit]", errors GitError },
                { ctor GitStatus, method git_status,
                  args { },
                  ret "[StatusEntry]", errors GitError },
                { ctor GitDiffStat, method git_diff_stat,
                  args { rev: "Text" as String },
                  ret "[FileDelta]", errors GitError },
                { ctor GitShow, method git_show,
                  args { rev: "Text" as String },
                  ret "Commit", errors GitError },
                { ctor GitLogNumstat, method git_log_numstat,
                  args { n: "Int" as i64 },
                  ret "[CommitDeltas]", errors GitError },
            ],
            helpers [
                { name gitLog, sig "forall effs. Member Git effs => Int -> Eff effs (Either GitError [Commit])",
                  doc ["Last N commits, newest-first. Each 'Commit' carries sha/subject/author/date/files."],
                  body pointfree GitLog },
                { name gitStatus, sig "forall effs. Member Git effs => Eff effs (Either GitError [StatusEntry])",
                  doc ["Working-tree status. Each 'StatusEntry' has path and 2-char XY state code",
                       "(e.g. \"M \", \"??\", \"A \")."],
                  body nullary GitStatus },
                { name gitDiffStat, sig "forall effs. Member Git effs => Text -> Eff effs (Either GitError [FileDelta])",
                  doc ["Per-file diff stats. THE ARGUMENT IS A REVSPEC COMPARED AGAINST THE WORKING",
                       "TREE, not \"commit vs its parent\" — `gitDiffStat sha` on a clean tree is",
                       "`Right []` (correct data, not an error). For \"what did this commit change\",",
                       "pass a RANGE: `gitDiffStat (sha <> \"~1..\" <> sha)` (also \"main..HEAD\",",
                       "\"HEAD~3..HEAD\"). For bulk per-commit history, use `gitLogNumstat` instead —",
                       "one subprocess for N commits, no per-commit mapM. 'FileDelta' carries",
                       "path/adds/dels/binary."],
                  body pointfree GitDiffStat },
                { name gitShow, sig "forall effs. Member Git effs => Text -> Eff effs (Either GitError Commit)",
                  doc ["Single commit by revspec. `Left (GitBadRevspec _)` on an unknown or",
                       "ambiguous revspec; unwrap with `Right c <- gitShow rev` or `>>= liftEither`."],
                  body pointfree GitShow },
                { name gitLogNumstat, sig "forall effs. Member Git effs => Int -> Eff effs (Either GitError [CommitDeltas])",
                  doc ["Last N commits, newest-first, EACH PAIRED WITH ITS OWN per-file numstat",
                       "deltas — one subprocess for all N, in place of `gitLog` followed by a",
                       "per-commit `mapM gitDiffStat`. 'CommitDeltas' carries {commit, deltas};",
                       "a merge or otherwise-empty commit has `deltas = []`. A renamed file records",
                       "only its NEW path."],
                  body pointfree GitLogNumstat },
            ],
        }
    };
}

/// Ask effect — single definition (decl-side only).
///
/// Ask has no `tidepool-handlers` handler: its dispatchers are server
/// machinery (`tidepool-mcp/src/ask.rs`, `tidepool-repl/src/ask.rs`), so only
/// [`effect_decl_projection!`] consumes this definition. The `handler`/`req`/
/// `method` slots name types that are never generated.
#[macro_export]
macro_rules! ask_effect_def {
    ($project:path) => {
        $project! {
            effect Ask,
            handler AskHandler,
            req AskReq,
            decl_fn ask_decl,
            helpers_row_polymorphic true,
            description [
                "Suspend execution and ask the calling agent a STRUCTURED question. ",
                "`ask schema prompt` carries the schema as JSON Schema in the suspension; ",
                "the resume reply is validated against it server-side before re-entering ",
                "the computation (invalid replies do NOT consume the continuation). ",
                "Extract fields from the returned Value with optics, e.g. ",
                "`v ^? key \"path\" . _String`.",
            ],
            // The Schema vocabulary (`data Schema`, `schemaToValue`, `isOpt`,
            // `innerSchema`) AND `ask` itself now live in the stdlib —
            // `Tidepool.Form.Schema`, auto-imported via
            // `extra_imports_for!(Ask)` — because they are ordinary pure/
            // composed Haskell, not decl-shaped verb surface (issue #24's
            // first act; see `tidepool-protocol/src/effects/ask.rs`'s module
            // doc). Only the THIN raw verb wrapper (`askRaw`, bare `send
            // (Ctor …)`) stays spliced into the generated module below — the
            // generated module cannot import authored library code (see
            // `extra_imports_for!`'s own doc), so anything referencing
            // `Schema`/`schemaToValue` must live OUTSIDE it, same split as
            // `AskUser`'s `askUserRaw` (thin, generated) vs. `Tidepool.Form`'s
            // `askUser` (composed, authored) and `Entropy`'s `entropySeed`
            // (thin, generated) vs. `Tidepool.Random`'s `newStdGen` (composed,
            // authored). `Ask` is always present in every ordinary stack, so
            // the `Tidepool.Form.Schema` import reaches `.tidepool/lib`
            // modules and Llm-less stacks exactly as the old inline
            // `type_defs` did. `Tidepool.Form.Schema` does NOT declare `llm`
            // — that lives in its own `Tidepool.Llm` module, gated by `Llm`'s
            // own `extra_imports_for!` arm, independent of this one (see that
            // module's doc for why the split is load-bearing, not cosmetic).
            type_defs [],
            verbs [
                { ctor AskWith, method ask_with,
                  args { prompt: "Text" as String, payload: "Value" as tidepool_eval::value::Value },
                  ret "Value" },
            ],
            helpers [
                { raw substrate ["askRaw :: forall effs. Member Ask effs => Text -> Value -> Eff effs Value",
                       "askRaw prompt payload = send (AskWith prompt payload)"] },
            ],
        }
    };
}

// AskUser effect: MIGRATED to the `tidepool-protocol` schema (#20 steps 2-3).
// `askuser_decl()` now comes from `tidepool-mcp/src/generated/ask_user.rs`;
// see `tidepool-protocol/src/effects/ask_user.rs` for the single-source
// definition.

// ReadState effect: MIGRATED to the `tidepool-protocol` schema (#20 steps
// 2-3). `readstate_decl()` now comes from
// `tidepool-mcp/src/generated/read_state.rs`; see
// `tidepool-protocol/src/effects/read_state.rs` for the single-source
// definition.

// RunLLMTurn effect: MIGRATED to the `tidepool-protocol` schema (#20 steps
// 2-3, helperbody-flip). `runllmturn_decl()` now comes from
// `tidepool-mcp/src/generated/run_l_l_m_turn.rs`; see
// `tidepool-protocol/src/effects/run_llm_turn.rs` for the single-source
// definition.

// Finalize effect: MIGRATED to the `tidepool-protocol` schema (#20 steps 2-3,
// helperbody-flip). `finalize_decl()` now comes from
// `tidepool-mcp/src/generated/finalize.rs`; see
// `tidepool-protocol/src/effects/finalize.rs` for the single-source definition.

// Fork effect: MIGRATED to the `tidepool-protocol` schema (#20 steps 2-3,
// helperbody-flip). `fork_decl()` now comes from
// `tidepool-mcp/src/generated/fork.rs`; see
// `tidepool-protocol/src/effects/fork.rs` for the single-source definition.

/// Llm effect — single definition.
// See `http_effect_def!` on why `crate::` (not `$crate`) is correct here.
#[allow(clippy::crate_in_macro_def)]
#[macro_export]
macro_rules! llm_effect_def {
    ($project:path) => {
        $project! {
            effect Llm,
            handler LlmHandler,
            req LlmReq,
            decl_fn llm_decl,
            helpers_row_polymorphic true,
            description [
                "Call an LLM for classification, extraction, or judgment. ",
                "`llm schema prompt` returns a Value validated against the schema ",
                "(structured output, no markdown fences). Extract with optics, e.g. ",
                "`v ^? key \"category\" . _String`.",
            ],
            type_defs [],
            // #335 typed-failure ADT. FULLY TOTAL: budget exhaustion is now DATA
            // (`LlmBudget`), not an abort — nothing in the Llm path kills the eval.
            // STABLE home (`stable_errors true`, below) — see the same note on
            // `errors GitError` above / `tidepool-mcp/src/fs_stable.rs`.
            errors LlmError [
                { ctor LlmApi,     fields { detail: "Text" as String }, doc "API/network call failure" },
                { ctor LlmRefusal, fields { detail: "Text" as String }, doc "the model declined to answer" },
                { ctor LlmBudget,  fields { },                          doc "the per-eval call budget is exhausted" },
            ],
            stable_errors true,
            verbs [
                { ctor LlmStructured, method llm_structured,
                  args { prompt: "Text" as String, schema: "Value" as crate::effect_glue::JsonArg },
                  ret "Value", errors LlmError },
            ],
            helpers [
                // The composed `llm` (which calls `schemaToValue`) lives in
                // its own `Tidepool.Llm` module now — the generated module
                // cannot import authored library code, so only the thin raw
                // verb wrapper stays here, same split as `Ask`'s
                // `askRaw`/`ask` (see `ask_effect_def!`'s doc for the full
                // rationale, and `Tidepool.Llm`'s module doc for why `llm`
                // is NOT in `Tidepool.Form.Schema` alongside `ask`).
                { raw substrate ["llmRaw :: forall effs. Member Llm effs => Text -> Value -> Eff effs (Either LlmError Value)",
                       "llmRaw prompt payload = send (LlmStructured prompt payload)"] },
                // Pure tally utilities (no LLM/Ask): build a frequency list while
                // preserving first-seen order. Kept for .tidepool/lib verbs.
                { raw ["findTally :: Eq a => a -> [(a, Int)] -> Maybe [(a, Int)]",
                       "findTally _ [] = Nothing",
                       "findTally x ((k, n):rest) = if x == k then Just ((k, n + 1) : rest) else case findTally x rest of { Just rest' -> Just ((k, n) : rest'); Nothing -> Nothing }"] },
                { raw ["tallyList :: Eq a => [a] -> [(a, Int)]",
                       "tallyList = foldl' (\\acc x -> case findTally x acc of { Just acc' -> acc'; Nothing -> acc ++ [(x, 1)] }) []"] },
            ],
        }
    };
}

/// KV effect — single definition.
///
/// NOT `errors`-tagged under #335: KV's only failures are JSON-file IO faults
/// (disk/permission) — infra faults nothing dispatches on, so they stay
/// aborts. Contrast Llm, which IS fully total, because budget exhaustion is
/// dispatchable DATA (`LlmBudget`). The test: does a caller ever branch on the
/// failure? Yes → typed (`Left`); no → abort. KV is a `no`.
#[macro_export]
macro_rules! kv_effect_def {
    ($project:path) => {
        $project! {
            effect KV,
            handler KvHandler,
            req KvReq,
            decl_fn kv_decl,
            helpers_row_polymorphic true,
            description [
                "Persistent key-value store. State survives across calls within one server session. ",
                "Key convention: use slash-delimited namespaces (e.g. \"agent-42/foo\") to avoid ",
                "cross-agent collision. kvClear/kvKeysP operate on prefix boundaries. For a typed ",
                "round-trip instead of hand-unwrapping the stored `Value`, use `kvGetAs @T key` ",
                "(`Tidepool.Kv`) — decodes via `FromJSON`, `Right (Just x)`/`Right Nothing`/a legible ",
                "`Left` on a decode mismatch, never a crash.",
            ],
            type_defs [],
            verbs [
                { ctor KvGet, method kv_get,
                  args { key: "Text" as String },
                  ret "(Maybe Value)" },
                { ctor KvSet, method kv_set,
                  args { key: "Text" as String, val: "Value" as tidepool_eval::value::Value },
                  ret "()" },
                { ctor KvDelete, method kv_delete,
                  args { key: "Text" as String },
                  ret "()" },
                { ctor KvKeys, method kv_keys,
                  args { },
                  ret "[Text]" },
                // Delete all keys with the given prefix; return count deleted.
                // Pass "" to clear the ENTIRE store (dangerous — see kvClear docstring).
                { ctor KvClear, method kv_clear,
                  args { prefix: "Text" as String },
                  ret "Int" },
                // List keys matching a prefix, sorted.
                { ctor KvKeysP, method kv_keys_p,
                  args { prefix: "Text" as String },
                  ret "[Text]" },
                // Summary: {count, sample, file_size_bytes} — inspect the junk-drawer.
                { ctor KvInfo, method kv_info,
                  args { },
                  ret "Value" },
                // Cross-process compare-and-swap: set key=new only if its
                // current value equals `expected` (Nothing = require absent).
                // `Left actual` on a mismatch — the lost-update-free primitive.
                { ctor KvCas, method kv_cas,
                  args { key: "Text" as String,
                         expected: "Maybe Value" as Option<tidepool_eval::value::Value>,
                         new: "Value" as tidepool_eval::value::Value },
                  ret "(Either Value ())" },
            ],
            helpers [
                { name kvGet, sig "forall effs. Member KV effs => Text -> Eff effs (Maybe Value)",
                  doc ["Look up a key; Nothing when absent."],
                  body pointfree KvGet },
                { name kvSet, sig "forall effs. Member KV effs => Text -> Value -> Eff effs ()",
                  doc ["Persist a JSON value under a key."],
                  body applied KvSet(k, v) },
                { name kvDel, sig "forall effs. Member KV effs => Text -> Eff effs ()",
                  doc ["Delete a key (no-op when absent)."],
                  body pointfree KvDelete },
                { name kvClear, sig "forall effs. Member KV effs => Text -> Eff effs Int",
                  doc ["Delete all keys whose name starts with @prefix@; return the count deleted.",
                       "Pass \"\" (empty string) to clear the ENTIRE store — this erases ALL",
                       "persisted KV data for this server session, so use with caution.",
                       "Recommended pattern: namespace keys as \"ns/key\" and clear with \"ns/\".",
                       "NOTE: per-session automatic scoping is a deferred design decision (#327);",
                       "callers manage namespaces manually via this prefix argument."],
                  body pointfree KvClear },
                { name kvKeysP, sig "forall effs. Member KV effs => Text -> Eff effs [Text]",
                  doc ["All keys whose name starts with @prefix@, returned sorted.",
                       "E.g. @kvKeysP \"agent/\"@ returns @[\"agent/bar\", \"agent/foo\", ...]@.",
                       "Pass \"\" to list ALL keys, sorted."],
                  body pointfree KvKeysP },
                { name kvInfo, sig "forall effs. Member KV effs => Eff effs Value",
                  doc ["Summary of KV store state as a JSON Value:",
                       "@{count :: Int, sample :: [Text], file_size_bytes :: Int}@.",
                       "Use to inspect junk-drawer accumulation without listing all keys.",
                       "Extract fields with optics: @i <- kvInfo; i ^? key \"count\" . _Int@"],
                  body nullary KvInfo },
                { name kvCas, sig "forall effs. Member KV effs => Text -> Maybe Value -> Value -> Eff effs (Either Value ())",
                  doc ["Atomic compare-and-swap: set @key@ to @new@ only if its current",
                       "value equals @expected@ (Nothing = require the key ABSENT).",
                       "@Right ()@ on success; @Left actual@ (the current value) on a",
                       "mismatch, with nothing written. Cross-process safe (the store",
                       "file is flocked), so it is the lost-update-free primitive that",
                       "kvModify\\/kvIncr\\/kvAppend retry over — prefer those for the",
                       "common read-modify-write; reach for kvCas directly for a custom",
                       "conflict policy."],
                  body applied KvCas(k, e, n) },
            ],
        }
    };
}

/// Read-only filesystem effect.
///
/// The helper surface is transported VERBATIM as one `raw` block per helper
/// (Fs's verbs are mostly multi-line Haskell bodies, not thin send-wrappers,
/// so the structured `body` forms don't apply — the raw escape hatch is the
/// designed answer here). Exact byte transport: the generated decl is
/// identical to the hand-written baseline.
#[macro_export]
macro_rules! fs_read_effect_def {
    ($project:path) => {
        $project! {
            effect FsRead,
            handler FsReadHandler,
            req FsReadReq,
            decl_fn fs_read_decl,
            // Every helper below is Member-polymorphic.
            helpers_row_polymorphic true,
            description ["Read files and filesystem metadata (sandboxed to server working directory)."],
            // `FileRead` and `FsError` are pre-schema domain types already
            // exported by `Tidepool.Records.Stable`; `stable_errors true`
            // below makes generated Core reuse them rather than declare a
            // second nominal copy. See `fs_stable.rs` for their source.
            type_defs [],
            // #335 typed-failure ADT. Coarse: `FsNotFound`/`FsNotUtf8` carry the
            // path so callers can dispatch (`Left (FsNotFound _)`); the rest carry
            // a message. `FsSandbox` covers sandbox escapes and the glob boundary
            // guards (empty/absolute pattern); `FsBadRegex` is a grep compile
            // failure; `FsIo` is the residual.
            errors FsError [
                { ctor FsNotFound, fields { path: "Text" as String },   doc "path does not exist" },
                { ctor FsNotUtf8,  fields { path: "Text" as String },   doc "file is not valid UTF-8" },
                { ctor FsSandbox,  fields { detail: "Text" as String }, doc "path escapes the sandbox, or the glob pattern is not allowed" },
                { ctor FsBadRegex, fields { detail: "Text" as String }, doc "grep regex failed to compile" },
                { ctor FsIo,       fields { detail: "Text" as String }, doc "other I/O failure" },
                // camino-utf8-paths: the OS path itself (not its content) is not
                // valid UTF-8, so it cannot be represented as Haskell `Text` at
                // all — `path` carries the lossy (replacement-char) rendering for
                // diagnostics ONLY, never as something to feed back into another
                // Fs verb.
                { ctor FsNonUtf8Path, fields { path: "Text" as String }, doc "path is not valid UTF-8 (lossy rendering shown for diagnostics)" },
            ],
            stable_errors true,
            verbs [
                { ctor FsRead, method fs_read,
                  args { path: "Text" as String },
                  ret "Text", errors FsError },
                // A Left is decided eagerly, so the success list materializes
                // whole (no lazy stream under an Either — see #335 boundary).
                { ctor FsListDir, method fs_list_dir,
                  args { path: "Text" as String },
                  ret "[Text]", errors FsError },
                { ctor FsGlob, method fs_glob,
                  args { pattern: "Text" as String },
                  ret "[Text]", errors FsError },
                { ctor FsGrep, method fs_grep,
                  args { pattern: "Text" as String, file_glob: "Text" as String },
                  ret "[Hit]", errors FsError },
                { ctor FsExists, method fs_exists,
                  args { path: "Text" as String },
                  ret "Bool", errors FsError },
                // Record-native: `Just FileMeta{..}` on success, `Nothing` for a
                // missing/unreadable path. Stays total-by-Maybe (no Either):
                // absence IS the answer here.
                { ctor FsMetadata, method fs_metadata,
                  args { path: "Text" as String },
                  ret "(Maybe FileMeta)" },
                // Per-file failure-isolating glob read (#328): each match is a
                // `FileRead {path, contents}` — a mixed glob (text + binary)
                // survives, the binary just comes back as `contents = Left err`.
                // The per-item Either rides inside the streamed list of records
                // (not a verb-level Either), so this verb stays lazy.
                { ctor FsReadGlob, method fs_read_glob,
                  args { pattern: "Text" as String },
                  ret "[FileRead]" },
                // Content-hash compare-and-swap surface (#330). FsHash = current
                // blake3 digest (Nothing = absent); FsWriteCas writes only if the
                // current hash equals the expected one (Nothing = require absent),
                // else returns `Left actual` with the ACTUAL hash (Nothing = absent).
                { ctor FsHash, method fs_hash,
                  args { path: "Text" as String },
                  ret "(Maybe Text)", errors FsError },
            ],
            helpers [
                { raw ["-- | Read a file. Failure is TYPED (#335): `Left (FsNotFound p)` / `Left\n-- (FsNotUtf8 p)` / `Left (FsIo _)`. The natural spelling unwraps-or-aborts with\n-- a failable bind: `Right src <- readFile path` (or `readFile path >>= liftEither`).\nreadFile :: forall effs. Member FsRead effs => FilePath -> Eff effs (Either FsError Text)\nreadFile = send . FsRead"] },
                { raw ["-- | List a directory. `Left (FsNotFound _)` when absent; unwrap with `liftEither`.\nlistDirectory :: forall effs. Member FsRead effs => FilePath -> Eff effs (Either FsError [FilePath])\nlistDirectory = send . FsListDir"] },
                { raw ["-- | TOTAL existence predicate (System.Directory semantics): False for a\n-- missing path, a directory, or a path outside the sandbox — never throws.\ndoesFileExist :: forall effs. Member FsRead effs => FilePath -> Eff effs Bool\ndoesFileExist p = send (FsMetadata p) <&> maybe False (\\m -> m.isFile)"] },
                { raw ["-- | TOTAL existence predicate: False for missing/non-dir/out-of-sandbox.\ndoesDirectoryExist :: forall effs. Member FsRead effs => FilePath -> Eff effs Bool\ndoesDirectoryExist p = send (FsMetadata p) <&> maybe False (\\m -> m.isDir)"] },
                { raw ["-- | File size in bytes, or `Nothing` if the path is missing.\ngetFileSize :: forall effs. Member FsRead effs => FilePath -> Eff effs (Maybe Int)\ngetFileSize p = send (FsMetadata p) <&> fmap (\\m -> m.size)"] },
                { raw ["-- | File metadata as a `FileMeta` record {size, isFile, isDir}, or `Nothing`\n-- if the path is missing/unreadable (use record-dot: `m.size`, `m.isDir`).\nfsMeta :: forall effs. Member FsRead effs => FilePath -> Eff effs (Maybe FileMeta)\nfsMeta = send . FsMetadata"] },
                { raw ["-- | Expand a glob to matching file paths. `Left (FsSandbox _)` on an empty\n-- or absolute pattern, `Left (FsNotFound _)` on a missing search root; unwrap\n-- with `Right ps <- glob pat` or `glob pat >>= liftEither`.\nglob :: forall effs. Member FsRead effs => FilePath -> Eff effs (Either FsError [FilePath])\nglob = send . FsGlob"] },
                { raw ["-- | Regex-search files matching a path glob. ARG ORDER: regex FIRST, glob\n-- SECOND — a path glob like \"*.rs\" goes in arg 2, not arg 1. Returns [Hit]\n-- {path, line, text} (the shared Hit shape, so it composes with\n-- hitsByFile/refs). Failure is typed: `Left (FsBadRegex _)` on a bad regex.\n-- NB regex metachars are double-escaped here (JSON x Haskell), so a literal dot\n-- needs four backslashes; the FsBadRegex detail shows the exact form.\ngrepGlob :: forall effs. Member FsRead effs => Text -> FilePath -> Eff effs (Either FsError [Hit])\ngrepGlob pat g = send (FsGrep pat g)"] },
                { raw ["-- | Read every file matching a glob with PER-FILE failure isolation: one\n-- `FileRead {path, contents}` per match — `contents` is `Right text` on a clean\n-- UTF-8 read, `Left err` on a per-file failure (binary / non-UTF-8, permission).\n-- One bad file (e.g. a binary swept up by a wide glob) does NOT fail the whole\n-- batch. An empty glob is rejected loudly, but a glob matching NOTHING yields\n-- `[]` silently — check `null rs` when absence itself is the signal. Recover\n-- the readable files with\n-- `[r.path | r <- rs, isRight r.contents]`, or split all outcomes with\n-- `partitionEithers (map (.contents) rs)`.\nreadGlob :: forall effs. Member FsRead effs => Text -> Eff effs [FileRead]\nreadGlob = send . FsReadGlob"] },
                { raw ["-- | Dry-run `update`: returns an `UpdateOutcome` (the review diff, or the\n-- reason it can't apply), writes NOTHING. Never errors — the conflict comes\n-- back as data so you can branch before committing.\nplanUpdate :: forall effs. Member FsRead effs => FilePath -> Text -> Text -> Eff effs UpdateOutcome\nplanUpdate path old new = do\n  er <- readFile path\n  case er of\n    Left e -> pure (UpdateRejected (\"file not found: \" <> show e) Nothing)\n    Right src ->\n      let n = if T.null old then 0 else length (T.splitOn old src) - 1\n      in if T.null old then pure (UpdateRejected \"'old' must be non-empty\" Nothing)\n         else if n == 0 then pure (UpdateRejected \"not found\" Nothing)\n         else if n > 1 then pure (UpdateRejected \"ambiguous\" (Just n))\n         else case Patch.genPatch path src (replace old new src) of\n                Left _ -> pure UpdateNoChange\n                Right fp -> pure (UpdateDiff (Patch.renderPatch [fp]))"] },
                { raw ["-- | Blake3 content hash (hex) of a file, or Nothing if it does not exist.\n-- The compare-and-swap token for writeCheckedIf: read it, compute your new\n-- content, then write back only if the file still hashes the same.\nfileHash :: forall effs. Member FsRead effs => FilePath -> Eff effs (Maybe Text)\nfileHash p = send (FsHash p) >>= liftEither"] },
            ],
        }
    };
}

/// Filesystem mutation effect. The read and write handlers share one backend;
/// this separate algebra is the static boundary used by read-only actors.
#[macro_export]
macro_rules! fs_write_effect_def {
    ($project:path) => {
        $project! {
            effect FsWrite,
            handler FsWriteHandler,
            req FsWriteReq,
            decl_fn fs_write_decl,
            helpers_row_polymorphic true,
            description ["Mutate files (sandboxed to server working directory)."],
            type_defs [],
            errors FsError [
                { ctor FsNotFound, fields { path: "Text" as String },   doc "path does not exist" },
                { ctor FsNotUtf8,  fields { path: "Text" as String },   doc "file is not valid UTF-8" },
                { ctor FsSandbox,  fields { detail: "Text" as String }, doc "path escapes the sandbox, or the glob pattern is not allowed" },
                { ctor FsBadRegex, fields { detail: "Text" as String }, doc "grep regex failed to compile" },
                { ctor FsIo,       fields { detail: "Text" as String }, doc "other I/O failure" },
                { ctor FsNonUtf8Path, fields { path: "Text" as String }, doc "path is not valid UTF-8 (lossy rendering shown for diagnostics)" },
            ],
            stable_errors true,
            verbs [
                { ctor FsWrite, method fs_write,
                  args { path: "Text" as String, content: "Text" as String },
                  ret "()", errors FsError },
                { ctor FsWriteCas, method fs_write_cas,
                  args { path: "Text" as String, expected: "Maybe Text" as Option<String>, content: "Text" as String },
                  ret "(Either (Maybe Text) ())" },
            ],
            helpers [
                { raw ["-- | Write a file (mkdir -p on the parent). `Left (FsSandbox _)` on a path\n-- escape, `Left (FsIo _)` on write failure; unwrap with `liftEither`.\nwriteFile :: forall effs. Member FsWrite effs => FilePath -> Text -> Eff effs (Either FsError ())\nwriteFile f c = send (FsWrite f c)"] },
                { raw ["-- | Append to a file (reads then writes). Failure is TYPED (#335): a read\n-- or write failure comes back as `Left (FsError)` DATA, nothing partially\n-- applied; unwrap with `Right () <- appendFile path t` or `>>= liftEither`.\nappendFile :: forall effs. Members '[FsRead, FsWrite] effs => FilePath -> Text -> Eff effs (Either FsError ())\nappendFile p t = do\n  er <- readFile p\n  case er of\n    Left e -> pure (Left e)\n    Right old -> writeFile p (old <> t)"] },
                { raw ["-- | Exact str-replace, EXACTLY-ONCE. Reports the outcome as an\n-- `UpdateOneOutcome` DATA value (never throws, mirrors `InsertAfterOutcome`):\n-- empty `old`, a missing file, `old` not found, or `old` matching 2+ places\n-- is `UpdateOneRejected` (nothing written); otherwise `UpdateOneApplied`.\n-- Pass enough surrounding text that `old` is unique. Use planUpdate to review\n-- the diff first; the full editing surface is in tidepool://edits.\nupdate :: forall effs. Members '[FsRead, FsWrite] effs => FilePath -> Text -> Text -> Eff effs UpdateOneOutcome\nupdate path old new\n  | T.null old = pure (UpdateOneRejected \"'old' must be non-empty\" Nothing)\n  | otherwise = do\n      er <- readFile path\n      case er of\n        Left e -> pure (UpdateOneRejected (\"file not found: \" <> show e) Nothing)\n        Right src ->\n          case length (T.splitOn old src) - 1 of\n            0 -> pure (UpdateOneRejected (\"'old' not found in \" <> path) Nothing)\n            1 -> writeFile path (replace old new src) >>= liftEither >> pure UpdateOneApplied\n            n -> pure (UpdateOneRejected (\"'old' matches \" <> show n <> \" places in \" <> path <> \" (add surrounding context to disambiguate)\") (Just n))"] },
                { raw ["-- | Replace EVERY occurrence of `old` with `new`. Reports the outcome as an\n-- `UpdateAllOutcome` DATA value (never throws): empty `old`, a missing file,\n-- or zero matches is `UpdateAllRejected` (nothing written); otherwise\n-- `UpdateAllApplied` carries the replacement count.\nupdateAll :: forall effs. Members '[FsRead, FsWrite] effs => FilePath -> Text -> Text -> Eff effs UpdateAllOutcome\nupdateAll path old new\n  | T.null old = pure (UpdateAllRejected \"'old' must be non-empty\")\n  | otherwise = do\n      er <- readFile path\n      case er of\n        Left e -> pure (UpdateAllRejected (\"file not found: \" <> show e))\n        Right src ->\n          let n = length (T.splitOn old src) - 1\n          in if n == 0\n               then pure (UpdateAllRejected (\"'old' not found in \" <> path))\n               else writeFile path (replace old new src) >>= liftEither >> pure (UpdateAllApplied n)"] },
                { raw ["-- | `update` from the `input` JSON parameter: {file, old, new} (for big/quote-heavy\n-- fragments). Reports the outcome as an `UpdateOneOutcome` DATA value (never\n-- throws, same contract as `update`): a malformed payload (missing or\n-- non-string file/old/new key) is `UpdateOneRejected` — one bad item never\n-- aborts a batch.\nupdateJ :: forall effs. Members '[FsRead, FsWrite] effs => Value -> Eff effs UpdateOneOutcome\nupdateJ v = case (v ^? key \"file\" . _String, v ^? key \"old\" . _String, v ^? key \"new\" . _String) of\n  (Just f, Just o, Just n) -> update f o n\n  _ -> pure (UpdateOneRejected \"updateJ: need {file, old, new} strings in input\" Nothing)"] },
                { raw ["-- | Insert a block after the unique line containing `anchor`. Reports the\n-- outcome as an `InsertAfterOutcome` DATA value (never throws): a missing\n-- file, or an anchor matching zero or 2+ lines, is `InsertAfterRejected`\n-- (nothing written); otherwise `InsertAfterApplied`.\ninsertAfter :: forall effs. Members '[FsRead, FsWrite] effs => FilePath -> Text -> Text -> Eff effs InsertAfterOutcome\ninsertAfter path anchor block = do\n  er <- readFile path\n  case er of\n    Left e -> pure (InsertAfterRejected (\"file not found: \" <> show e) Nothing)\n    Right src ->\n      let ls = lines src\n          n = length (filter (isInfixOf anchor) ls)\n      in case n of\n           1 -> writeFile path (unlines (concatMap (\\l -> if anchor `isInfixOf` l then [l, block] else [l]) ls))\n                  >>= liftEither >> pure InsertAfterApplied\n           _ -> pure (InsertAfterRejected (\"anchor matched \" <> show n <> \" lines in \" <> path) (Just n))"] },
                { raw ["-- | Compute-check-commit: write only if every named check holds; failures\n-- come back as a `WriteOutcome` (nothing written on failure).\nwriteChecked :: forall effs. Member FsWrite effs => FilePath -> [(Text, Bool)] -> Text -> Eff effs WriteOutcome\nwriteChecked path checks content = do\n  let failed = [name | (name, ok) <- checks, not ok]\n  if null failed\n    then writeFile path content >>= liftEither >> pure (Written path (length checks))\n    else pure (WriteBlocked path failed)"] },
                { raw ["-- | Content-hash compare-and-swap write (#330). Writes CONTENT only if the\n-- file's current blake3 hash equals EXPECTED (Nothing = expect the file ABSENT,\n-- i.e. create-only). The compare-and-write is atomic within the handler, closing\n-- the lost-update race between parallel agents. Returns a WriteOutcome: 'Written'\n-- on success, or 'WriteConflict' (carrying expected vs actual hash) if the\n-- precondition failed — conflicts come back as DATA, nothing is written. Get\n-- EXPECTED from fileHash; on a conflict re-read, recompute, and retry.\nwriteCheckedIf :: forall effs. Member FsWrite effs => Maybe Text -> FilePath -> Text -> Eff effs WriteOutcome\nwriteCheckedIf expected path content = do\n  r <- send (FsWriteCas path expected content)\n  pure $ case r of\n    Right () -> Written path 1\n    Left actual -> WriteConflict path expected actual"] },
            ],
        }
    };
}

/// Subagent effect — single definition (the one-cycle coupled spawn).
/// PROVISIONAL: every shape here may be renamed when root freezes the public
/// surface (in particular, the GADT is `Subagent` rather than `Agent` because
/// `Tidepool.Agent` is the harness answerer's module and the rename is
/// root's call).
///
/// **One verb.** `spawnAgent` is the whole lane-1 authored surface: ONE call
/// atomically resolves a workspace (new managed worktree, or an existing
/// unbound one), binds it, dispatches one task to the backend, runs ONE work
/// cycle, and returns a typed outcome + receipt — or ONE case-matchable
/// `SpawnError` naming the saga stage that failed, with the rollback already
/// done (no Active binding left behind; a created worktree is RETAINED and
/// rebindable, per retain-first). `createWorktree` deliberately remains
/// non-public vocabulary for agent work (addendum decision 4); this effect
/// does not re-export it.
///
/// **Row requirement.** These types reference `WorktreeSpec` /
/// `WorktreeHandle` / `WorktreeError` from the Worktree contract's
/// type_defs, so a row containing Subagent must also contain Worktree.
///
/// **Typed-result decoding is Haskell-side.** The verb returns the terminal
/// payload as a `CyclePayload`; decoding `PayloadStructured` against the
/// caller's result type is `Tidepool.Agent.Spawn`'s job — ordinary
/// `FromJSON`, whose named-field shape the derived `outputSchema`
/// (`Tidepool.Aeson.Schema`) describes — and a decode failure is a typed
/// error, never a success.
// See `http_effect_def!` on why `crate::` (not `$crate`) is correct here:
// `crate::handlers::worktree::WorktreeError` / `crate::effect_glue::JsonArg`
// resolve at the EXPANSION site (tidepool-handlers), the only consumer of the
// Rust arg types.
#[allow(clippy::crate_in_macro_def)]
#[macro_export]
macro_rules! subagent_effect_def {
    ($project:path) => {
        $project! {
            effect Subagent,
            handler SubagentHandler,
            req SubagentReq,
            decl_fn subagent_decl,
            helpers_row_polymorphic true,
            description [
                "Typed headless subagents: one-cycle coupled spawn. ",
                "`spawnAgentRaw spec schema` is ONE atomic call: it allocates or takes a ",
                "managed worktree, binds it to a fresh agent, dispatches the task to the ",
                "coding backend, runs ONE work cycle in that worktree, and returns a ",
                "`SpawnOutcome` (the WorkerRun pair, the terminal payload, and a receipt ",
                "naming the worktree, binding, backend thread, and EXACT resolved model) ",
                "— or a case-matchable `SpawnError` naming the stage that failed, ",
                "with the rollback already done: no binding is left active, and a created ",
                "worktree is retained and rebindable, never deleted. Build the spec with ",
                "`spawnSpec` (new worktree) or `spawnSpecIn` (existing unbound worktree). ",
                "`agentBeginRaw`/`agentResumeRaw` are the same call sequence driven one ",
                "STOP at a time, for an agent that holds dynamic tools: begin returns either a ",
                "parked `StepToolCall` (the child called one of your tools; its turn is ",
                "stopped until you answer) or a `StepDone`, and resume answers the parked ",
                "call and drives on. Prefer the typed `spawnAgentWithTools`, which runs ",
                "that loop against your own Haskell tool handlers.\n",
                "TYPED SURFACE (`import Tidepool.Agent.Spawn` — not auto-imported): ",
                "`spawnAgent @r spec` derives the worker's `outputSchema` from `r`'s own ",
                "structure and decodes the terminal payload into an `r`, so the type IS the ",
                "schema. `r` must be a SINGLE-CONSTRUCTOR RECORD (a sum renders `oneOf` at the ",
                "schema root and the turn is refused whole) — model an alternative as a field:\n",
                "  data WorkerResult = WorkerResult { summary :: Text, blocked :: Maybe Text } deriving (Generic, FromJSON, JsonSchema)\n",
                "  Right (outcome, r) <- spawnAgent @WorkerResult (spawnSpec wspec \"porter\" \"port the handler\")\n",
                "A payload that does not decode is `SpawnResultMalformed`, never a success with ",
                "a defaulted field. `spawnAgentWithTools @tools @r rounds tools spec` is the ",
                "same call for a child that may CALL BACK into your own Haskell handlers.\n",
                "CONCURRENT: `spawnAsync @r spec` is the same call detached — it returns an ",
                "abstract `AgentHandle r` as soon as the cycle is admitted, `awaitAgent h` ",
                "blocks for that one cycle's `(outcome, r)`, and `cancelAgent h` reaps it ",
                "(total: cancelling a finished or unknown handle is a no-op, and settles the ",
                "binding without deleting anything). Run N children by spawning N handles and ",
                "awaiting them in whatever order suits you — completion order is not an input ",
                "to any result. A spawn past the handler's cycle cap is refused immediately ",
                "with `SpawnCapacityExhausted` (a BOUND, not a queue), and an await on a ",
                "cancelled handle is `SpawnCancelled`, never a hang.",
            ],
            type_defs [
                "data AgentId = AgentId Int deriving (Show, Eq)",
                "data BackendThreadId = BackendThreadId Text deriving (Show, Eq)",
                "data SpawnWorkspace = SpawnNewWorktree WorktreeSpec | SpawnExistingWorktree WorktreeId deriving (Show, Eq)",
                "data SpawnSpec = SpawnSpec { spawnWorkspace :: SpawnWorkspace, spawnAgentLabel :: Text, spawnTask :: Text } deriving (Show, Eq)",
                "data SpawnStage = StageAllocating | StageWorktreeReady | StageBound | StageThreadAccepted | StageRunning deriving (Show, Eq)",
                "data BackendFailure = BackendUnavailable Text | ProtocolRejected Text | RunFailed Text deriving (Show, Eq)",
                "data CyclePayload = PayloadStructured Value | PayloadUnstructured Text | PayloadAbsent deriving (Show, Eq)",
                "data WorkerRun = WorkerRun { runAgent :: AgentId, runWorktree :: WorktreeHandle, runThread :: BackendThreadId } deriving (Show, Eq)",
                "data AgentActivity = ActivityCommand Text (Maybe Int) | ActivityFileChanged Text deriving (Show, Eq)",
                "data TokenUsage = TokenUsage { usageInput :: Int, usageCachedInput :: Int, usageOutput :: Int, usageReasoningOutput :: Int, usageTotal :: Int } deriving (Show, Eq)",
                // Field ORDER is the wire contract, so every widening APPENDS —
                // `receiptRounds`/`receiptUsage` and `outcomeActivity` are the
                // tool-dispatch lane's additions, positioned last on purpose.
                "data SpawnReceipt = SpawnReceipt { receiptAgent :: AgentId, receiptWorktree :: WorktreeId, receiptBindingRef :: Text, receiptThread :: BackendThreadId, receiptModel :: Text, receiptTurn :: Text, receiptRounds :: Int, receiptUsage :: Maybe TokenUsage } deriving (Show, Eq)",
                "data SpawnOutcome = SpawnOutcome { outcomeRun :: WorkerRun, outcomePayload :: CyclePayload, outcomeReceipt :: SpawnReceipt, outcomeActivity :: [AgentActivity] } deriving (Show, Eq)",
                "data AgentStep = StepToolCall AgentId Text Text Value | StepDone SpawnOutcome deriving (Show, Eq)",
                // ToJSON for every type reachable from a SpawnError field (the
                // errors block templates the SpawnError instance itself). The
                // vendored generic default rejects multi-constructor sums, so
                // these are hand-written, same as the Worktree family's.
                "instance ToJSON SpawnStage where toJSON s = toJSON (show s)",
                "instance ToJSON BackendFailure where { toJSON (BackendUnavailable t) = object [\"backendUnavailable\" .= t]; toJSON (ProtocolRejected t) = object [\"protocolRejected\" .= t]; toJSON (RunFailed t) = object [\"runFailed\" .= t] }",
                // The async trio's handle. Opaque and cycle-scoped: the handler
                // mints it, an author echoes it back, nobody parses it. The
                // typed surface (`Tidepool.Agent.Spawn`) wraps it in an
                // ABSTRACT `AgentHandle r`, so a forged `CycleId Int` is not a
                // handle. APPENDED — type_defs order is not the wire contract,
                // but the widening rule is the same everywhere in this def.
                "data CycleId = CycleId Int deriving (Show, Eq)",
                // `SpawnCancelled` carries a CycleId, and the errors block
                // templates `ToJSON SpawnError` over its fields — so CycleId
                // needs one, same reason SpawnStage/BackendFailure do.
                "instance ToJSON CycleId where toJSON (CycleId n) = toJSON n",
            ],
            // Typed per-verb failure (#335) — typed failure results
            // everywhere; variant list is this lane's contact with reality,
            // deliberately provisional.
            errors SpawnError [
                { ctor SpawnWorktreeFailed, fields { stage: "SpawnStage" as tidepool_bridge_effects::AgSpawnStage, worktreeFailure: "WorktreeError" as crate::handlers::worktree::WorktreeError },
                  doc "the workspace could not be resolved: creation failed, or an existing id was lost/unregistered" },
                { ctor SpawnBindingFailed, fields { bindStage: "SpawnStage" as tidepool_bridge_effects::AgSpawnStage, bindingFailure: "WorktreeError" as crate::handlers::worktree::WorktreeError },
                  doc "the binding was refused (WorktreeBusy names the holder) or could not be persisted" },
                { ctor SpawnBackendFailed, fields { backendStage: "SpawnStage" as tidepool_bridge_effects::AgSpawnStage, backendFailure: "BackendFailure" as tidepool_bridge_effects::AgBackendFailure },
                  doc "the backend failed; the stage distinguishes a rejected thread from a failed cycle" },
                { ctor SpawnRollbackFailed, fields { rollbackStage: "SpawnStage" as tidepool_bridge_effects::AgSpawnStage, originalFailure: "Text" as String, rollbackFailure: "Text" as String },
                  doc "the rollback itself failed — both failures carried, rendered; never a silent swallow" },
                { ctor SpawnResultMalformed, fields { malformedDetail: "Text" as String },
                  doc "the structured terminal payload did not decode to the requested result type — produced by the Haskell-side decoder, never sent by Rust" },
                { ctor SpawnDriveFailed, fields { driveStage: "SpawnStage" as tidepool_bridge_effects::AgSpawnStage, driveDetail: "Text" as String },
                  doc "the tool-dispatch loop was driven wrongly (a reply naming an agent or call that is not the parked one), or the runtime's hard round backstop fired — a caller-sequencing failure, not a backend one" },
                { ctor SpawnCapacityExhausted, fields { capacityLimit: "Int" as i64 },
                  doc "the handler's cycle table is full — a BOUND, not a queue: a spawn past the cap is refused immediately so an operator sees the ceiling instead of an unbounded backlog forming behind it" },
                { ctor SpawnCancelled, fields { cancelledCycle: "CycleId" as tidepool_bridge_effects::AgCycleId },
                  doc "the cycle was cancelled before it produced a result — the terminal an await on a cancelled handle resolves to. A distinct constructor rather than a drive failure: 'I cancelled this' and 'this broke' call for different handling, and an author who raced their own cancel against their own await must be able to tell them apart by case, not by reading a string" },
            ],
            verbs [
                { ctor SubagentSpawn, method subagent_spawn,
                  args { spec: "SpawnSpec" as tidepool_bridge_effects::AgSpawnSpec, schema: "Value" as crate::effect_glue::JsonArg },
                  ret "SpawnOutcome", errors SpawnError },
                // The tool-dispatch pair. `tools` and the tool answer ride FLAT
                // `Value` arguments rather than bridged records because
                // `serde_json::Value` has ToCore but no FromCore — inbound JSON
                // cannot ride inside a bridged record, and `SubagentSpawn`'s
                // `schema` is the same lane.
                { ctor SubagentBegin, method subagent_begin,
                  args { spec: "SpawnSpec" as tidepool_bridge_effects::AgSpawnSpec, tools: "Value" as crate::effect_glue::JsonArg, schema: "Value" as crate::effect_glue::JsonArg },
                  ret "AgentStep", errors SpawnError },
                { ctor SubagentResume, method subagent_resume,
                  args { agent: "AgentId" as tidepool_bridge_effects::AgAgentId, call: "Text" as String, ok: "Bool" as bool, body: "Value" as crate::effect_glue::JsonArg },
                  ret "AgentStep", errors SpawnError },
                // The async trio, APPENDED — verb order is the
                // wire contract, so these three sit after the three that
                // already existed and nothing above moves. Same saga as
                // `SubagentSpawn`, detached onto its own cycle: spawn returns a
                // `CycleId` immediately, await blocks for that cycle's outcome,
                // cancel reaps it.
                { ctor SubagentSpawnAsync, method subagent_spawn_async,
                  args { spec: "SpawnSpec" as tidepool_bridge_effects::AgSpawnSpec, schema: "Value" as crate::effect_glue::JsonArg },
                  ret "CycleId", errors SpawnError },
                { ctor SubagentAwait, method subagent_await,
                  args { cycle: "CycleId" as tidepool_bridge_effects::AgCycleId },
                  ret "SpawnOutcome", errors SpawnError },
                // TOTAL, deliberately — no `errors` block. Cancelling a cycle
                // that already finished, or one this handler never minted, is a
                // NO-OP, so there is no failure to type.
                { ctor SubagentCancel, method subagent_cancel,
                  args { cycle: "CycleId" as tidepool_bridge_effects::AgCycleId },
                  ret "()" },
            ],
            helpers [
                { raw substrate ["-- | RAW one-cycle coupled spawn: workspace + binding + agent + one",
                       "-- backend cycle, atomically; `schema` is the JSON Schema the terminal",
                       "-- result must conform to. Prefer the typed wrapper in",
                       "-- `Tidepool.Agent.Spawn` (schema derived from your result type's",
                       "-- Generic representation, payload decoded by its FromJSON instance);",
                       "-- this is its substrate.",
                       "spawnAgentRaw :: forall effs. Member Subagent effs => SpawnSpec -> Value -> Eff effs (Either SpawnError SpawnOutcome)",
                       "spawnAgentRaw spec schema = send (SubagentSpawn spec schema)"] },
                { raw substrate ["-- | RAW begin of a coupled spawn that carries dynamic tools: it does",
                       "-- everything `spawnAgentRaw` does, then drives the turn to its FIRST",
                       "-- stop instead of to the end — either `StepToolCall agent callId tool",
                       "-- args` (the child called one of your tools and its turn is PARKED",
                       "-- until you answer) or `StepDone outcome` (it never called one).",
                       "-- `tools` is a JSON array of {name, description, inputSchema};",
                       "-- `schema` is the JSON Schema the terminal result must conform to.",
                       "-- Prefer `spawnAgentWithTools` in `Tidepool.Agent.Spawn`, which",
                       "-- compiles both from your types and runs the answer loop for you.",
                       "agentBeginRaw :: forall effs. Member Subagent effs => SpawnSpec -> Value -> Value -> Eff effs (Either SpawnError AgentStep)",
                       "agentBeginRaw spec tools schema = send (SubagentBegin spec tools schema)"] },
                { raw substrate ["-- | RAW answer to the parked tool call, driving the turn on to its next",
                       "-- stop (another `StepToolCall`, or `StepDone`). `agent` and `callId`",
                       "-- are echoed from the `StepToolCall` you are answering; naming a",
                       "-- different agent or a different call is refused (`SpawnDriveFailed`)",
                       "-- rather than misrouted. `ok` False is a REFUSAL the child reads and",
                       "-- reacts to — an ordinary conversational fact, not a transport error,",
                       "-- and never a way to leave the call unanswered; `body` is then the",
                       "-- text it sees. Prefer `spawnAgentWithTools` in `Tidepool.Agent.Spawn`.",
                       "agentResumeRaw :: forall effs. Member Subagent effs => AgentId -> Text -> Bool -> Value -> Eff effs (Either SpawnError AgentStep)",
                       "agentResumeRaw agent callId ok body = send (SubagentResume agent callId ok body)"] },
                { raw ["-- | Spawn in a NEW managed worktree: worktree spec, agent label, task.",
                       "spawnSpec :: WorktreeSpec -> Text -> Text -> SpawnSpec",
                       "spawnSpec wspec lbl task = SpawnSpec (SpawnNewWorktree wspec) lbl task"] },
                { raw ["-- | Spawn in an EXISTING unbound managed worktree by durable id.",
                       "-- Fails `SpawnBindingFailed` (naming the holder) if it is bound.",
                       "spawnSpecIn :: WorktreeId -> Text -> Text -> SpawnSpec",
                       "spawnSpecIn tid lbl task = SpawnSpec (SpawnExistingWorktree tid) lbl task"] },
                { raw ["-- | One-line operator-readable rendering of a spawn failure.",
                       "-- Case-match the constructor when you mean to BRANCH on it.",
                       "renderBackendFailure :: BackendFailure -> Text",
                       "renderBackendFailure (BackendUnavailable t) = \"backend unavailable: \" <> t",
                       "renderBackendFailure (ProtocolRejected t) = \"backend rejected request: \" <> t",
                       "renderBackendFailure (RunFailed t) = \"agent run failed: \" <> t"] },
                { raw substrate ["-- | RAW async spawn: everything `spawnAgentRaw` does, except that the",
                       "-- cycle runs on its OWN thread and this call returns as soon as it is",
                       "-- admitted — a `CycleId` naming the running cycle, not its outcome.",
                       "-- Refused with `SpawnCapacityExhausted` when the handler's cycle table",
                       "-- is already full; that is a BOUND, not a queue, so nothing is",
                       "-- allocated behind it. Every cycle must eventually be reaped by",
                       "-- `agentAwaitRaw` or `agentCancelRaw`. Prefer the typed wrapper",
                       "-- `spawnAsync` in `Tidepool.Agent.Spawn` (schema derived from your",
                       "-- result type, handle abstract over it); this is its substrate.",
                       "agentSpawnAsyncRaw :: forall effs. Member Subagent effs => SpawnSpec -> Value -> Eff effs (Either SpawnError CycleId)",
                       "agentSpawnAsyncRaw spec schema = send (SubagentSpawnAsync spec schema)"] },
                { raw substrate ["-- | RAW await: BLOCK until the named cycle finishes, and return the same",
                       "-- `Either SpawnError SpawnOutcome` the synchronous `spawnAgentRaw`",
                       "-- returns — the async path is the same call, reaped later. Awaiting a",
                       "-- cycle this handler never minted (or one already reaped) is a",
                       "-- `SpawnDriveFailed` at `StageRunning`: a caller-sequencing failure,",
                       "-- not a backend one. Prefer the typed wrapper `awaitAgent` in",
                       "-- `Tidepool.Agent.Spawn`, which decodes the payload into your result",
                       "-- type as `spawnAgent` does.",
                       "agentAwaitRaw :: forall effs. Member Subagent effs => CycleId -> Eff effs (Either SpawnError SpawnOutcome)",
                       "agentAwaitRaw cycle = send (SubagentAwait cycle)"] },
                { raw substrate ["-- | RAW cancel: reap the named cycle's backend and release its binding.",
                       "-- TOTAL — cancelling a cycle that already finished, or one this handler",
                       "-- never minted, is a NO-OP, so there is nothing to case-match. Retain-",
                       "-- first as everywhere else: the binding is settled, and the worktree",
                       "-- stays registered and rebindable — nothing is deleted. Prefer the",
                       "-- typed wrapper `cancelAgent` in `Tidepool.Agent.Spawn`.",
                       "agentCancelRaw :: forall effs. Member Subagent effs => CycleId -> Eff effs ()",
                       "agentCancelRaw cycle = send (SubagentCancel cycle)"] },
            ],
        }
    };
}

// Journal effect: MIGRATED to the `tidepool-protocol` schema.
// `journal_decl()` now comes from `tidepool-mcp/src/generated/journal.rs`; see
// `tidepool-protocol/src/effects/journal.rs` for the single-source definition.

// Green effect: MIGRATED to the `tidepool-protocol` schema (#20 steps 2-3,
// helperbody-flip). `green_decl()` now comes from
// `tidepool-mcp/src/generated/green.rs`; see
// `tidepool-protocol/src/effects/green.rs` for the single-source definition.

#[cfg(test)]
mod tests {
    /// Every generated `*_decl()` must be byte-identical to the hand-written
    /// builder it replaced — the effects-module source (and so the
    /// compiled-artifact cache key) must not move.
    #[test]
    fn generated_time_decl_matches_handwritten_baseline() {
        let d = crate::time_decl();
        assert_eq!(d.type_name, "Time");
        assert_eq!(
            d.description,
            "UTC wall-clock access (epoch milliseconds). \
             `getCurrentTime` returns an opaque `UTCTime` value. \
             `formatISO8601` renders it as ISO-8601 (e.g. \"2024-02-29T00:00:00Z\"). \
             `diffUTCTime a b` gives seconds between two times; `addUTCTime secs t` adds seconds. \
             `epochMillis t` exposes the raw epoch-millisecond integer."
        );
        assert_eq!(d.constructors, &["TimeNow :: Time Int"]);
        assert!(d.type_defs.is_empty());
        assert_eq!(
            d.helpers,
            &[
                "-- | Current UTC time as an opaque UTCTime (epoch-millisecond resolution).\n\
               getCurrentTime :: forall effs. Member Time effs => Eff effs UTCTime\n\
               getCurrentTime = UTCTime <$> send TimeNow"
            ]
        );
    }

    /// The generated decl threads `Either ExecError` through
    /// every verb (Run/RunIn/RunArgv), the Try* constructors are gone, and the
    /// error ADT lands in type_defs.
    #[test]
    fn generated_exec_decl_matches_handwritten_baseline() {
        let d = crate::exec_decl();
        assert_eq!(d.type_name, "Exec");
        assert_eq!(d.description, "Run shell commands and capture output.");
        assert_eq!(
            d.constructors,
            &[
                "Run :: Text -> Exec (Either ExecError Proc)",
                "RunIn :: Text -> Text -> Exec (Either ExecError Proc)",
                "RunArgv :: [Text] -> Exec (Either ExecError Proc)",
            ]
        );
        assert!(!d.constructors.iter().any(|c| c.starts_with("TryRun")));
        // Re-synced 2026-08-22: the schema (`tidepool-protocol/src/effects/exec.rs`)
        // added the `ExecTimeout` variant, which this baseline had not caught up to
        // (a genuine, deliberate content change — not renderer-wording drift). There
        // is no separate callable "prompt-text renderer" to derive this from short of
        // `exec_decl()` itself (which the test already calls); re-pasting from the
        // committed generated source (`tidepool-mcp/src/generated/exec.rs`, itself
        // single-sourced from the schema) is the closest available approximation of
        // rule 3's "derive by calling the renderer" for this effect.
        assert!(
            d.type_defs.contains(&"data ExecError = ExecSpawn Text | ExecBadDir Text | ExecTimeout Text deriving (Show, Eq)\ninstance ToJSON ExecError where\n  toJSON e = case e of\n    ExecSpawn detail -> object [\"tag\" .= (\"ExecSpawn\" :: Text), \"detail\" .= detail]\n    ExecBadDir detail -> object [\"tag\" .= (\"ExecBadDir\" :: Text), \"detail\" .= detail]\n    ExecTimeout detail -> object [\"tag\" .= (\"ExecTimeout\" :: Text), \"detail\" .= detail]\n")
        );
        assert_eq!(d.helpers.len(), 3);
        assert_eq!(
            d.helpers[0],
            "-- | Run a shell command; returns a `Proc` record {exitCode, stdout, stderr}\n\
             -- (use `ok p` for the zero-exit check). Failure is TYPED (#335): `Left\n\
             -- (ExecSpawn _)` when the process can't be spawned, `Left (ExecBadDir _)`\n\
             -- for `runIn` with a bad/escaping directory, `Left (ExecTimeout _)` when\n\
             -- the command outran its timeout and was killed. A nonzero EXIT is NOT a\n\
             -- failure — inspect `p.exitCode`. Natural spelling: `Right p <- run cmd`.\n\
             run :: forall effs. Member Exec effs => Text -> Eff effs (Either ExecError Proc)\n\
             run = send . Run"
        );
    }

    /// Meta's generated decl — pins the (deliberately normalized) output.
    #[test]
    fn generated_meta_decl_shape() {
        let d = crate::meta_decl();
        assert_eq!(d.type_name, "Meta");
        assert_eq!(d.constructors.len(), 7);
        assert_eq!(
            d.constructors[1],
            "MetaLookupCon :: Text -> Meta (Maybe (Int, Int))"
        );
        assert_eq!(d.helpers.len(), 7);
        assert!(d.helpers[0].ends_with("metaConstructors = send MetaConstructors"));
    }

    /// The `errors` grammar renders an `Either`-wrapped result
    /// for tagged verbs and a `data <Err> = …` decl for the block.
    #[test]
    fn errors_grammar_ctor_sig_and_data_decl() {
        // errors-tagged verb wraps its result in `Either <Err>`.
        assert_eq!(
            crate::effect_defs::ctor_sig!({ Fs, [], FsRead, ["Text"], "Text", errors FsError }),
            "FsRead :: Text -> Fs (Either FsError Text)"
        );
        // plain verb is unchanged (the byte-identity path).
        assert_eq!(
            crate::effect_defs::ctor_sig!({ Fs, [], FsExists, ["Text"], "Bool" }),
            "FsExists :: Text -> Fs Bool"
        );
        // the error ADT renders `|`-separated with a Show/Eq deriving.
        assert_eq!(
            crate::effect_defs::error_decl_text!(FsError,
                { ctor FsNotFound, fields { path: "Text" as String }, doc "x" },
                { ctor FsIo, fields { detail: "Text" as String }, doc "y" }),
            "data FsError = FsNotFound Text | FsIo Text deriving (Show, Eq)\ninstance ToJSON FsError where\n  toJSON e = case e of\n    FsNotFound path -> object [\"tag\" .= (\"FsNotFound\" :: Text), \"path\" .= path]\n    FsIo detail -> object [\"tag\" .= (\"FsIo\" :: Text), \"detail\" .= detail]\n"
        );
    }

    /// The generated `fs_decl()` threads `Either FsError` through
    /// the tagged verbs, leaves the untagged ones bare. `stable_errors true`
    /// (fs_stable.rs) means NEITHER `FileRead` nor `FsError` lands in
    /// `type_defs` — both live in the stable `Tidepool.Records.Stable` module
    /// instead and are imported into universal Core.
    #[test]
    fn generated_fs_decl_threads_either_and_reuses_stable_types() {
        let read = crate::fs_read_decl();
        assert!(
            read.constructors
                .contains(&"FsRead :: Text -> FsRead (Either FsError Text)"),
            "{:?}",
            read.constructors
        );
        assert!(read
            .constructors
            .contains(&"FsExists :: Text -> FsRead (Either FsError Bool)"));
        assert!(read
            .constructors
            .contains(&"FsGrep :: Text -> Text -> FsRead (Either FsError [Hit])"));
        // Untagged verbs keep their bare result.
        assert!(read
            .constructors
            .contains(&"FsMetadata :: Text -> FsRead (Maybe FileMeta)"));
        assert!(read
            .constructors
            .contains(&"FsReadGlob :: Text -> FsRead [FileRead]"));
        assert!(!read.constructors.iter().any(|c| c.contains("FsWrite")));

        let write = crate::fs_write_decl();
        assert_eq!(
            write.constructors,
            &[
                "FsWrite :: Text -> Text -> FsWrite (Either FsError ())",
                "FsWriteCas :: Text -> Maybe Text -> Text -> FsWrite (Either (Maybe Text) ())",
            ]
        );
        assert!(!write.constructors.iter().any(|c| c.contains("FsRead ::")));

        // `stable_errors true`: type_defs carries neither decl inline.
        assert!(read.type_defs.is_empty());
        assert!(write.type_defs.is_empty());
    }

    /// Every Git verb threads `Either GitError`. `stable_errors
    /// true` (fs_stable.rs precedent) means the error ADT does NOT land in
    /// `type_defs` — it reuses `Tidepool.Records.Stable` instead.
    #[test]
    fn generated_git_decl_threads_either_and_reuses_stable_types() {
        let d = crate::git_decl();
        assert!(d
            .constructors
            .contains(&"GitLog :: Int -> Git (Either GitError [Commit])"));
        assert!(d
            .constructors
            .contains(&"GitStatus :: Git (Either GitError [StatusEntry])"));
        assert!(d
            .constructors
            .contains(&"GitDiffStat :: Text -> Git (Either GitError [FileDelta])"));
        assert!(d
            .constructors
            .contains(&"GitShow :: Text -> Git (Either GitError Commit)"));
        assert!(d
            .constructors
            .contains(&"GitLogNumstat :: Int -> Git (Either GitError [CommitDeltas])"));
        assert!(d.type_defs.is_empty());
    }

    /// HttpGet/HttpPost thread `Either HttpError`,
    /// `HttpStatus` carries the status CODE as a field, and Try* is gone.
    /// JSON parsing is pure (`eitherDecode` over the JsonDecode primop) — the
    /// Http effect carries no parse verb and no bad-JSON error. `stable_errors
    /// true` means the error ADT does not land in `type_defs` (same as Git).
    #[test]
    fn generated_http_decl_threads_either_and_reuses_stable_types() {
        let d = crate::http_decl();
        assert!(d
            .constructors
            .contains(&"HttpGet :: Text -> Http (Either HttpError Value)"));
        assert!(d
            .constructors
            .contains(&"HttpPost :: Text -> Value -> Http (Either HttpError Value)"));
        assert!(!d.constructors.iter().any(|c| c.contains("ParseJson")));
        assert!(!d.constructors.iter().any(|c| c.starts_with("Try")));
        assert!(d.type_defs.is_empty());
    }

    /// LlmStructured threads `Either LlmError`, `LlmBudget` is
    /// a nullary constructor (budget exhaustion is DATA, not an abort), and
    /// TryLlmStructured is gone. `stable_errors true` means the error ADT
    /// does not land in `type_defs` (same as Git).
    #[test]
    fn generated_llm_decl_threads_either_and_reuses_stable_types() {
        let d = crate::llm_decl();
        assert!(d
            .constructors
            .contains(&"LlmStructured :: Text -> Value -> Llm (Either LlmError Value)"));
        assert!(!d.constructors.iter().any(|c| c.starts_with("Try")));
        assert!(d.type_defs.is_empty());
    }

    /// The Fork effect's generated decl: two constructors (`ForkWith`/
    /// `ForkAllWith`) carrying a site id, and the `forkSited`/`forkAllSited`
    /// executing helpers `Tidepool.Fork`'s stubs head-swap to.
    #[test]
    fn generated_fork_decl_shape() {
        let d = crate::fork_decl();
        assert_eq!(d.type_name, "Fork");
        assert_eq!(
            d.constructors,
            &[
                "ForkWith :: Int -> Text -> Fork Value",
                "ForkAllWith :: Int -> [Text] -> Fork Value",
            ]
        );
        assert!(d.type_defs.is_empty());
        assert!(d.helpers[0].contains("forkSited :: forall a effs. Member Fork effs"));
        assert!(d.helpers[0].contains("send (ForkWith sid brief)"));
        assert!(d.helpers[1].contains("forkAllSited :: forall a effs. Member Fork effs"));
        assert!(d.helpers[1].contains("send (ForkAllWith sid prompts)"));
    }

    #[test]
    fn generated_console_decl_matches_handwritten_baseline() {
        let d = crate::console_decl();
        assert_eq!(d.type_name, "Console");
        assert_eq!(d.description, "Print text output.");
        assert_eq!(d.constructors, &["Print :: Text -> Console ()"]);
        assert!(d.type_defs.is_empty());
        assert_eq!(
            d.helpers,
            &[
                "-- | Emit a line of console output. Thin wrapper over the Print effect\n\
                 -- so chains never need `send (Print …)`.\n\
                 say :: forall effs. Member Console effs => Text -> Eff effs ()\n\
                 say = send . Print",
            ]
        );
    }
}
