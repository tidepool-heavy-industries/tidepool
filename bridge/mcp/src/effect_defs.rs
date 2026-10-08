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
//! 2. `effect_rust_projection!` (`bridge/handlers/src/effect_glue.rs`) —
//!    generates the `#[derive(FromHaskell)] enum *Req`, the `DescribeEffect`
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
//!   `#[derive(ToHaskell, FromHaskell, Debug, PartialEq, Eq)] pub enum FsError {
//!   FsNotFound(String), … }` (ToHaskell/FromHaskell use plain name+arity lookup, like
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
/// two-projections-must-be-kept-in-sync-by-hand gate (friction #23).
macro_rules! extra_imports_for {
    (Git) => {
        &["import qualified Tidepool.Git as Git"]
    };
    // AskUser was migrated to the `tidepool-protocol` schema; its
    // `extra_imports` (`import Tidepool.Form`) is schema data now, emitted
    // straight into its generated decl. See
    // `bridge/protocol/src/effects/ask_user.rs`.
    //
    // `Ask` stays hand-carried (see `ask_effect_def!`'s doc and
    // `bridge/protocol/src/effects/ask.rs`'s module doc for why it can't
    // flip onto the generator), but its pure `Schema`/`schemaToHaskell`/
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
    // `Llm`'s composed `llm` (built on `schemaToHaskell`, `Tidepool.Form.
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
    // gated AskUser), so this import is unconditionally live. It
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
    // Green was migrated to the `tidepool-protocol` schema; its
    // `extra_imports` (`import Tidepool.Async` — the green-thread surface
    // rides `Green`'s substrate verbs) is schema data now, emitted straight
    // into its generated decl. See `bridge/protocol/src/effects/green.rs`.
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
            }
        }
    };
    // An unparameterized effect: forward to the arm below
    // with an empty type-parameter list. Two arms rather than one optional
    // slot because the parameters are consumed INSIDE the per-constructor
    // repetition (each constructor's result type is the applied head,
    // `State s a`), and macro_rules cannot nest an optional group there.
    (
        effect $eff:ident,
        handler $handler:ident,
        req $req:ident,
        decl_fn $decl_fn:ident,
        $(prompt_card $pc:tt,)?
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
            }
        }
    };
}
pub(crate) use effect_decl_projection;

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
            description [
                "Randomness. Seeded, deterministic generation is pure Haskell — no effect ",
                "needed: `mkStdGen :: Int -> StdGen`, `randomR :: (a, a) -> StdGen -> (a, ",
                "StdGen)`, `randoms :: StdGen -> [a]`, `split :: StdGen -> (StdGen, StdGen)` ",
                "(Int and Double instances; each `randomR` draw stays within the given ",
                "bounds, inclusive; the same seed always replays the same sequence). For ",
                "non-deterministic values seeded from OS entropy: `newStdGen :: Member Entropy effects => Eff effects StdGen` and ",
                "`randomRIO :: Member Entropy effects => (a, a) -> Eff effects a`, e.g. `n <- randomRIO (1 :: Int, 100)`.",
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
            // `errors GitError` in `git_effect_def!` / `bridge/mcp/src/fs_stable.rs`.
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
            // #335 typed-failure ADT. `GitBadRevspec` covers a revspec rejected
            // before execution (for example, option-shaped input); `GitFailed`
            // carries every subprocess failure. Structured-output corruption and
            // paths outside the Haskell Text domain have their own constructors.
            // Reuse the pre-schema `GitError` exported by
            // `Tidepool.Records.Stable`; do not declare a duplicate in Core.
            errors GitError [
                { ctor GitBadRevspec, fields { detail: "Text" as String },                     doc "revspec rejected before execution" },
                { ctor GitFailed,          fields { code: "Int" as i64, detail: "Text" as String },  doc "git exited nonzero (or could not be spawned)" },
                { ctor GitMalformedOutput, fields { detail: "Text" as String },                     doc "git returned malformed structured output" },
                { ctor GitNonUtf8Path,     fields { path: "Text" as String },                       doc "git returned a path that is not valid UTF-8" },
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
                  doc ["Single commit by revspec. `Left (GitBadRevspec _)` when the handler rejects",
                       "option-shaped input; Git subprocess failures return `GitFailed`. Unwrap with",
                       "`Right c <- gitShow rev` or `>>= liftEither`."],
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
/// machinery (`bridge/mcp/src/ask.rs`, `tidepool-repl/src/ask.rs`), so only
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
            description [
                "Suspend execution and ask the calling agent a STRUCTURED question. ",
                "`ask schema prompt` carries the schema as JSON Schema in the suspension ",
                "for the caller to use; Ask does not validate resume replies. ",
                "Extract fields from the returned Value with optics, e.g. ",
                "`v ^? key \"path\" . _String`.",
            ],
            // The Schema vocabulary (`data Schema`, `schemaToHaskell`, `isOpt`,
            // `innerSchema`) AND `ask` itself now live in the stdlib —
            // `Tidepool.Form.Schema`, auto-imported via
            // `extra_imports_for!(Ask)` — because they are ordinary pure/
            // composed Haskell, not decl-shaped verb surface (issue #24's
            // first act; see `bridge/protocol/src/effects/ask.rs`'s module
            // doc). Only the THIN raw verb wrapper (`askRaw`, bare `send
            // (Ctor …)`) stays spliced into the generated module below — the
            // generated module cannot import authored library code (see
            // `extra_imports_for!`'s own doc), so anything referencing
            // `Schema`/`schemaToHaskell` must live OUTSIDE it, same split as
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
                  args { prompt: "Text" as String, payload: "Value" as tidepool_bridge::HaskellValue },
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
// `askuser_decl()` now comes from `bridge/mcp/src/generated/ask_user.rs`;
// see `bridge/protocol/src/effects/ask_user.rs` for the single-source
// definition.

// ReadState effect: MIGRATED to the `tidepool-protocol` schema (#20 steps
// 2-3). `readstate_decl()` now comes from
// `bridge/mcp/src/generated/read_state.rs`; see
// `bridge/protocol/src/effects/read_state.rs` for the single-source
// definition.

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
            description [
                "Call an LLM for classification, extraction, or judgment. ",
                "`llm schema prompt` requests schema-constrained JSON from the provider ",
                "and returns a parsed Value. Extract with optics, e.g. ",
                "`v ^? key \"category\" . _String`.",
            ],
            type_defs [],
            // #335 typed-failure ADT. FULLY TOTAL: budget exhaustion is now DATA
            // (`LlmBudget`), not an abort — nothing in the Llm path kills the eval.
            // STABLE home (`stable_errors true`, below) — see the same note on
            // `errors GitError` above / `bridge/mcp/src/fs_stable.rs`.
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
                // The composed `llm` (which calls `schemaToHaskell`) lives in
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
/// KV failures are typed so authored callers can decide how to recover.
#[macro_export]
macro_rules! kv_effect_def {
    ($project:path) => {
        $project! {
            effect KV,
            handler KvHandler,
            req KvReq,
            decl_fn kv_decl,
            description [
                "Persistent key-value store. State survives across calls within one server session. ",
                "Key convention: use slash-delimited namespaces (e.g. \"agent-42/foo\") to avoid ",
                "cross-agent collision. kvClear/kvKeysP operate on prefix boundaries. For a typed ",
                "round-trip instead of hand-unwrapping the stored `Value`, use `kvGetAs @T key` ",
                "(`Tidepool.Kv`) — decodes via `FromJSON`, returning `Right (Just x)`/`Right Nothing` ",
                "or a typed `KvError` for storage or decode failures.",
            ],
            type_defs [],
            errors KvError [
                { ctor KvCorrupt, fields { detail: "Text" as String }, doc "the backing file contains invalid JSON" },
                { ctor KvIo, fields { detail: "Text" as String }, doc "a storage operation failed before publication" },
                { ctor KvDurabilityUnknown, fields { detail: "Text" as String }, doc "the new value is visible but its durability could not be confirmed" },
                { ctor KvDecode, fields { detail: "Text" as String }, doc "a stored value could not be decoded to the requested type" },
            ],
            verbs [
                { ctor KvGet, method kv_get,
                  args { key: "Text" as String },
                  ret "(Maybe Value)", errors KvError },
                { ctor KvSet, method kv_set,
                  args { key: "Text" as String, val: "Value" as crate::effect_glue::JsonArg },
                  ret "()", errors KvError },
                { ctor KvDelete, method kv_delete,
                  args { key: "Text" as String },
                  ret "()", errors KvError },
                { ctor KvKeys, method kv_keys,
                  args { },
                  ret "[Text]", errors KvError },
                // Delete all keys with the given prefix; return count deleted.
                // Pass "" to clear the ENTIRE store (dangerous — see kvClear docstring).
                { ctor KvClear, method kv_clear,
                  args { prefix: "Text" as String },
                  ret "Int", errors KvError },
                // List keys matching a prefix, sorted.
                { ctor KvKeysP, method kv_keys_p,
                  args { prefix: "Text" as String },
                  ret "[Text]", errors KvError },
                // Summary: {count, sample, file_size_bytes} — inspect the junk-drawer.
                { ctor KvInfo, method kv_info,
                  args { },
                  ret "Value", errors KvError },
                // Cross-process compare-and-swap: set key=new only if its
                // current value equals `expected` (Nothing = require absent).
                // `Right (Left actual)` on conflict, preserving absence vs JSON null.
                { ctor KvCas, method kv_cas,
                  args { key: "Text" as String,
                         expected: "Maybe Value" as Option<crate::effect_glue::JsonArg>,
                         new: "Value" as crate::effect_glue::JsonArg },
                  ret "(Either (Maybe Value) ())", errors KvError },
            ],
            helpers [
                { name kvGet, sig "forall effs. Member KV effs => Text -> Eff effs (Either KvError (Maybe Value))",
                  doc ["Look up a key; Nothing when absent."],
                  body pointfree KvGet },
                { name kvSet, sig "forall effs. Member KV effs => Text -> Value -> Eff effs (Either KvError ())",
                  doc ["Persist a JSON value under a key."],
                  body applied KvSet(k, v) },
                { name kvDel, sig "forall effs. Member KV effs => Text -> Eff effs (Either KvError ())",
                  doc ["Delete a key (no-op when absent)."],
                  body pointfree KvDelete },
                { name kvClear, sig "forall effs. Member KV effs => Text -> Eff effs (Either KvError Int)",
                  doc ["Delete all keys whose name starts with @prefix@; return the count deleted.",
                       "Pass \"\" (empty string) to clear the ENTIRE store — this erases ALL",
                       "persisted KV data for this server session, so use with caution.",
                       "Recommended pattern: namespace keys as \"ns/key\" and clear with \"ns/\".",
                       "NOTE: per-session automatic scoping is a deferred design decision (#327);",
                       "callers manage namespaces manually via this prefix argument."],
                  body pointfree KvClear },
                { name kvKeysP, sig "forall effs. Member KV effs => Text -> Eff effs (Either KvError [Text])",
                  doc ["All keys whose name starts with @prefix@, returned sorted.",
                       "E.g. @kvKeysP \"agent/\"@ returns @[\"agent/bar\", \"agent/foo\", ...]@.",
                       "Pass \"\" to list ALL keys, sorted."],
                  body pointfree KvKeysP },
                { name kvInfo, sig "forall effs. Member KV effs => Eff effs (Either KvError Value)",
                  doc ["Summary of KV store state as a JSON Value:",
                       "@{count :: Int, sample :: [Text], file_size_bytes :: Int}@.",
                       "Use to inspect junk-drawer accumulation without listing all keys.",
                       "Extract fields with optics: @Right i <- kvInfo; i ^? key \"count\" . _Int@"],
                  body nullary KvInfo },
                { name kvCas, sig "forall effs. Member KV effs => Text -> Maybe Value -> Value -> Eff effs (Either KvError (Either (Maybe Value) ()))",
                  doc ["Atomic compare-and-swap: set @key@ to @new@ only if its current",
                       "value equals @expected@ (Nothing = require the key ABSENT).",
                       "@Right (Right ())@ on success; @Right (Left actual)@ on conflict,",
                       "where @actual@ is @Nothing@ for absence and @Just value@ otherwise.",
                       "Outer @Left@ carries a storage failure. Cross-process safe (the store",
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
                { ctor FsDurabilityUnknown, fields { detail: "Text" as String }, doc "the write is visible but storage durability could not be confirmed" },
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
                { raw ["-- | Dry-run `update`: returns an `UpdateOutcome` (the review diff, or the\n-- reason it can't apply), writes NOTHING. Never errors — the conflict comes\n-- back as data so you can branch before committing.\nplanUpdate :: forall effs. Member FsRead effs => FilePath -> Text -> Text -> Eff effs UpdateOutcome\nplanUpdate path old new = do\n  er <- readFile path\n  case er of\n    Left e -> pure (UpdateRejected (\"file not found: \" <> T.pack (show e)) Nothing)\n    Right src ->\n      let n = if T.null old then 0 else length (T.splitOn old src) - 1\n      in if T.null old then pure (UpdateRejected \"'old' must be non-empty\" Nothing)\n         else if n == 0 then pure (UpdateRejected \"not found\" Nothing)\n         else if n > 1 then pure (UpdateRejected \"ambiguous\" (Just n))\n         else case Patch.genPatch path src (replace old new src) of\n                Left _ -> pure UpdateNoChange\n                Right fp -> pure (UpdateDiff (Patch.renderPatch [fp]))"] },
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
            description ["Mutate files (sandboxed to server working directory)."],
            type_defs [],
            errors FsError [
                { ctor FsNotFound, fields { path: "Text" as String },   doc "path does not exist" },
                { ctor FsNotUtf8,  fields { path: "Text" as String },   doc "file is not valid UTF-8" },
                { ctor FsSandbox,  fields { detail: "Text" as String }, doc "path escapes the sandbox, or the glob pattern is not allowed" },
                { ctor FsBadRegex, fields { detail: "Text" as String }, doc "grep regex failed to compile" },
                { ctor FsIo,       fields { detail: "Text" as String }, doc "other I/O failure" },
                { ctor FsDurabilityUnknown, fields { detail: "Text" as String }, doc "the write is visible but storage durability could not be confirmed" },
                { ctor FsNonUtf8Path, fields { path: "Text" as String }, doc "path is not valid UTF-8 (lossy rendering shown for diagnostics)" },
            ],
            stable_errors true,
            verbs [
                { ctor FsWrite, method fs_write,
                  args { path: "Text" as String, content: "Text" as String },
                  ret "()", errors FsError },
                { ctor FsWriteCas, method fs_write_cas,
                  args { path: "Text" as String, expected: "Maybe Text" as Option<String>, content: "Text" as String },
                  ret "(Either (Maybe Text) ())", errors FsError },
            ],
            helpers [
                { raw ["-- | Write a file (mkdir -p on the parent). `Left (FsSandbox _)` on a path\n-- escape, `Left (FsIo _)` on write failure; unwrap with `liftEither`.\nwriteFile :: forall effs. Member FsWrite effs => FilePath -> Text -> Eff effs (Either FsError ())\nwriteFile f c = send (FsWrite f c)"] },
                { raw ["-- | Append to a file (reads then writes). Failure is TYPED (#335): a read\n-- or write failure comes back as `Left (FsError)` DATA, nothing partially\n-- applied; unwrap with `Right () <- appendFile path t` or `>>= liftEither`.\nappendFile :: forall effs. Members '[FsRead, FsWrite] effs => FilePath -> Text -> Eff effs (Either FsError ())\nappendFile p t = do\n  er <- readFile p\n  case er of\n    Left e -> pure (Left e)\n    Right old -> writeFile p (old <> t)"] },
                { raw ["-- | Exact str-replace, EXACTLY-ONCE. Reports the outcome as an\n-- `UpdateOneOutcome` DATA value (never throws, mirrors `InsertAfterOutcome`):\n-- empty `old`, a missing file, `old` not found, or `old` matching 2+ places\n-- is `UpdateOneRejected` (nothing written); otherwise `UpdateOneApplied`.\n-- Pass enough surrounding text that `old` is unique. Use planUpdate to review\n-- the diff first; the full editing surface is in tidepool://edits.\nupdate :: forall effs. Members '[FsRead, FsWrite] effs => FilePath -> Text -> Text -> Eff effs UpdateOneOutcome\nupdate path old new\n  | T.null old = pure (UpdateOneRejected \"'old' must be non-empty\" Nothing)\n  | otherwise = do\n      er <- readFile path\n      case er of\n        Left e -> pure (UpdateOneRejected (\"file not found: \" <> T.pack (show e)) Nothing)\n        Right src ->\n          case length (T.splitOn old src) - 1 of\n            0 -> pure (UpdateOneRejected (\"'old' not found in \" <> path) Nothing)\n            1 -> writeFile path (replace old new src) >>= liftEither >> pure UpdateOneApplied\n            n -> pure (UpdateOneRejected (\"'old' matches \" <> T.pack (show n) <> \" places in \" <> path <> \" (add surrounding context to disambiguate)\") (Just n))"] },
                { raw ["-- | Replace EVERY occurrence of `old` with `new`. Reports the outcome as an\n-- `UpdateAllOutcome` DATA value (never throws): empty `old`, a missing file,\n-- or zero matches is `UpdateAllRejected` (nothing written); otherwise\n-- `UpdateAllApplied` carries the replacement count.\nupdateAll :: forall effs. Members '[FsRead, FsWrite] effs => FilePath -> Text -> Text -> Eff effs UpdateAllOutcome\nupdateAll path old new\n  | T.null old = pure (UpdateAllRejected \"'old' must be non-empty\")\n  | otherwise = do\n      er <- readFile path\n      case er of\n        Left e -> pure (UpdateAllRejected (\"file not found: \" <> T.pack (show e)))\n        Right src ->\n          let n = length (T.splitOn old src) - 1\n          in if n == 0\n               then pure (UpdateAllRejected (\"'old' not found in \" <> path))\n               else writeFile path (replace old new src) >>= liftEither >> pure (UpdateAllApplied n)"] },
                { raw ["-- | `update` from the `input` JSON parameter: {file, old, new} (for big/quote-heavy\n-- fragments). Reports the outcome as an `UpdateOneOutcome` DATA value (never\n-- throws, same contract as `update`): a malformed payload (missing or\n-- non-string file/old/new key) is `UpdateOneRejected` — one bad item never\n-- aborts a batch.\nupdateJ :: forall effs. Members '[FsRead, FsWrite] effs => Value -> Eff effs UpdateOneOutcome\nupdateJ v = case (v ^? key \"file\" . _String, v ^? key \"old\" . _String, v ^? key \"new\" . _String) of\n  (Just f, Just o, Just n) -> update f o n\n  _ -> pure (UpdateOneRejected \"updateJ: need {file, old, new} strings in input\" Nothing)"] },
                { raw ["-- | Insert a block after the unique line containing `anchor`. Reports the\n-- outcome as an `InsertAfterOutcome` DATA value (never throws): a missing\n-- file, or an anchor matching zero or 2+ lines, is `InsertAfterRejected`\n-- (nothing written); otherwise `InsertAfterApplied`.\ninsertAfter :: forall effs. Members '[FsRead, FsWrite] effs => FilePath -> Text -> Text -> Eff effs InsertAfterOutcome\ninsertAfter path anchor block = do\n  er <- readFile path\n  case er of\n    Left e -> pure (InsertAfterRejected (\"file not found: \" <> T.pack (show e)) Nothing)\n    Right src ->\n      let ls = lines src\n          n = length (filter (isInfixOf anchor) ls)\n      in case n of\n           1 -> writeFile path (unlines (concatMap (\\l -> if anchor `isInfixOf` l then [l, block] else [l]) ls))\n                  >>= liftEither >> pure InsertAfterApplied\n           _ -> pure (InsertAfterRejected (\"anchor matched \" <> T.pack (show n) <> \" lines in \" <> path) (Just n))"] },
                { raw ["-- | Compute-check-commit: write only if every named check holds; failures\n-- come back as a `WriteOutcome` (nothing written on failure).\nwriteChecked :: forall effs. Member FsWrite effs => FilePath -> [(Text, Bool)] -> Text -> Eff effs WriteOutcome\nwriteChecked path checks content = do\n  let failed = [name | (name, ok) <- checks, not ok]\n  if null failed\n    then writeFile path content >>= liftEither >> pure (Written path (length checks))\n    else pure (WriteBlocked path failed)"] },
                { raw ["-- | Content-hash compare-and-swap write (#330). Writes CONTENT only if the\n-- file's current blake3 hash equals EXPECTED (Nothing = expect the file ABSENT,\n-- i.e. create-only). The compare-and-write is atomic within the handler, closing\n-- the lost-update race between parallel agents. Returns a WriteOutcome: 'Written'\n-- on success, or 'WriteConflict' (carrying expected vs actual hash) if the\n-- precondition failed — conflicts come back as DATA, nothing is written.\n-- Storage and sandbox failures are `Left FsError`; inspect them before using\n-- the `WriteOutcome`. Get EXPECTED from fileHash; on conflict re-read,\n-- recompute, and retry. Never retry `FsDurabilityUnknown`: the write is visible.\nwriteCheckedIf :: forall effs. Member FsWrite effs => Maybe Text -> FilePath -> Text -> Eff effs (Either FsError WriteOutcome)\nwriteCheckedIf expected path content = do\n  r <- send (FsWriteCas path expected content)\n  pure $ case r of\n    Left err -> Left err\n    Right (Right ()) -> Right (Written path 1)\n    Right (Left actual) -> Right (WriteConflict path expected actual)"] },
            ],
        }
    };
}

// Journal effect: MIGRATED to the `tidepool-protocol` schema.
// `journal_decl()` now comes from `bridge/mcp/src/generated/journal.rs`; see
// `bridge/protocol/src/effects/journal.rs` for the single-source definition.

// Green effect: MIGRATED to the `tidepool-protocol` schema (#20 steps 2-3,
// helperbody-flip). `green_decl()` now comes from
// `bridge/mcp/src/generated/green.rs`; see
// `bridge/protocol/src/effects/green.rs` for the single-source definition.

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
        // Re-synced 2026-08-22: the schema (`bridge/protocol/src/effects/exec.rs`)
        // added the `ExecTimeout` variant, which this baseline had not caught up to
        // (a genuine, deliberate content change — not renderer-wording drift). There
        // is no separate callable "prompt-text renderer" to derive this from short of
        // `exec_decl()` itself (which the test already calls); re-pasting from the
        // committed generated source (`bridge/mcp/src/generated/exec.rs`, itself
        // single-sourced from the schema) is the closest available approximation of
        // rule 3's "derive by calling the renderer" for this effect.
        assert!(
            d.type_defs.contains(&"data ExecError = ExecSpawn Text | ExecBadDir Text | ExecTimeout Text | ExecOutput Text | ExecWait Text deriving (Show, Eq)\ninstance ToJSON ExecError where\n  toJSON e = case e of\n    ExecSpawn detail -> object [\"tag\" .= (\"ExecSpawn\" :: Text), \"detail\" .= detail]\n    ExecBadDir detail -> object [\"tag\" .= (\"ExecBadDir\" :: Text), \"detail\" .= detail]\n    ExecTimeout detail -> object [\"tag\" .= (\"ExecTimeout\" :: Text), \"detail\" .= detail]\n    ExecOutput detail -> object [\"tag\" .= (\"ExecOutput\" :: Text), \"detail\" .= detail]\n    ExecWait detail -> object [\"tag\" .= (\"ExecWait\" :: Text), \"detail\" .= detail]\n")
        );
        assert_eq!(d.helpers.len(), 3);
        assert_eq!(
            d.helpers[0],
            "-- | Run a shell command; returns a `Proc` record {exitCode, stdout, stderr}\n\
             -- (use `ok p` for the zero-exit check). Failure is TYPED (#335): `Left\n\
             -- (ExecSpawn _)` when the process can't be spawned, `Left (ExecBadDir _)`\n\
             -- for `runIn` with a bad/escaping directory, `Left (ExecTimeout _)` when\n\
             -- execution or output draining outran its timeout, `Left (ExecOutput _)` for a read failure,\n\
             -- and `Left (ExecWait _)` if its exit status cannot be collected. A nonzero\n\
             -- EXIT is NOT a failure — inspect `p.exitCode`. Natural spelling:\n\
             -- `Right p <- run cmd`.\n\
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
                "FsWriteCas :: Text -> Maybe Text -> Text -> FsWrite (Either FsError (Either (Maybe Text) ()))",
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

    #[test]
    fn generated_kv_decl_types_every_verb_failure_and_preserves_absence_in_cas() {
        let decl = crate::kv_decl();
        assert_eq!(decl.constructors.len(), 8);
        assert!(decl
            .constructors
            .iter()
            .all(|constructor| constructor.contains("KV (Either KvError ")));
        assert!(decl
            .constructors
            .iter()
            .any(|constructor| constructor.contains("Either (Maybe Value) ()")));
        assert!(decl.type_defs.iter().any(|definition| {
            definition.contains("data KvError = KvCorrupt Text | KvIo Text | KvDurabilityUnknown Text | KvDecode Text")
        }));
    }

    #[test]
    fn generated_console_decl_contains_actor_display_protocol() {
        let d = crate::console_decl();
        assert_eq!(d.type_name, "Console");
        assert!(d.description.contains("operator feed"));
        assert_eq!(
            d.constructors,
            &[
                "Print :: Text -> Console ()",
                "DisplayViewWith :: Value -> Console ()",
                "DisplayWith :: ((Int, Int, Int), Text, [(Int, Text)], Bool) -> payload -> Console (Int, Int, Int)",
                "DisplayExpandWith :: ((Int, Int, Int), Int) -> Console [(Int, Text)]",
                "DisplayAllowanceWith :: Console Int",
                "DisplayExpansionInputWith :: Console ((Int, Int, Int), Int, Int)",
            ]
        );
        assert!(d.type_defs.is_empty());
        assert!(d
            .helpers
            .iter()
            .any(|helper| helper.contains("say = send . Print")));
        assert!(d
            .helpers
            .iter()
            .any(|helper| helper.contains("displayViewRaw")));
    }
}
