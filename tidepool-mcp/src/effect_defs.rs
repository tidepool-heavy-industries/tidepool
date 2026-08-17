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
/// (`preamble::eval_import_lines`) — `Exec`'s helpers build on `runArgv`
/// (`Tidepool.Shell`/`Tidepool.Cargo`), `Git`'s on the Git verbs
/// (`Tidepool.Git`), `AskUser`'s `Tidepool.Form` on `askUserRaw`. Every other
/// effect needs nothing beyond the fixed surface.
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
    (Exec) => {
        &[
            "import qualified Tidepool.Shell as Shell",
            "import Tidepool.Shell (sh)",
            "import qualified Tidepool.Cargo as Cargo",
        ]
    };
    (Git) => {
        &["import qualified Tidepool.Git as Git"]
    };
    (AskUser) => {
        &["import Tidepool.Form"]
    };
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
            description ["Print text output."],
            type_defs [],
            verbs [
                { ctor Print, method print,
                  args { msg: "Text" as String },
                  ret "()" },
            ],
            helpers [
                { name say, sig "Text -> M ()",
                  doc ["Emit a line of console output. Thin wrapper over the Print effect",
                       "so chains never need `send (Print …)`."],
                  body pointfree Print },
                { raw ["-- | `say` on anything Showable (`say . show`).",
                       "sayShow :: Show a => a -> M ()",
                       "sayShow = say . show"] },
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
                       "getCurrentTime :: M UTCTime",
                       "getCurrentTime = UTCTime <$> send TimeNow"] },
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
                { name metaConstructors, sig "M [(Text, Int)]",
                  doc ["Constructor table: (name, arity) pairs."],
                  body nullary MetaConstructors },
                { name metaLookupCon, sig "Text -> M (Maybe (Int, Int))",
                  doc ["Look up a constructor by name: (tag, arity)."],
                  body pointfree MetaLookupCon },
                { name metaPrimOps, sig "M [Text]",
                  doc ["Names of the JIT-implemented primops."],
                  body nullary MetaPrimOps },
                { name metaEffects, sig "M [Text]",
                  doc ["Effect type names in the running stack."],
                  body nullary MetaEffects },
                { name metaDiagnostics, sig "M [Text]",
                  doc ["Drain pending runtime diagnostics."],
                  body nullary MetaDiagnostics },
                { name metaVersion, sig "M Text",
                  doc ["Server crate version."],
                  body nullary MetaVersion },
                { name metaHelp, sig "M [Text]",
                  doc ["Helper-verb signatures of the running stack."],
                  body nullary MetaHelp },
            ],
        }
    };
}

/// Exec effect — single definition.
#[macro_export]
macro_rules! exec_effect_def {
    ($project:path) => {
        $project! {
            effect Exec,
            handler ExecHandler,
            req ExecReq,
            decl_fn exec_decl,
            description ["Run shell commands and capture output."],
            type_defs [],
            // #335 typed-failure ADT. A nonzero EXIT is NOT a failure here — `run`
            // still returns a Proc with its exitCode on nonzero exit; `Left` is
            // only for a spawn failure or a bad/escaping working directory.
            errors ExecError [
                { ctor ExecSpawn,  fields { detail: "Text" as String }, doc "the process could not be spawned" },
                { ctor ExecBadDir, fields { detail: "Text" as String }, doc "working directory is invalid or escapes the sandbox" },
            ],
            verbs [
                { ctor Run, method exec_run,
                  args { cmd: "Text" as String },
                  ret "Proc", errors ExecError },
                { ctor RunIn, method exec_run_in,
                  args { dir: "Text" as String, cmd: "Text" as String },
                  ret "Proc", errors ExecError },
                // Shell-free exec: argv list, no sh -c. Safe with metachars ($1, globs).
                { ctor RunArgv, method exec_run_argv,
                  args { argv: "[Text]" as Vec<String> },
                  ret "Proc", errors ExecError },
            ],
            helpers [
                { raw ["-- | Run a shell command; returns a `Proc` record {exitCode, stdout, stderr}",
                       "-- (use `ok p` for the zero-exit check). Failure is TYPED (#335): `Left",
                       "-- (ExecSpawn _)` when the process can't be spawned, `Left (ExecBadDir _)`",
                       "-- for `runIn` with a bad/escaping directory. A nonzero EXIT is NOT a",
                       "-- failure — inspect `p.exitCode`. Natural spelling: `Right p <- run cmd`.",
                       "run :: Text -> M (Either ExecError Proc)",
                       "run = send . Run"] },
                { raw ["runIn :: Text -> Text -> M (Either ExecError Proc)",
                       "runIn dir cmd = send (RunIn dir cmd)"] },
                // Shell-free: argv list, no sh -c. $1/$VAR/globs are literal — safe.
                { raw ["runArgv :: [Text] -> M (Either ExecError Proc)",
                       "runArgv = send . RunArgv"] },
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
            errors HttpError [
                { ctor HttpInvalidUrl, fields { detail: "Text" as String },              doc "the URL is malformed or uses an unsupported scheme" },
                { ctor HttpRestricted, fields { detail: "Text" as String },              doc "the URL targets a sandboxed/internal address" },
                { ctor HttpNetwork,    fields { detail: "Text" as String },              doc "a network-level failure (connect/timeout/read)" },
                { ctor HttpStatus,     fields { code: "Int" as i64, body: "Text" as String }, doc "a non-2xx HTTP response" },
                { ctor HttpTooLarge,   fields { nodes: "Int" as i64 },                   doc "the JSON response exceeds the materialization cap (node count); narrow the query" },
            ],
            verbs [
                { ctor HttpGet, method http_get,
                  args { url: "Text" as String },
                  ret "Value", errors HttpError },
                { ctor HttpPost, method http_post,
                  args { url: "Text" as String, body: "Value" as crate::effect_glue::JsonArg },
                  ret "Value", errors HttpError },
            ],
            helpers [
                { name httpGet, sig "Text -> M (Either HttpError Value)",
                  doc ["Fetch JSON from an HTTP endpoint. Failure is TYPED (#335): `Left",
                       "(HttpStatus code body)` on a non-2xx response, `Left (HttpNetwork _)`",
                       "on a network failure. Unwrap with `Right v <- httpGet url` or `>>= liftEither`."],
                  body pointfree HttpGet },
                { name httpPost, sig "Text -> Value -> M (Either HttpError Value)",
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
                "`gitDiffStat rev` → per-file diff stats vs a revspec; `gitShow rev` → one commit. ",
                "All three list verbs return typed records: `Commit {sha,subject,author,date,files}`, ",
                "`StatusEntry {path,state}` (state = 2-char XY porcelain code), ",
                "`FileDelta {path,adds,dels,binary}`.",
            ],
            type_defs [],
            // #335 typed-failure ADT. `GitBadRevspec` covers an unknown/ambiguous
            // revspec (also a `gitShow` with zero matching commits); `GitFailed`
            // is the residual (git exited nonzero for another reason, or the git
            // binary itself couldn't be spawned — exit code -1 in that case).
            errors GitError [
                { ctor GitBadRevspec, fields { detail: "Text" as String },                     doc "unknown or ambiguous revspec" },
                { ctor GitFailed,     fields { code: "Int" as i64, detail: "Text" as String },  doc "git exited nonzero (or could not be spawned)" },
            ],
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
            ],
            helpers [
                { name gitLog, sig "Int -> M (Either GitError [Commit])",
                  doc ["Last N commits, newest-first. Each 'Commit' carries sha/subject/author/date/files."],
                  body pointfree GitLog },
                { name gitStatus, sig "M (Either GitError [StatusEntry])",
                  doc ["Working-tree status. Each 'StatusEntry' has path and 2-char XY state code",
                       "(e.g. \"M \", \"??\", \"A \")."],
                  body nullary GitStatus },
                { name gitDiffStat, sig "Text -> M (Either GitError [FileDelta])",
                  doc ["Per-file diff stats vs a revspec (\"HEAD~1\", \"main\", \"HEAD~3..HEAD\", etc.).",
                       "'FileDelta' carries path/adds/dels/binary."],
                  body pointfree GitDiffStat },
                { name gitShow, sig "Text -> M (Either GitError Commit)",
                  doc ["Single commit by revspec. `Left (GitBadRevspec _)` on an unknown or",
                       "ambiguous revspec; unwrap with `Right c <- gitShow rev` or `>>= liftEither`."],
                  body pointfree GitShow },
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
            description [
                "Suspend execution and ask the calling agent a STRUCTURED question. ",
                "`ask schema prompt` carries the schema as JSON Schema in the suspension; ",
                "the resume reply is validated against it server-side before re-entering ",
                "the computation (invalid replies do NOT consume the continuation). ",
                "Extract fields from the returned Value with optics, e.g. ",
                "`v ^? key \"path\" . _String`.",
            ],
            // Schema vocabulary lives on the Ask effect (always present in
            // every stack) so .tidepool/lib modules and Llm-less stacks can
            // build schemas. llm (llm_decl) references schemaToValue from
            // here — same generated module.
            type_defs [
                "data Schema = SObj [(Text, Schema)] | SArr Schema | SStr | SNum | SBool | SEnum [Text] | SOpt Schema",
            ],
            verbs [
                { ctor AskWith, method ask_with,
                  args { prompt: "Text" as String, payload: "Value" as tidepool_eval::value::Value },
                  ret "Value" },
            ],
            helpers [
                { raw ["ask :: Schema -> Text -> M Value",
                       "ask schema prompt = send (AskWith prompt (object [\"schema\" .= schemaToValue schema]))"] },
                { raw ["isOpt :: Schema -> Bool",
                       "isOpt (SOpt _) = True",
                       "isOpt _ = False"] },
                { raw ["innerSchema :: Schema -> Schema",
                       "innerSchema (SOpt s) = s",
                       "innerSchema s = s"] },
                { raw ["schemaToValue :: Schema -> Value",
                       "schemaToValue SStr = object [\"type\" .= (\"string\" :: Text)]",
                       "schemaToValue SNum = object [\"type\" .= (\"number\" :: Text)]",
                       "schemaToValue SBool = object [\"type\" .= (\"boolean\" :: Text)]",
                       "schemaToValue (SEnum vs) = object [\"type\" .= (\"string\" :: Text), \"enum\" .= vs]",
                       "schemaToValue (SArr item) = object [\"type\" .= (\"array\" :: Text), \"items\" .= schemaToValue item]",
                       "schemaToValue (SOpt s) = schemaToValue s",
                       "schemaToValue (SObj fields) = object [\"type\" .= (\"object\" :: Text), \"properties\" .= object (map (\\(k,s) -> k .= schemaToValue (innerSchema s)) fields), \"required\" .= map fst (filter (not . isOpt . snd) fields)]"] },
            ],
        }
    };
}

/// AskUser effect — single definition.
///
/// A distinct effect, decl-side only, living ALONGSIDE `Ask` (NOT a rename —
/// `ask_decl` stays load-bearing for `standard_decls()`/`llm_decl`). Presents
/// a typed form to a HUMAN OPERATOR and
/// BLOCKS until they submit — distinct from `Ask`, which suspends to the
/// CALLING LLM AGENT. Answerer-only: it rides in
/// `tidepool-harness::selfharness::driver::answerer_decls` (`[AskUser,
/// Finalize]`), not in `standard_decls()`. No `tidepool-handlers` handler —
/// harness-serviced only (same convention as `ask_effect_def!`/
/// `runllmturn_effect_def!`/`finalize_effect_def!`'s own doc comments); only
/// [`effect_decl_projection!`] consumes this definition, so the
/// `handler`/`req`/`method` slots name types that are never generated.
///
/// The typed surface a caller writes is `askUser @T` (`Tidepool.Form`, which
/// derives the form from `T`'s own `Generic` representation); `askUserRaw ::
/// Value -> M Value` here is the raw escape hatch it is built on — the ONE
/// frozen cross-agent contract name this definition exists to provide.
///
/// `NoteWith` is a SECOND constructor riding this same GADT (not a new effect
/// / union tag): a display-only channel for the answerer to post narration
/// ("here's what I'm asking and why") to the operator GUI's accumulating
/// feed, distinct from `AskUserWith`'s blocking form. It does NOT block — the
/// harness driver services it by posting the text and resuming immediately
/// with `()`, never presenting anything to the operator gate's
/// `present_form`. `noteRaw :: Text -> M ()` is the raw escape hatch;
/// `Tidepool.Form.note` is the surface an answerer turn actually calls.
#[macro_export]
macro_rules! askuser_effect_def {
    ($project:path) => {
        $project! {
            effect AskUser,
            handler AskUserHandler,
            req AskUserReq,
            decl_fn askuser_decl,
            prompt_card [
                "`choose :: [(Text, a)] -> M a` — labeled decision from (label, value) pairs; ",
                "ALWAYS prefer it for a decision, the label is the only text the operator sees. ",
                "`chooseMany :: [(Text, a)] -> M [a]` — pick a subset.\n",
                "`askUser @T :: M T` — form derived from `T`'s own shape: a record's fields ",
                "become named inputs, a SUM's constructors become the choices (nullary ",
                "constructors are direct options; a payload constructor is a selectable branch ",
                "with its fields), or a primitive (`Text`/`Int`/`Bool`); a bad submission ",
                "re-prompts internally, no `Either` to unwrap: `d <- askUser @Deploy`, ",
                "`k <- askUser @NextStep` then `case k of ...` to sequence follow-ups. A type ",
                "you define for this needs `deriving (Generic, FromJSON)`, and a sum's payload ",
                "constructors need RECORD syntax — `Other { detail :: Text }`, never ",
                "`Other Text` (positional fields have no JSON key).\n",
                "`note \"...\" :: M ()` — non-blocking narration to the operator's feed; call it ",
                "BEFORE presenting a form to explain what you're about to ask and why (it never ",
                "costs a turn).",
            ],
            description [
                "Present a typed form to a HUMAN OPERATOR and block until they submit. ",
                "`askUser @T` presents a human form and returns `T`. Define `T` using ordinary ",
                "records and constructors, derive `Generic` and `FromJSON` for it and any ",
                "nested custom types, and end fields in `Text`, `Int`, `Double`, or `Bool`. ",
                "Constructors are choices, record fields are named inputs, and `Maybe a` is ",
                "optional; a bad submission re-prompts internally, so there is no `Either` to ",
                "unwrap. The type IS the form — there is no second description of the shape to ",
                "drift from the answer type, and the submission is ordinary JSON read back by ",
                "that same generic `FromJSON`:\n",
                "  data Env = Development | Staging | Production deriving (Generic, FromJSON)\n",
                "  data Deploy = Deploy { service :: Text, env :: Env, replicas :: Int, note :: Maybe Text } deriving (Generic, FromJSON)\n",
                "  d <- askUser @Deploy   -- then read fields with record-dot: d.service, d.env\n",
                "For alternatives that ",
                "exist only as runtime values, `choose :: [(Text, a)] -> M a` and ",
                "`chooseMany :: [(Text, a)] -> M [a]` take (label, value) pairs. ",
                "`askUserRaw :: Value -> M Value` is the raw escape hatch these are ",
                "built on, carrying the form spec as JSON directly. `note :: Text -> M ()` ",
                "posts markdown-ish text to the operator's feed WITHOUT blocking — use it to ",
                "explain what you are about to ask and why, before presenting a form.",
            ],
            type_defs [],
            verbs [
                { ctor AskUserWith, method ask_user_with,
                  args { spec: "Value" as tidepool_eval::value::Value },
                  ret "Value" },
                { ctor NoteWith, method note_with,
                  args { text: "Text" as String },
                  ret "()" },
            ],
            helpers [
                { raw ["askUserRaw :: Value -> M Value",
                       "askUserRaw spec = send (AskUserWith spec)"] },
                { raw ["noteRaw :: Text -> M ()",
                       "noteRaw text = send (NoteWith text)"] },
            ],
        }
    };
}

/// ReadState effect — single definition. Decl-only (no `tidepool-handlers`
/// handler, same convention as `askuser_effect_def!`): the self-iterating
/// harness DRIVER services it — classify by constructor name, resume
/// IMMEDIATELY with the loop's durable state as JSON (`note`'s service
/// shape: no operator, no model round). Companion State v2
/// (`plans/companion-state-v2.md`): the agent computes over its own state —
/// filtering, archive search, counting — instead of reading only `render`'s
/// prose projection.
#[macro_export]
macro_rules! readstate_effect_def {
    ($project:path) => {
        $project! {
            effect ReadState,
            handler ReadStateHandler,
            req ReadStateReq,
            decl_fn readstate_decl,
            prompt_card [
                "`getStateJson :: M Value` — the loop's durable State as JSON, ",
                "immediately (no operator, no model round), as of this window's START ",
                "(this window's edit and any operator message being ingested are not in ",
                "it yet). Query with optics (`v ^? key \"memories\" . _Array`) or decode ",
                "the typed spine: `Aeson.fromJSON v :: Aeson.Result State`.",
            ],
            description [
                "Read the loop's durable State — the same value the framing renders a ",
                "SELECTION of — as JSON, immediately. `getStateJson :: M Value` returns ",
                "the state as of this cognition window's start; the current window's ",
                "edit (and any operator message being ingested this window) are not yet ",
                "in it. Use optics for ad-hoc queries, or decode the typed spine with ",
                "`Aeson.fromJSON` and compute over memories, threads, and scratch with ",
                "ordinary Haskell — filter the archive, search, count, join.",
            ],
            type_defs [],
            verbs [
                { ctor ReadStateWith, method read_state_with,
                  args { },
                  ret "Value" },
            ],
            helpers [
                { raw ["getStateJson :: M Value",
                       "getStateJson = send ReadStateWith"] },
            ],
        }
    };
}

/// RunLLMTurn effect — single definition.
///
/// Separate from [`ask_effect_def!`]: `runLLMTurn`/`runLLMTurnFork`/
/// `runLLMTurnFanout` have their own GADT/union-tag (`RunLLMTurnWith`,
/// structurally IDENTICAL to `AskWith` — same `Text -> Value -> M Value`
/// shape, same `typedSite`/`fork`/`fan`/`prompts` payload scheme) so `Ask`
/// and `RunLLMTurn` are independently interposed effects sharing the JIT's
/// one suspend-tag-threshold path (`tidepool-codegen::jit_machine::drive_effect_loop`)
/// rather than one effect doing double duty. Like `Ask`, this has no
/// `tidepool-handlers` handler — its dispatcher is server/harness machinery
/// (`tidepool-harness::engine::classify_hole`, the eval/repl servers' own
/// suspend paths) — so only [`effect_decl_projection!`] consumes this
/// definition; the `handler`/`req`/`method` slots name types that are never
/// generated (same convention as `ask_effect_def!`'s own doc comment).
#[macro_export]
macro_rules! runllmturn_effect_def {
    ($project:path) => {
        $project! {
            effect RunLLMTurn,
            handler RunLLMTurnHandler,
            req RunLLMTurnReq,
            decl_fn runllmturn_decl,
            // Every helper below is already `Member RunLLMTurn effs =>`
            // (siteid-plugin spike) rather than fixed to the closed `M`
            // stack — see the helpers' own comment. That's what makes
            // RunLLMTurn safe to include in a compile's VOCABULARY without
            // being in its ROW (extract-wave item 0b): the GADT + these
            // helpers typecheck at their DEFINITION site regardless of
            // whether `RunLLMTurn` is in the current row; `Member` alone
            // gates whether a call site can actually solve the constraint.
            helpers_row_polymorphic true,
            description [
                "Suspend for a TYPED answer. `runLLMTurn \\@T prompt` (same calling ",
                "model answers in context) / `runLLMTurnFork \\@T prompt` (a forked ",
                "sub-agent answers) / `runLLMTurnFanout \\@T prompts` (N forked ",
                "sub-agents, one per prompt, answered as a batch `[T]`) — GHC ",
                "validates the answer against `T` before it resumes the continuation ",
                "(an ill-typed answer never consumes it).",
            ],
            type_defs [],
            verbs [
                { ctor RunLLMTurnWith, method run_llm_turn_with,
                  args { prompt: "Text" as String, payload: "Value" as tidepool_eval::value::Value },
                  ret "Value" },
            ],
            helpers [
                // #R0 typed-yield pass, plans/harness-r0/10-extract-pass; split out of
                // Ask by self-iterating-harness WS-B (07-impl-orchestration.md).
                // `runLLMTurn`/`runLLMTurnFork`/`runLLMTurnFanout` are the surface
                // verbs: extract's Translate.hs intercepts their call sites (matched
                // by name, mirroring the tagToEnum# arm) and head-swaps to the hidden
                // *Sited sibling below with a fresh per-call-site literal Int
                // prepended — so the OPAQUE bodies here never actually run; they exist
                // only so GHC's typechecker accepts the call and so
                // `runLLMTurnSited`/`runLLMTurnForkSited`/
                // `runLLMTurnFanoutSited` are reachable (referencing them here is
                // what pulls their real, executing definitions into the closed
                // program). OPAQUE keeps every one of the six un-inlined and
                // un-w/w'd, so both the call-site match and the sibling lookup (by
                // name, in Translate.hs) stay stable across -O2.
                //
                // Member-polymorphic (siteid-plugin spike, GHC-verified): each verb
                // carries `Member RunLLMTurn effs =>` instead of being fixed to the
                // closed `M` stack, so a reusable helper can be written generically
                // over `effs` — `helper :: Member RunLLMTurn effs => Text -> Eff
                // effs T; helper p = runLLMTurn @T p` — and still get a distinct
                // site id per call site once specialized. This is SAFE with
                // by-name/by-arity interception because OPAQUE genuinely survives
                // -O2 (verified empirically: the closed Core still names the Var
                // `runLLMTurn` at every call site, dictionary argument and all —
                // GHC's specializer never clones or renames an OPAQUE binding). Two
                // extract-side generalizations were needed, both in Translate.hs:
                // (1) the interception arms match "first type arg, trailing N value
                // args" (`splitTrailingArgs`) instead of an exact arity, since a
                // `Member` dictionary now rides as 0+ EXTRA leading value args
                // (re-applied verbatim to the *Sited sibling, which carries the
                // identical constraint); (2) at a call site where the dictionary is
                // a statically-known top-level instance (i.e. NOT inside a still-
                // generic helper), GHC's specializer additionally wraps the call in
                // `nospec @ty (runLLMTurn @T) $dInstance prompt` to block
                // over-specialization — `stripNospecSpine` re-flattens this back
                // into one spine before interception runs, so both the
                // still-abstract (inside-a-helper) and the fully-resolved
                // (top-level, concrete-dictionary) call shapes intercept
                // identically. No GHC plugin, no `-fplugin`, no earlier Core-to-Core
                // pass was needed — the existing post-`-O2` by-name scan already
                // runs before extract does anything else with the Core, and OPAQUE
                // was already sufficient protection; the bug was purely in the
                // pattern-matching arity assumption, not in *when* the swap ran.
                { raw ["{-# OPAQUE runLLMTurn #-}",
                       "runLLMTurn :: forall a effs. Member RunLLMTurn effs => Text -> Eff effs a",
                       "runLLMTurn prompt = runLLMTurnSited 0 prompt"] },
                { raw ["{-# OPAQUE runLLMTurnFork #-}",
                       "runLLMTurnFork :: forall a effs. Member RunLLMTurn effs => Text -> Eff effs a",
                       "runLLMTurnFork prompt = runLLMTurnForkSited 0 prompt"] },
                // runLLMTurnFanout (B1, widen): one park, N thunk children — the
                // SAME classification scheme as runLLMTurnFork ("typedSite" +
                // "fork" .= True), plus an ADDITIVE "fan" count and the individual
                // per-child "prompts" (F3: additive payload fields are allowed).
                // The answer type applied here (`@a`) is the PER-CHILD element type;
                // the site's recorded asks.json type is the LIST type `[a]` (see
                // Translate.hs's fanout interception arm) — the harness derives the
                // element type back by stripping the outer `[]`.
                { raw ["{-# OPAQUE runLLMTurnFanout #-}",
                       "runLLMTurnFanout :: forall a effs. Member RunLLMTurn effs => [Text] -> Eff effs [a]",
                       "runLLMTurnFanout prompts = runLLMTurnFanoutSited 0 prompts"] },
                // The Int arg is the site id extract substitutes at the call site (the
                // literal `0` above is a placeholder, never the value that actually
                // runs). `unsafeCoerce` is safe here ONLY because extract has already
                // checked (Translate.hs's checkRunLLMTurnType) that the site's answer
                // type is monomorphic; a PURE function type is allowed (the model may
                // finalize a `State -> State`), but a type mentioning the effect monad
                // is rejected (typeMentionsEffectMonad — the generated `M`/row is
                // fragment-nominal, so an effectful answer cannot unify across
                // surfaces) — the harness resumes this suspension with a value the
                // caller validated against that exact type, so the coercion is a
                // same-representation relabeling, not a genuine type change.
                { raw ["{-# OPAQUE runLLMTurnSited #-}",
                       "runLLMTurnSited :: forall a effs. Member RunLLMTurn effs => Int -> Text -> Eff effs a",
                       "runLLMTurnSited sid p = unsafeCoerce <$> send (RunLLMTurnWith p (object [\"typedSite\" .= sid]))"] },
                { raw ["{-# OPAQUE runLLMTurnForkSited #-}",
                       "runLLMTurnForkSited :: forall a effs. Member RunLLMTurn effs => Int -> Text -> Eff effs a",
                       "runLLMTurnForkSited sid p = unsafeCoerce <$> send (RunLLMTurnWith p (object [\"typedSite\" .= sid, \"fork\" .= True]))"] },
                { raw ["{-# OPAQUE runLLMTurnFanoutSited #-}",
                       "runLLMTurnFanoutSited :: forall a effs. Member RunLLMTurn effs => Int -> [Text] -> Eff effs [a]",
                       "runLLMTurnFanoutSited sid prompts = unsafeCoerce <$> send (RunLLMTurnWith (intercalate \"\\n\" prompts) (object [\"typedSite\" .= sid, \"fork\" .= True, \"fan\" .= length prompts, \"prompts\" .= prompts]))"] },
            ],
        }
    };
}

/// Finalize effect — single definition.
///
/// The Agent-side terminal handoff: `finalize \@T x` hands a typed value UP
/// to the parent `runLLMTurn` hole and TERMINATES the current Agent turn
/// loop — it does NOT resume (unlike `Ask`/`RunLLMTurn`, whose continuation
/// the caller's answer resumes). The value crosses IN-HEAP via `run_child`
/// (no JSON round-trip), so — RELAXED from `runLLMTurn`'s rule — it may be a
/// closure or other non-serializable value; `FinalizeWith`'s payload field is
/// therefore the raw polymorphic answer type, not `Data.Aeson.Value`. Shares
/// `Ask`/`RunLLMTurn`'s suspend/classify machinery (its own GADT/union-tag,
/// caught by the same JIT suspend-tag threshold) — no duplicated plumbing,
/// only the wire shape differs where finalize's semantics genuinely differ.
/// No `tidepool-handlers` handler (harness-serviced only, same convention as
/// `Ask`/`RunLLMTurn` — see their doc comments); only
/// [`effect_decl_projection!`] consumes this definition.
#[macro_export]
macro_rules! finalize_effect_def {
    ($project:path) => {
        $project! {
            effect Finalize,
            handler FinalizeHandler,
            req FinalizeReq,
            decl_fn finalize_decl,
            // `Finalize` is TYPE-INDEXED by its answer type, exactly like
            // `State s`: the row entry a turn compiles against is `Finalize T`
            // for the hole's answer type `T`, so `Member (Finalize T) effs` —
            // the row itself — is what admits `finalize @T x`. A wrong-typed
            // answer is then a plain GHC error naming the row
            // (`'Finalize Text' is not a member of '[…, Finalize Decision]'`),
            // not a value that compiles and case-traps after crossing in-heap.
            // The type argument is per-compile (an author type like `Decision`),
            // supplied through `RowArgs`; `NoAnswer` is the default for a turn
            // that is not answering a typed hole — uninhabited, so such a turn
            // simply has no finalize capability, which is the true statement.
            type_params [v] default_row_args ["NoAnswer"],
            prompt_card [
                "`finalize @T value` — commit the typed answer and end this turn; ",
                "`value` crosses in-heap to the parent `runLLMTurn` hole.",
            ],
            description [
                "Terminate the current Agent turn loop and hand a typed value UP to ",
                "the parent `runLLMTurn` hole, in-heap (no JSON round-trip — the value ",
                "may be a closure or other non-serializable value). `finalize x` never ",
                "resumes; the harness driver reads the value directly and resolves the ",
                "parent hole via `run_child`.",
            ],
            // The uninhabited default answer type (see `type_params` above).
            type_defs ["data NoAnswer"],
            verbs [
                { ctor FinalizeWith, method finalize_with,
                  args { site: "Int" as i64, value: "v" as tidepool_eval::value::Value },
                  ret "a" },
            ],
            helpers [
                // The Int arg is the site id extract substitutes at the call site
                // (mirrors runLLMTurn's *Sited convention — the literal `0` below is
                // a placeholder, never the value that actually runs). No
                // `unsafeCoerce` here: unlike runLLMTurn's answer (which crosses as
                // `Data.Aeson.Value` and must be relabeled back to `T`), `finalize`'s
                // value is carried at its own native representation the whole way —
                // `FinalizeWith`'s payload field is `v` itself, not `Value`.
                //
                // TWO forall'd type variables, deliberately: `v` (the finalized
                // value's own type, what `@T` fixes) and `a` (finalize's "return
                // type" — it never actually returns, the send diverges via
                // suspension — left INDEPENDENT of `v` on purpose, so a caller
                // needing to satisfy some OTHER constraint downstream, e.g. a
                // `toJSON`-wrapping template around a turn that never reaches it,
                // can pin `a` separately without needing `v` itself, e.g. a
                // function type, to satisfy that constraint). `finalize @T x`
                // therefore desugars to TWO explicit Core type arguments
                // (`@T @inferred`), not one — Translate.hs's detection arm
                // matches on the first two type args, not runLLMTurn's
                // single-tyvar shape.
                //
                // `effs` (siteid-plugin spike) is a THIRD, trailing forall'd
                // tyvar — appended LAST so `finalize @T x`'s explicit `@T`
                // still binds `v`, not `effs` (visible type application binds
                // in forall-declaration order). Translate.hs's detection arm
                // only reads the first two type args and ignores any beyond
                // them, so this addition needed no change there; see the
                // RunLLMTurn helpers' comment above for the shared mechanism
                // (`splitTrailingArgs` + `stripNospecSpine`) that makes the
                // `Member` dictionary argument transparent to the head-swap.
                { raw ["{-# OPAQUE finalize #-}",
                       "finalize :: forall v a effs. Member (Finalize v) effs => v -> Eff effs a",
                       "finalize v = finalizeSited 0 v"] },
                { raw ["{-# OPAQUE finalizeSited #-}",
                       "finalizeSited :: forall v a effs. Member (Finalize v) effs => Int -> v -> Eff effs a",
                       "finalizeSited sid v = send (FinalizeWith sid v)"] },
            ],
        }
    };
}

/// Fork effect — single definition (answerer parallel-delegation surface).
///
/// The context-window fork as its own effect, distinct from `RunLLMTurn`:
/// `runLLMTurn` is suspend-and-resume (an open turn the answerer holds); a
/// fork is spawn-and-gather (a bounded fan-out with a join). Two constructors,
/// each carrying its extract-substituted site id and the child brief(s):
/// `ForkWith site brief` (one child, one typed answer) and `ForkAllWith site
/// prompts` (N children, one per prompt, answered as a typed batch). The Rust
/// layer routes a suspension by CONSTRUCTOR NAME (`engine::classify_hole`), so
/// a fork never rides a `runLLMTurn` payload key.
///
/// Decl-side only (harness-serviced, no `tidepool-handlers` handler — same
/// convention as `runllmturn_effect_def!`/`finalize_effect_def!`/
/// `askuser_effect_def!`); only [`effect_decl_projection!`] consumes this
/// definition, so the `handler`/`req`/`method` slots name types that are never
/// generated.
///
/// `forkSited`/`forkAllSited` are the executing helpers — the `send`-wrappers
/// `Tidepool.Fork`'s OPAQUE `fork`/`forkAll`/`forkMap`/`forkCata` stubs
/// head-swap to (extract resolves them by name in `Translate.hs`). They are
/// `Member Fork effs`-polymorphic so extract can re-apply a dictionary
/// argument verbatim at the swapped call site, exactly like the `runLLMTurn`
/// family. The site id extract substitutes selects the recorded answer type;
/// the `0` literal in the stub bodies is a placeholder that never runs.
#[macro_export]
macro_rules! fork_effect_def {
    ($project:path) => {
        $project! {
            effect Fork,
            handler ForkHandler,
            req ForkReq,
            decl_fn fork_decl,
            prompt_card [
                "`fork @T brief :: M T` — delegate to one sub-answerer that answers `brief` ",
                "on its own.\n",
                "`forkAll @T briefs :: M [T]` — delegate to one sub-answerer per brief, ",
                "answered together as a batch `[T]` (`import Tidepool.Fork`). A forked child ",
                "cannot itself fork.",
            ],
            description [
                "Spawn parallel sub-answerers and gather their typed answers. ",
                "`fork \\@T brief` forks ONE child that answers a single `T`; ",
                "`forkAll \\@T briefs` forks one child per brief, answered together ",
                "as `[T]` in order (`import Tidepool.Fork`). A forked child answers ",
                "its own brief directly and cannot itself fork — depth-one.",
            ],
            type_defs [],
            verbs [
                { ctor ForkWith, method fork_with,
                  args { site: "Int" as i64, brief: "Text" as String },
                  ret "Value" },
                { ctor ForkAllWith, method fork_all_with,
                  args { site: "Int" as i64, prompts: "[Text]" as Vec<String> },
                  ret "Value" },
            ],
            helpers [
                // The Int arg is the site id extract substitutes at the call
                // site (the literal `0` in Tidepool.Fork's stubs is a
                // placeholder). `unsafeCoerce` relabels the same runtime bytes
                // back to the caller's answer type — safe because extract has
                // checked (Translate.hs's checkRunLLMTurnType) the site's
                // answer type is monomorphic; a pure function type is allowed,
                // an effect-monad-mentioning type is rejected
                // (typeMentionsEffectMonad) — so the harness resumes with a
                // value the caller validated against that type.
                { raw ["{-# OPAQUE forkSited #-}",
                       "forkSited :: forall a effs. Member Fork effs => Int -> Text -> Eff effs a",
                       "forkSited sid brief = unsafeCoerce <$> send (ForkWith sid brief)"] },
                { raw ["{-# OPAQUE forkAllSited #-}",
                       "forkAllSited :: forall a effs. Member Fork effs => Int -> [Text] -> Eff effs [a]",
                       "forkAllSited sid prompts = unsafeCoerce <$> send (ForkAllWith sid prompts)"] },
            ],
        }
    };
}

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
                "`llm schema prompt` returns a Value validated against the schema ",
                "(structured output, no markdown fences). Extract with optics, e.g. ",
                "`v ^? key \"category\" . _String`.",
            ],
            type_defs [],
            // #335 typed-failure ADT. FULLY TOTAL: budget exhaustion is now DATA
            // (`LlmBudget`), not an abort — nothing in the Llm path kills the eval.
            errors LlmError [
                { ctor LlmApi,     fields { detail: "Text" as String }, doc "API/network call failure" },
                { ctor LlmRefusal, fields { detail: "Text" as String }, doc "the model declined to answer" },
                { ctor LlmBudget,  fields { },                          doc "the per-eval call budget is exhausted" },
            ],
            verbs [
                { ctor LlmStructured, method llm_structured,
                  args { prompt: "Text" as String, schema: "Value" as crate::effect_glue::JsonArg },
                  ret "Value", errors LlmError },
            ],
            helpers [
                // schemaToValue lives in ask_decl (Ask is always present).
                { raw ["-- | Call the LLM for structured output. Failure is TYPED and TOTAL",
                       "-- (#335): `Left (LlmApi _)` on an API/network failure, `Left (LlmRefusal",
                       "-- _)` on a declined answer, `Left LlmBudget` when the per-eval call budget",
                       "-- is exhausted — none of these abort the eval. Unwrap with `Right v <- llm",
                       "-- schema prompt` or `>>= liftEither`.",
                       "llm :: Schema -> Text -> M (Either LlmError Value)",
                       "llm schema prompt = send (LlmStructured prompt (schemaToValue schema))"] },
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

/// Lsp effect — single definition. (Constructor alignment padding
/// normalizes to single spaces — same one-time change as Meta.)
#[macro_export]
macro_rules! lsp_effect_def {
    ($project:path) => {
        $project! {
            effect Lsp,
            handler LspHandler,
            req LspReq,
            decl_fn lsp_decl,
            description [
                "Semantic code-graph navigation via a language server (rust-analyzer, .rs). ",
                "Everything is a LspNode {nodeName, nodeContainer, nodeKind, nodeFile, nodePos, nodeText} ",
                "(nodeLine derives from nodePos) — the currency you thread. ",
                "`lspWhere name` → all definitions of NAME (the seed, `Either LspError [LspNode]`). ",
                "Then walk the graph: `lspCallers n` / `lspCallees n` (incoming/outgoing calls), ",
                "`lspRefs n` (use sites) — each `LspNode -> M [LspNode]`, [] = none, so they chain ",
                "directly (e.g. `lspWhere \"x\" >>= liftEither >>= concatMapM lspCallers`). ",
                "`lspDef n` (any node → its definition), `lspHover n` (type/sig/docs) stay `Maybe` ",
                "(a node genuinely may lack one). `lspRename n new` (→ unified diff; review then ",
                "`applyDiff`). `lspDiags file` for a file's errors. Needs the `tidepool-lsp-daemon` ",
                "running in the workspace; a daemon-down failure surfaces as `Left (LspDaemonDown _)` ",
                "from `lspWhere`/`lspDiags`, or a structural eval abort from the walker verbs.",
            ],
            type_defs [
                "data Position = Position { posLine :: Int, posChar :: Int } deriving (Show, Eq)",
                "data LspNode = LspNode { nodeName :: Text, nodeContainer :: Text, nodeKind :: Text, nodeFile :: Text, nodePos :: Position, nodeText :: Text } deriving (Show, Eq)",
                "data Diag = Diag { diagFile :: Text, diagLine :: Int, diagSeverity :: Text, diagMessage :: Text }",
                "nodeLine :: LspNode -> Int\nnodeLine = posLine . nodePos",
                "instance ToJSON Position where\n  toJSON (Position l c) = object [\"line\" .= l, \"char\" .= c]",
                "instance ToJSON LspNode where\n  toJSON nd@(LspNode n c k f _ t) = object [\"name\" .= n, \"container\" .= c, \"kind\" .= k, \"file\" .= f, \"line\" .= nodeLine nd, \"text\" .= t]",
                "instance ToJSON Diag where\n  toJSON (Diag f l s m) = object [\"file\" .= f, \"line\" .= l, \"severity\" .= s, \"message\" .= m]",
            ],
            // #335 typed-failure ADT — MINIMAL tagging. The daemon-connection
            // failure is the only failure shared across every verb, but
            // lspDef/lspHover/lspRename already answer per-node absence via
            // `Maybe` (Nothing = "doesn't apply to this node"), so tagging
            // them too would fold two different kinds of "no" into one
            // Either. Only the SEED (`lspWhere`) and the plain-list
            // `lspDiags` — neither of which has a Maybe already — get
            // `errors LspError`. lspCallers/lspCallees/lspRefs return a plain
            // `[LspNode]` (empty = none) so they compose with `concatMapM`;
            // a daemon-down failure there is a STRUCTURAL eval abort (you
            // can't meaningfully continue a graph walk with no daemon), not
            // a silent `[]` — see `lsp_err_to_effect` in handlers/lsp.rs.
            // lspDef/lspHover/lspRename keep aborting on daemon-down too,
            // unchanged.
            errors LspError [
                { ctor LspDaemonDown, fields { detail: "Text" as String }, doc "no tidepool-lsp-daemon reachable at the workspace socket" },
            ],
            verbs [
                { ctor LspWhere, method lsp_where,
                  args { symbol: "Text" as String },
                  ret "[LspNode]", errors LspError },
                { ctor LspCallers, method lsp_callers,
                  args { n: "LspNode" as LspNode },
                  ret "[LspNode]" },
                { ctor LspCallees, method lsp_callees,
                  args { n: "LspNode" as LspNode },
                  ret "[LspNode]" },
                { ctor LspRefs, method lsp_refs,
                  args { n: "LspNode" as LspNode },
                  ret "[LspNode]" },
                { ctor LspDef, method lsp_def,
                  args { n: "LspNode" as LspNode },
                  ret "(Maybe LspNode)" },
                { ctor LspHover, method lsp_hover,
                  args { n: "LspNode" as LspNode },
                  ret "(Maybe Text)" },
                { ctor LspRename, method lsp_rename,
                  args { n: "LspNode" as LspNode, new_name: "Text" as String },
                  ret "(Maybe Text)" },
                { ctor LspDiagnostics, method lsp_diagnostics,
                  args { file: "Text" as String },
                  ret "[Diag]", errors LspError },
            ],
            helpers [
                { name lspWhere, sig "Text -> M (Either LspError [LspNode])",
                  doc ["Seed: every workspace definition named X (each a LspNode with container/file/line/source line).",
                       "`Left (LspDaemonDown _)` when the daemon isn't reachable; unwrap with `>>= liftEither`."],
                  body pointfree LspWhere },
                { name lspCallers, sig "LspNode -> M [LspNode]",
                  doc ["Incoming calls; [] = none (or node not callable). A daemon-down failure",
                       "aborts the eval structurally (not a silent []) — see the Lsp effect description."],
                  body pointfree LspCallers },
                { name lspCallees, sig "LspNode -> M [LspNode]",
                  doc ["Outgoing calls; [] = none (or node not callable)."],
                  body pointfree LspCallees },
                { name lspRefs, sig "LspNode -> M [LspNode]",
                  doc ["Use sites of this node's symbol (kind = \"reference\"); [] = none (or not a symbol)."],
                  body pointfree LspRefs },
                { name lspDef, sig "LspNode -> M (Maybe LspNode)",
                  doc ["Resolve any node (e.g. a use site) to its definition node."],
                  body pointfree LspDef },
                { name lspHover, sig "LspNode -> M (Maybe Text)",
                  doc ["Type / signature / docs for a node."],
                  body pointfree LspHover },
                { name lspRename, sig "LspNode -> Text -> M (Maybe Text)",
                  doc ["Rename a node's symbol to NEW; returns a unified diff (apply with applyDiff). Nothing = can't rename."],
                  body applied LspRename(n, new) },
                { name lspDiags, sig "FilePath -> M (Either LspError [Diag])",
                  doc ["Diagnostics (errors / warnings) for FILE. `Left (LspDaemonDown _)`",
                       "when the daemon isn't reachable; unwrap with `>>= liftEither`."],
                  body pointfree LspDiagnostics },
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
            description [
                "Persistent key-value store. State survives across calls within one server session. ",
                "Key convention: use slash-delimited namespaces (e.g. \"agent-42/foo\") to avoid ",
                "cross-agent collision. kvClear/kvKeysP operate on prefix boundaries.",
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
                { name kvGet, sig "Text -> M (Maybe Value)",
                  doc ["Look up a key; Nothing when absent."],
                  body pointfree KvGet },
                { name kvSet, sig "Text -> Value -> M ()",
                  doc ["Persist a JSON value under a key."],
                  body applied KvSet(k, v) },
                { name kvDel, sig "Text -> M ()",
                  doc ["Delete a key (no-op when absent)."],
                  body pointfree KvDelete },
                { name kvKeys, sig "M [Text]",
                  doc ["All keys (unordered; kvKeysP \"\" for sorted)."],
                  body nullary KvKeys },
                { name kvClear, sig "Text -> M Int",
                  doc ["Delete all keys whose name starts with @prefix@; return the count deleted.",
                       "Pass \"\" (empty string) to clear the ENTIRE store — this erases ALL",
                       "persisted KV data for this server session, so use with caution.",
                       "Recommended pattern: namespace keys as \"ns/key\" and clear with \"ns/\".",
                       "NOTE: per-session automatic scoping is a deferred design decision (#327);",
                       "callers manage namespaces manually via this prefix argument."],
                  body pointfree KvClear },
                { name kvKeysP, sig "Text -> M [Text]",
                  doc ["All keys whose name starts with @prefix@, returned sorted.",
                       "E.g. @kvKeysP \"agent/\"@ returns @[\"agent/bar\", \"agent/foo\", ...]@.",
                       "Pass \"\" to list ALL keys sorted (like kvKeys but deterministically ordered)."],
                  body pointfree KvKeysP },
                { name kvInfo, sig "M Value",
                  doc ["Summary of KV store state as a JSON Value:",
                       "@{count :: Int, sample :: [Text], file_size_bytes :: Int}@.",
                       "Use to inspect junk-drawer accumulation without listing all keys.",
                       "Extract fields with optics: @i <- kvInfo; i ^? key \"count\" . _Int@"],
                  body nullary KvInfo },
                { name kvCas, sig "Text -> Maybe Value -> Value -> M (Either Value ())",
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

/// Fs effect — single definition.
///
/// The helper surface is transported VERBATIM as one `raw` block per helper
/// (Fs's verbs are mostly multi-line Haskell bodies, not thin send-wrappers,
/// so the structured `body` forms don't apply — the raw escape hatch is the
/// designed answer here). Exact byte transport: the generated decl is
/// identical to the hand-written baseline.
#[macro_export]
macro_rules! fs_effect_def {
    ($project:path) => {
        $project! {
            effect Fs,
            handler FsHandler,
            req FsReq,
            decl_fn fs_decl,
            description ["Read and write files (sandboxed to server working directory)."],
            // `FileRead` is the per-file result record `readGlob` yields (records
            // over tuples). It references `FsError` from the errors block below;
            // top-level decls in the generated module are order-independent.
            type_defs [
                "data FileRead = FileRead { path :: Text, contents :: Either FsError Text } deriving (Show, Eq)",
                "instance ToJSON FileRead where\n  toJSON (FileRead p c) = object [\"path\" .= p, \"contents\" .= c]",
            ],
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
            ],
            verbs [
                { ctor FsRead, method fs_read,
                  args { path: "Text" as String },
                  ret "Text", errors FsError },
                { ctor FsWrite, method fs_write,
                  args { path: "Text" as String, content: "Text" as String },
                  ret "()", errors FsError },
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
                // CAS keeps its own `Either (Maybe Text) ()` shape: the Left is a
                // hash CONFLICT (data), not an FsError. Not errors-tagged.
                { ctor FsWriteCas, method fs_write_cas,
                  args { path: "Text" as String, expected: "Maybe Text" as Option<String>, content: "Text" as String },
                  ret "(Either (Maybe Text) ())" },
            ],
            helpers [
                { raw ["-- | Read a file. Failure is TYPED (#335): `Left (FsNotFound p)` / `Left\n-- (FsNotUtf8 p)` / `Left (FsIo _)`. The natural spelling unwraps-or-aborts with\n-- a failable bind: `Right src <- readFile path` (or `readFile path >>= liftEither`).\nreadFile :: FilePath -> M (Either FsError Text)\nreadFile = send . FsRead"] },
                { raw ["-- | Write a file (mkdir -p on the parent). `Left (FsSandbox _)` on a path\n-- escape, `Left (FsIo _)` on write failure; unwrap with `liftEither`.\nwriteFile :: FilePath -> Text -> M (Either FsError ())\nwriteFile f c = send (FsWrite f c)"] },
                { raw ["-- | Append to a file (reads then writes). Failure is TYPED (#335): a read\n-- or write failure comes back as `Left (FsError)` DATA, nothing partially\n-- applied; unwrap with `Right () <- appendFile path t` or `>>= liftEither`.\nappendFile :: FilePath -> Text -> M (Either FsError ())\nappendFile p t = do\n  er <- readFile p\n  case er of\n    Left e -> pure (Left e)\n    Right old -> writeFile p (old <> t)"] },
                { raw ["-- | List a directory. `Left (FsNotFound _)` when absent; unwrap with `liftEither`.\nlistDirectory :: FilePath -> M (Either FsError [FilePath])\nlistDirectory = send . FsListDir"] },
                { raw ["-- | TOTAL existence predicate (System.Directory semantics): False for a\n-- missing path, a directory, or a path outside the sandbox — never throws.\ndoesFileExist :: FilePath -> M Bool\ndoesFileExist p = send (FsMetadata p) <&> maybe False (\\m -> m.isFile)"] },
                { raw ["-- | TOTAL existence predicate: False for missing/non-dir/out-of-sandbox.\ndoesDirectoryExist :: FilePath -> M Bool\ndoesDirectoryExist p = send (FsMetadata p) <&> maybe False (\\m -> m.isDir)"] },
                { raw ["-- | File size in bytes, or `Nothing` if the path is missing.\ngetFileSize :: FilePath -> M (Maybe Int)\ngetFileSize p = send (FsMetadata p) <&> fmap (\\m -> m.size)"] },
                { raw ["-- | File metadata as a `FileMeta` record {size, isFile, isDir}, or `Nothing`\n-- if the path is missing/unreadable (use record-dot: `m.size`, `m.isDir`).\nfsMeta :: FilePath -> M (Maybe FileMeta)\nfsMeta = send . FsMetadata"] },
                { raw ["getCurrentDirectory :: M FilePath\ngetCurrentDirectory = do { p <- run \"pwd\" >>= liftEither; pure (T.strip p.stdout) }"] },
                { raw ["-- | Expand a glob to matching file paths. `Left (FsSandbox _)` on an empty\n-- or absolute pattern, `Left (FsNotFound _)` on a missing search root; unwrap\n-- with `Right ps <- glob pat` or `glob pat >>= liftEither`.\nglob :: FilePath -> M (Either FsError [FilePath])\nglob = send . FsGlob"] },
                { raw ["-- | Regex-search files matching a path glob. ARG ORDER: regex FIRST, glob\n-- SECOND — a path glob like \"*.rs\" goes in arg 2, not arg 1. Returns [Hit]\n-- {path, line, text} (the shared Hit shape, so it composes with\n-- hitsByFile/refs). Failure is typed: `Left (FsBadRegex _)` on a bad regex.\n-- NB regex metachars are double-escaped here (JSON x Haskell), so a literal dot\n-- needs four backslashes; the FsBadRegex detail shows the exact form.\ngrepGlob :: Text -> FilePath -> M (Either FsError [Hit])\ngrepGlob pat g = send (FsGrep pat g)"] },
                { raw ["-- | Read every file matching a glob with PER-FILE failure isolation: one\n-- `FileRead {path, contents}` per match — `contents` is `Right text` on a clean\n-- UTF-8 read, `Left err` on a per-file failure (binary / non-UTF-8, permission).\n-- One bad file (e.g. a binary swept up by a wide glob) does NOT fail the whole\n-- batch. An empty glob is rejected loudly, but a glob matching NOTHING yields\n-- `[]` silently — check `null rs` when absence itself is the signal. Recover\n-- the readable files with\n-- `[r.path | r <- rs, isRight r.contents]`, or split all outcomes with\n-- `partitionEithers (map (.contents) rs)`.\nreadGlob :: Text -> M [FileRead]\nreadGlob = send . FsReadGlob"] },
                { raw ["-- | Exact str-replace, EXACTLY-ONCE. Reports the outcome as an\n-- `UpdateOneOutcome` DATA value (never throws, mirrors `InsertAfterOutcome`):\n-- empty `old`, a missing file, `old` not found, or `old` matching 2+ places\n-- is `UpdateOneRejected` (nothing written); otherwise `UpdateOneApplied`.\n-- Pass enough surrounding text that `old` is unique. Use planUpdate to review\n-- the diff first; the full editing surface is in tidepool://edits.\nupdate :: FilePath -> Text -> Text -> M UpdateOneOutcome\nupdate path old new\n  | T.null old = pure (UpdateOneRejected \"'old' must be non-empty\" Nothing)\n  | otherwise = do\n      er <- readFile path\n      case er of\n        Left e -> pure (UpdateOneRejected (\"file not found: \" <> show e) Nothing)\n        Right src ->\n          case len (T.splitOn old src) - 1 of\n            0 -> pure (UpdateOneRejected (\"'old' not found in \" <> path) Nothing)\n            1 -> writeFile path (replace old new src) >>= liftEither >> pure UpdateOneApplied\n            n -> pure (UpdateOneRejected (\"'old' matches \" <> show n <> \" places in \" <> path <> \" (add surrounding context to disambiguate)\") (Just n))"] },
                { raw ["-- | Replace EVERY occurrence of `old` with `new`. Reports the outcome as an\n-- `UpdateAllOutcome` DATA value (never throws): empty `old`, a missing file,\n-- or zero matches is `UpdateAllRejected` (nothing written); otherwise\n-- `UpdateAllApplied` carries the replacement count.\nupdateAll :: FilePath -> Text -> Text -> M UpdateAllOutcome\nupdateAll path old new\n  | T.null old = pure (UpdateAllRejected \"'old' must be non-empty\")\n  | otherwise = do\n      er <- readFile path\n      case er of\n        Left e -> pure (UpdateAllRejected (\"file not found: \" <> show e))\n        Right src ->\n          let n = len (T.splitOn old src) - 1\n          in if n == 0\n               then pure (UpdateAllRejected (\"'old' not found in \" <> path))\n               else writeFile path (replace old new src) >>= liftEither >> pure (UpdateAllApplied n)"] },
                { raw ["-- | Dry-run `update`: returns an `UpdateOutcome` (the review diff, or the\n-- reason it can't apply), writes NOTHING. Never errors — the conflict comes\n-- back as data so you can branch before committing.\nplanUpdate :: FilePath -> Text -> Text -> M UpdateOutcome\nplanUpdate path old new = do\n  er <- readFile path\n  case er of\n    Left e -> pure (UpdateRejected (\"file not found: \" <> show e) Nothing)\n    Right src ->\n      let n = if T.null old then 0 else len (T.splitOn old src) - 1\n      in if T.null old then pure (UpdateRejected \"'old' must be non-empty\" Nothing)\n         else if n == 0 then pure (UpdateRejected \"not found\" Nothing)\n         else if n > 1 then pure (UpdateRejected \"ambiguous\" (Just n))\n         else case Patch.genPatch path src (replace old new src) of\n                Left _ -> pure UpdateNoChange\n                Right fp -> pure (UpdateDiff (Patch.renderPatch [fp]))"] },
                { raw ["-- | `update` from the input lane: {file, old, new} (for big/quote-heavy\n-- fragments). Reports the outcome as an `UpdateOneOutcome` DATA value (never\n-- throws, same contract as `update`): a malformed payload (missing or\n-- non-string file/old/new key) is `UpdateOneRejected` — one bad item never\n-- aborts a batch.\nupdateJ :: Value -> M UpdateOneOutcome\nupdateJ v = case (v ^? key \"file\" . _String, v ^? key \"old\" . _String, v ^? key \"new\" . _String) of\n  (Just f, Just o, Just n) -> update f o n\n  _ -> pure (UpdateOneRejected \"updateJ: need {file, old, new} strings in input\" Nothing)"] },
                { raw ["-- | Insert a block after the unique line containing `anchor`. Reports the\n-- outcome as an `InsertAfterOutcome` DATA value (never throws): a missing\n-- file, or an anchor matching zero or 2+ lines, is `InsertAfterRejected`\n-- (nothing written); otherwise `InsertAfterApplied`.\ninsertAfter :: FilePath -> Text -> Text -> M InsertAfterOutcome\ninsertAfter path anchor block = do\n  er <- readFile path\n  case er of\n    Left e -> pure (InsertAfterRejected (\"file not found: \" <> show e) Nothing)\n    Right src ->\n      let ls = lines src\n          n = len (filter (isInfixOf anchor) ls)\n      in case n of\n           1 -> writeFile path (unlines (concatMap (\\l -> if anchor `isInfixOf` l then [l, block] else [l]) ls))\n                  >>= liftEither >> pure InsertAfterApplied\n           _ -> pure (InsertAfterRejected (\"anchor matched \" <> show n <> \" lines in \" <> path) (Just n))"] },
                { raw ["-- | Compute-check-commit: write only if every named check holds; failures\n-- come back as a `WriteOutcome` (nothing written on failure).\nwriteChecked :: FilePath -> [(Text, Bool)] -> Text -> M WriteOutcome\nwriteChecked path checks content = do\n  let failed = [name | (name, ok) <- checks, not ok]\n  if null failed\n    then writeFile path content >>= liftEither >> pure (Written path (length checks))\n    else pure (WriteBlocked path failed)"] },
                { raw ["-- | Blake3 content hash (hex) of a file, or Nothing if it does not exist.\n-- The compare-and-swap token for writeCheckedIf: read it, compute your new\n-- content, then write back only if the file still hashes the same.\nfileHash :: FilePath -> M (Maybe Text)\nfileHash p = send (FsHash p) >>= liftEither"] },
                { raw ["-- | Content-hash compare-and-swap write (#330). Writes CONTENT only if the\n-- file's current blake3 hash equals EXPECTED (Nothing = expect the file ABSENT,\n-- i.e. create-only). The compare-and-write is atomic within the handler, closing\n-- the lost-update race between parallel agents. Returns a WriteOutcome: 'Written'\n-- on success, or 'WriteConflict' (carrying expected vs actual hash) if the\n-- precondition failed — conflicts come back as DATA, nothing is written. Get\n-- EXPECTED from fileHash; on a conflict re-read, recompute, and retry.\nwriteCheckedIf :: Maybe Text -> FilePath -> Text -> M WriteOutcome\nwriteCheckedIf expected path content = do\n  r <- send (FsWriteCas path expected content)\n  pure $ case r of\n    Right () -> Written path 1\n    Left actual -> WriteConflict path expected actual"] },
            ],
        }
    };
}

/// Worktree effect — single definition (PRD 19, lane L4).
///
/// The authored surface is `haskell/lib/Tidepool/Worktree.hs`, which was
/// written FIRST and re-exports every name below from the generated
/// `Tidepool.Effects`. That module is frozen: this definition exists to satisfy
/// its import list exactly, not to shape it.
///
/// **Identity types are `data`, not `newtype`, and not type synonyms.** PRD 19
/// states that `EventId` is opaque runtime identity while `GitOid` is domain
/// data, and that they are different kinds of thing. A synonym to `Text` would
/// make them the same thing and let a git OID be passed where a worktree id is
/// wanted; a `newtype` is erased in Core, so the Rust `ToCore` side would build
/// a one-field `Con` the Haskell side no longer has. `data` is the spelling
/// that survives both.
///
/// **`WorktreeReceipt`'s id field is `treeId`, not the PRD snippet's
/// `worktreeId`.** The PRD's own public surface also requires
/// `worktreeId :: WorktreeHandle -> WorktreeId` as a standalone function, and a
/// record field selector and a top-level function of the same name are an
/// ambiguous occurrence at the export. The function is the one the PRD pins by
/// signature, so the field yielded. Access is `r.treeId` — record-dot, per the
/// eval records rule.
#[macro_export]
macro_rules! worktree_effect_def {
    ($project:path) => {
        $project! {
            effect Worktree,
            handler WorktreeHandler,
            req WorktreeReq,
            decl_fn worktree_decl,
            description [
                "Managed git worktrees: create an isolated worktree from a clean source ",
                "(or, explicitly, from a dirty-source snapshot), look a retained one back ",
                "up by durable id, and list what exists. Retain-first — there is no ",
                "release or delete verb in v1, deliberately. Git WORKFLOW (rebase, merge, ",
                "cherry-pick, conflict resolution) is absent by design: that work belongs ",
                "to coding agents with their native tools, and Tidepool observes what the ",
                "repository became through `Tidepool.Event`.",
            ],
            type_defs [
                "data WorktreeId = WorktreeId Text deriving (Show, Eq)",
                "data GitOid = GitOid Text deriving (Show, Eq)",
                "data GitRef = GitRef Text deriving (Show, Eq)",
                "data BranchName = BranchName Text deriving (Show, Eq)",
                "data WorktreeSource = SourceCurrentRepository | SourceRef GitRef | SourceWorktree WorktreeId deriving (Show, Eq)",
                "data DirtyPolicy = RequireClean | AllowDirtySnapshot deriving (Show, Eq)",
                "data WorktreeSpec = WorktreeSpec { specSource :: WorktreeSource, specLabel :: Text, specDirtyPolicy :: DirtyPolicy } deriving (Show, Eq)",
                "data InProgressKind = InProgressMerge | InProgressRebase | InProgressCherryPick | InProgressRevert | InProgressBisect deriving (Show, Eq)",
                "data DirtySummary = DirtySummary { staged :: [Text], unstaged :: [Text], untracked :: [Text], ignoredExcluded :: Int } deriving (Show, Eq)",
                "data GitFailureReceipt = GitFailureReceipt { gitArgs :: [Text], gitCwd :: Text, gitExitCode :: Maybe Int, gitStdout :: Text, gitStderr :: Text } deriving (Show, Eq)",
                "data WorktreeReceipt = WorktreeReceipt { treeId :: WorktreeId, cwd :: Text, branch :: BranchName, sourceHead :: GitOid, snapshotRef :: Maybe GitRef, createdAt :: Int } deriving (Show, Eq)",
                "data WorktreeHandle = WorktreeHandle { handleReceipt :: WorktreeReceipt } deriving (Show, Eq)",
                "data WorktreeSummary = WorktreeSummary { summaryReceipt :: WorktreeReceipt, present :: Bool } deriving (Show, Eq)",
                // The `errors` block templates a `ToJSON` instance for
                // `WorktreeError`, so every type reachable from one of its
                // fields needs one. These are hand-written rather than derived
                // because the vendored `ToJSON`'s generic default only covers
                // single-constructor records — `InProgressKind` is
                // multi-constructor and the identity types are positional.
                // The identity types render as their bare payload: a receipt
                // reader wants the id, not a wrapper object.
                "instance ToJSON WorktreeId where toJSON (WorktreeId t) = toJSON t",
                "instance ToJSON GitOid where toJSON (GitOid t) = toJSON t",
                "instance ToJSON GitRef where toJSON (GitRef t) = toJSON t",
                "instance ToJSON BranchName where toJSON (BranchName t) = toJSON t",
                "instance ToJSON InProgressKind where toJSON k = toJSON (show k)",
                "instance ToJSON DirtySummary where toJSON d = object [\"staged\" .= d.staged, \"unstaged\" .= d.unstaged, \"untracked\" .= d.untracked, \"ignoredExcluded\" .= d.ignoredExcluded]",
                "instance ToJSON GitFailureReceipt where toJSON r = object [\"args\" .= r.gitArgs, \"cwd\" .= r.gitCwd, \"exitCode\" .= r.gitExitCode, \"stdout\" .= r.gitStdout, \"stderr\" .= r.gitStderr]",
            ],
            // Typed per-verb failure (#335): a dirty source, a lost tree, or a
            // busy worktree is DATA an author cases on, not an eval abort.
            // These are PRD 19's `WorktreeError` variants plus the two the
            // PRD's prose requires but its illustrative ADT did not spell out
            // (see `tidepool-worktree/src/error.rs` for the argument).
            errors WorktreeError [
                { ctor SourceDirty, fields { dirty: "DirtySummary" as tidepool_bridge_effects::WtDirtySummary },
                  doc "source working tree has uncommitted state and the spec did not opt into a snapshot" },
                { ctor NotARepository, fields { path: "Text" as String },
                  doc "the path is not inside a git repository" },
                { ctor WorktreeLost, fields { lostId: "WorktreeId" as tidepool_bridge_effects::WtWorktreeId },
                  doc "registered but gone from disk; never silently recreated" },
                { ctor DirtySubmoduleUnsupported, fields { submodule: "Text" as String },
                  doc "a dirty submodule in the source; v1 refuses rather than capturing a gitlink it did not follow" },
                { ctor SourceOperationInProgress, fields { inProgress: "InProgressKind" as tidepool_bridge_effects::WtInProgressKind },
                  doc "source is mid-merge / mid-rebase / mid-cherry-pick" },
                { ctor WorktreeBusy, fields { busyId: "WorktreeId" as tidepool_bridge_effects::WtWorktreeId, holder: "Text" as String },
                  doc "one worktree, one agent — binding a second fails explicitly" },
                { ctor GitFailure, fields { receipt: "GitFailureReceipt" as tidepool_bridge_effects::WtGitFailureReceipt },
                  doc "git itself failed; the receipt carries the invocation and its output" },
                // The storage-error lane (L6) added three domain variants after
                // this block was first written. They get wire variants of their
                // own rather than being folded into an existing one, because
                // each names a failure an author would act on DIFFERENTLY, and
                // `tidepool-worktree/src/error.rs` makes the argument itself:
                // collapsing a distinct failure into a neighbour "hides the
                // second behind the first". `InvalidRegistryRoot` and
                // `StorageFailure` in particular are not `GitFailure` — no git
                // process runs in either — and spelling them as one would send
                // an operator reading a git receipt that does not exist.
                { ctor WorktreeNotRegistered, fields { notRegisteredId: "WorktreeId" as tidepool_bridge_effects::WtWorktreeId },
                  doc "no worktree registered under this id — a typo or a stale id, DISTINCT from WorktreeLost's data loss" },
                { ctor InvalidRegistryRoot, fields { root: "Text" as String, inside: "Text" as String },
                  doc "the registry root resolves inside a git working tree; it must live outside every source repository" },
                { ctor StorageFailure, fields { storagePath: "Text" as String, storageDetail: "Text" as String },
                  doc "I/O failure against Tidepool's own registry / binding table / journal" },
            ],
            verbs [
                { ctor WorktreeCreate, method worktree_create,
                  args { spec: "WorktreeSpec" as tidepool_bridge_effects::WtWorktreeSpec },
                  ret "WorktreeHandle", errors WorktreeError },
                { ctor WorktreeLookup, method worktree_lookup,
                  args { treeId: "WorktreeId" as tidepool_bridge_effects::WtWorktreeId },
                  ret "WorktreeHandle", errors WorktreeError },
                { ctor WorktreeList, method worktree_list,
                  args { },
                  ret "[WorktreeSummary]", errors WorktreeError },
                { ctor WorktreeBranchOf, method worktree_branch_of,
                  args { treeId: "WorktreeId" as tidepool_bridge_effects::WtWorktreeId },
                  ret "BranchName", errors WorktreeError },
                // A FRESH git read of the worktree's current HEAD. Deliberately
                // NOT the handle's recorded `sourceHead` (the seed the branch
                // was rooted at) and NOT the monitor's last-observed baseline:
                // the entire point is to see what the monitor did not. See the
                // helper's docs for the gap it exists to close.
                { ctor WorktreeHeadOf, method worktree_head_of,
                  args { treeId: "WorktreeId" as tidepool_bridge_effects::WtWorktreeId },
                  ret "GitOid", errors WorktreeError },
            ],
            helpers [
                { raw ["-- | Seed a managed worktree from the repository Tidepool is running",
                       "-- against. Clean-by-default: a dirty source is REFUSED unless the spec",
                       "-- is passed through 'allowDirtySnapshot'.",
                       "fromCurrentRepository :: Text -> WorktreeSpec",
                       "fromCurrentRepository lbl = WorktreeSpec SourceCurrentRepository lbl RequireClean"] },
                { raw ["-- | Seed from an explicit ref (branch, tag, remote ref, or raw OID).",
                       "fromRef :: GitRef -> Text -> WorktreeSpec",
                       "fromRef r lbl = WorktreeSpec (SourceRef r) lbl RequireClean"] },
                { raw ["-- | Seed from another managed worktree's current HEAD. This is how a",
                       "-- reviewer gets its own isolated tree off the branch it is reviewing.",
                       "fromWorktree :: WorktreeHandle -> Text -> WorktreeSpec",
                       "fromWorktree h lbl = WorktreeSpec (SourceWorktree (worktreeId h)) lbl RequireClean"] },
                { raw ["-- | Opt IN to snapshotting a dirty source. Spelled at the call site so a",
                       "-- reader of the resident can see that a synthetic commit was taken; it",
                       "-- never alters the source branch, HEAD, index, or working-tree bytes.",
                       "allowDirtySnapshot :: WorktreeSpec -> WorktreeSpec",
                       "allowDirtySnapshot s = s { specDirtyPolicy = AllowDirtySnapshot }"] },
                { raw ["-- | Create a managed worktree. `Left (SourceDirty summary)` when the",
                       "-- source is dirty and the spec did not opt in; case-match the error",
                       "-- rather than unwrapping if you mean to handle it.",
                       "createWorktree :: WorktreeSpec -> M (Either WorktreeError WorktreeHandle)",
                       "createWorktree = send . WorktreeCreate"] },
                { raw ["-- | Look a retained worktree up by durable id. Survives restart:",
                       "-- resolution reads on-disk registry state, not process memory.",
                       "-- `Left (WorktreeLost i)` when it is registered but gone from disk.",
                       "lookupWorktree :: WorktreeId -> M (Either WorktreeError WorktreeHandle)",
                       "lookupWorktree = send . WorktreeLookup"] },
                { raw ["-- | Every registered worktree, present or lost. A lost tree is listed",
                       "-- with `present = False` rather than failing the whole listing.",
                       "listWorktrees :: M [WorktreeSummary]",
                       "listWorktrees = send WorktreeList >>= liftEither"] },
                { raw ["-- | The managed branch this worktree is on, read fresh from git.",
                       "worktreeBranch :: WorktreeHandle -> M BranchName",
                       "worktreeBranch h = send (WorktreeBranchOf (worktreeId h)) >>= liftEither"] },
                { raw ["-- | This worktree's CURRENT @HEAD@, read fresh from git right now.",
                       "--",
                       "-- Deliberately none of the three things it could be confused with: it",
                       "-- is not the handle's recorded @sourceHead@ (the commit the managed",
                       "-- branch was rooted at), and it is not the event monitor's",
                       "-- last-observed baseline.  The whole purpose is to see what the",
                       "-- monitor did NOT.",
                       "--",
                       "-- It exists for the gap a resident spanning cycles has to close",
                       "-- itself.  A subscription never replays, and it lives only for its",
                       "-- cycle, so @HEAD@ can move after one cycle unregisters and before the",
                       "-- next one registers.  A resident closes that window in ORDINARY",
                       "-- AUTHORED CODE: compare @worktreeHead tree@ against the head it",
                       "-- checkpointed, act on any difference, and only then register live",
                       "-- reactions with 'withHandler'.",
                       "--",
                       "-- That is a reinforcement of no-replay, not a loophole in it.  The",
                       "-- journal stays diagnostic rather than quietly becoming a callback",
                       "-- replay mechanism, because the resident — which knows what it already",
                       "-- acted on — decides what the gap meant, rather than the runtime",
                       "-- guessing on its behalf.",
                       "worktreeHead :: WorktreeHandle -> M GitOid",
                       "worktreeHead h = send (WorktreeHeadOf (worktreeId h)) >>= liftEither"] },
                { raw ["-- | The durable identity of a managed worktree. Pure: the handle",
                       "-- already carries its receipt, so this reads no git state.",
                       "worktreeId :: WorktreeHandle -> WorktreeId",
                       "worktreeId h = h.handleReceipt.treeId"] },
                { raw ["renderWorktreeId :: WorktreeId -> Text",
                       "renderWorktreeId (WorktreeId t) = t"] },
                { raw ["renderGitOid :: GitOid -> Text",
                       "renderGitOid (GitOid t) = t"] },
                { raw ["renderBranchName :: BranchName -> Text",
                       "renderBranchName (BranchName t) = t"] },
                { raw ["-- | A one-line, operator-readable rendering of a worktree failure.",
                       "-- Case-match the constructor when you mean to BRANCH on the failure;",
                       "-- this is for receipts and logs.",
                       "renderWorktreeError :: WorktreeError -> Text",
                       "renderWorktreeError (SourceDirty d) = \"source repository is dirty: \" <> show (length d.staged) <> \" staged, \" <> show (length d.unstaged) <> \" unstaged, \" <> show (length d.untracked) <> \" untracked\"",
                       "renderWorktreeError (NotARepository p) = \"not a git repository: \" <> p",
                       "renderWorktreeError (WorktreeLost i) = \"managed worktree \" <> renderWorktreeId i <> \" is registered but missing on disk\"",
                       "renderWorktreeError (DirtySubmoduleUnsupported p) = \"dirty submodule is unsupported in v1: \" <> p",
                       "renderWorktreeError (SourceOperationInProgress k) = \"source repository has an operation in progress: \" <> show k",
                       "renderWorktreeError (WorktreeBusy i holder) = \"worktree \" <> renderWorktreeId i <> \" is already bound to agent \" <> holder",
                       "renderWorktreeError (GitFailure r) = \"git \" <> T.intercalate \" \" r.gitArgs <> \" failed: \" <> T.strip r.gitStderr",
                       "renderWorktreeError (WorktreeNotRegistered i) = \"no managed worktree registered with id \" <> renderWorktreeId i",
                       "renderWorktreeError (InvalidRegistryRoot root inside) = \"registry root \" <> root <> \" resolves inside the git working tree at \" <> inside <> \" — the registry must live outside every source repository\"",
                       "renderWorktreeError (StorageFailure p d) = \"tidepool storage failure at \" <> p <> \": \" <> d"] },
            ],
        }
    };
}

/// Repository-event effect — single definition (PRD 19, lane L4).
///
/// The authored surface is `haskell/lib/Tidepool/Event.hs`, frozen and written
/// first. The GADT is named `RepoEvent` rather than `Event` because `Event a`
/// is the authored DESCRIPTION type this definition also declares, and two
/// types cannot share a name.
///
/// ## `withHandler` is an interposition, not a send-wrapper
///
/// The mechanism, its verification, and the alternatives rejected are written
/// up in `plans/post-restart/worktree-lanes/L4-mechanism.md`. In short: `Eff` is
/// freer-simple's free monad, so a scope can WALK the computation it encloses
/// and interpose an action before every effect that computation performs. The
/// author's handler closure is therefore applied by ordinary Haskell
/// application, inside the resident's own continuation. It never crosses to
/// Rust, is never rooted by Rust, and needs no closure-application entry point
/// on the parked path — `tidepool-codegen` is untouched by this lane.
///
/// Every authored semantic falls out of that shape rather than being enforced:
/// a subscription cannot re-enter its own handler because `pumpEff` recurses on
/// the BODY only (the tick's own effects are not pumped by its own pump);
/// ordering is Haskell's own sequencing over a FIFO drain; a handler that
/// suspends is an ordinary suspension of the resident; a handler that fails
/// fails the enclosing scope by ordinary means.
///
/// ## `nextEvent` / `after` (PRD 20, S1-L3) — the blocking sibling
///
/// `RepoEventAwait` is `RepoEventDrain` plus a timeout: it BLOCKS at the
/// handler (loop of reconcile-pass / check-queue / bounded sleep) until the
/// subscription has queued something or the deadline elapses, and an elapsed
/// deadline is an EMPTY batch — typed data, not an `EventError`. `nextEvent`
/// is subscribe → block-await → unsubscribe, the one-shot sibling of
/// `withHandler` sharing its registry, no-replay rule, queue bound, and
/// loud-overflow semantics, but with no handler callback and no
/// caller-supplied timeout of its own.
///
/// Deadlines ride the SAME registry as repository watches: `WatchDeadline`
/// carries a RELATIVE millisecond duration, fixed to an absolute deadline by
/// the RUNTIME at `subscribe()` time — `after` itself is pure data
/// construction, no effect, no row dependency (unlike
/// `subagent_effect_def!`'s genuine `Worktree` requirement, `RepoEvent`
/// already ships in rows with no `Time` handler, and `after` must not break
/// them) — and fires exactly one `Tick`, queued directly onto the one
/// subscription that armed it, never broadcast, so
/// `nextEvent (someEvent <|> after ms)` is an ordinary select with a timeout
/// branch.
#[macro_export]
macro_rules! event_effect_def {
    ($project:path) => {
        $project! {
            effect RepoEvent,
            handler RepoEventHandler,
            req RepoEventReq,
            decl_fn event_decl,
            description [
                "Typed repository events. `commit tree` and `headChanged tree` are event ",
                "DESCRIPTIONS — values you can build, `fmap`, and merge with `<|>` before ",
                "anything is registered. `withHandler event handler body` makes one live ",
                "for exactly the extent of its lexical body: it registers without ",
                "blocking, never replays events older than the registration, invokes the ",
                "handler in the SAME effect row as the surrounding code (so it may send a ",
                "typed message, spawn a reviewer, or ask the operator — and may itself ",
                "suspend), runs one handler at a time per subscription with later ",
                "observations queued in observation order, and on exit closes intake, ",
                "drains, then unregisters. Handler failure fails the enclosing scope. ",
                "Queue overflow fails loudly — commits are never silently dropped. ",
                "`nextEvent event` blocks until the FIRST matching observation (or ",
                "forever): subscribe, block-await, unsubscribe — the one-shot sibling ",
                "of `withHandler`, no caller-supplied timeout. `after ms` is a ",
                "one-shot deadline event, `ms` milliseconds from the moment it is ",
                "SUBSCRIBED (not from the `after` call itself), that fires exactly one ",
                "`Tick`, so `nextEvent (someEvent <|> after ms)` reads as an ordinary ",
                "select with a timeout branch.",
            ],
            type_defs [
                "data EventId = EventId Int deriving (Show, Eq)",
                "data SubscriptionId = SubscriptionId Int deriving (Show, Eq)",
                // What the runtime watches. One entry per (worktree, kind) pair;
                // `<|>` concatenates, so a merged Event is ONE subscription over
                // several watches rather than several subscriptions.
                // `WatchDeadline` carries a RELATIVE millisecond duration: the
                // runtime fixes the absolute deadline at `subscribe()` time
                // (`now + ms`), so `after` itself needs no effect of its own.
                "data Watch = WatchCommit WorktreeId | WatchHead WorktreeId | WatchDeadline Int deriving (Show, Eq)",
                "data HeadChangeKind = Advanced [GitOid] | Amended GitOid GitOid | Rewritten [(GitOid, GitOid)] | Rewound | Switched | UnknownChange deriving (Show, Eq)",
                "data HeadChangeReceipt = HeadChangeReceipt { headWorktree :: WorktreeId, oldHead :: Maybe GitOid, newHead :: GitOid, kind :: HeadChangeKind, headBranch :: Maybe BranchName, observedAtMs :: Int } deriving (Show, Eq)",
                "data CommitReceipt = CommitReceipt { commitWorktree :: WorktreeId, oid :: GitOid, parents :: [GitOid], subject :: Text, author :: Text, committedAtMs :: Int, files :: [Text] } deriving (Show, Eq)",
                // The payload a fired deadline watch delivers. `firedAtMs` is the
                // wall-clock moment the runtime observed it as due.
                "data Tick = Tick { firedAtMs :: Int } deriving (Show, Eq)",
                // The wire shape one reconciled fact crosses as. A normal commit
                // produces one of each SHARING an EventId — that sharing is how a
                // consumer tells "two views of one change" from "two changes", so
                // the id rides on the wire rather than being minted per view.
                // `ObservedTick` is never broadcast — it is queued directly onto
                // the one subscription that armed the deadline.
                "data RepositoryEvent = ObservedCommit EventId CommitReceipt | ObservedHeadChange EventId HeadChangeReceipt | ObservedTick EventId Tick deriving (Show, Eq)",
                "data Observed a = Observed { eventId :: EventId, value :: a } deriving (Show, Eq)",
                // An Event is a DESCRIPTION: what to watch, plus how to project a
                // raw observation into the author's type. Keeping the projection
                // in the value is what makes Event a lawful Functor and lets `<|>`
                // merge two sources into ONE subscription.
                "data Event a = Event { eventWatches :: [Watch], eventProject :: RepositoryEvent -> Maybe a }",
                "instance Functor Event where fmap f e = Event e.eventWatches (\\r -> fmap f (e.eventProject r))",
            ],
            errors EventError [
                { ctor EventQueueOverflow, fields { overflowSub: "Int" as i64, dropped: "Int" as i64 },
                  doc "the per-subscription queue bound was exceeded — the scope fails rather than dropping commits" },
                { ctor EventUnknownSubscription, fields { unknownSub: "Int" as i64 },
                  doc "no such live subscription (already unregistered)" },
                { ctor EventSourceLost, fields { lostDetail: "Text" as String },
                  doc "a watched worktree is no longer observable" },
                { ctor EventSourceFailed, fields { failedDetail: "Text" as String },
                  doc "reconciliation against git failed" },
            ],
            verbs [
                { ctor RepoEventSubscribe, method repo_event_subscribe,
                  args { watches: "[Watch]" as Vec<tidepool_bridge_effects::EvWatch> },
                  ret "SubscriptionId", errors EventError },
                { ctor RepoEventDrain, method repo_event_drain,
                  args { subscription: "SubscriptionId" as tidepool_bridge_effects::EvSubscriptionId },
                  ret "[RepositoryEvent]", errors EventError },
                // BLOCKS at the handler until the subscription has >= 1
                // observation or `timeoutMs` elapses (negative == no
                // deadline). An elapsed timeout is an EMPTY batch, never an
                // error — the same typed distinction `RepoEventDrain` already
                // makes between "nothing yet" and a real failure, just with a
                // deadline attached. Poison/overflow semantics are unchanged.
                { ctor RepoEventAwait, method repo_event_await,
                  args { subscription: "SubscriptionId" as tidepool_bridge_effects::EvSubscriptionId, timeoutMs: "Int" as i64 },
                  ret "[RepositoryEvent]", errors EventError },
                { ctor RepoEventUnsubscribe, method repo_event_unsubscribe,
                  args { subscription: "SubscriptionId" as tidepool_bridge_effects::EvSubscriptionId },
                  ret "()", errors EventError },
            ],
            helpers [
                { raw ["-- | Commits observed in a managed worktree — the high-signal semantic",
                       "-- checkpoint (normal commit, merge, cherry-pick, or amend). For review,",
                       "-- test, and receipt reactions.",
                       "commit :: WorktreeHandle -> Event (Observed CommitReceipt)",
                       "commit h = Event [WatchCommit (worktreeId h)] (projectCommit (worktreeId h))"] },
                { raw ["projectCommit :: WorktreeId -> RepositoryEvent -> Maybe (Observed CommitReceipt)",
                       "projectCommit w (ObservedCommit eid r) = if r.commitWorktree == w then Just (Observed eid r) else Nothing",
                       "projectCommit _ _ = Nothing"] },
                { raw ["-- | Observed movement of a worktree's HEAD — advance, amend,",
                       "-- rebase/rewrite, reset, or checkout. This is the dependency-propagation",
                       "-- signal: children want a rebase poke even when their parent was itself",
                       "-- rebased. Observations are COALESCED state deltas, not a movement log.",
                       "headChanged :: WorktreeHandle -> Event (Observed HeadChangeReceipt)",
                       "headChanged h = Event [WatchHead (worktreeId h)] (projectHead (worktreeId h))"] },
                { raw ["projectHead :: WorktreeId -> RepositoryEvent -> Maybe (Observed HeadChangeReceipt)",
                       "projectHead w (ObservedHeadChange eid r) = if r.headWorktree == w then Just (Observed eid r) else Nothing",
                       "projectHead _ _ = Nothing"] },
                { raw ["-- | Merge two same-typed sources into ONE subscription: observations",
                       "-- from either. Subscriptions repeat for their lexical lifetime — this",
                       "-- is not one-shot. Combine with 'fmap' to keep heterogeneous selection",
                       "-- typed: `fmap Left (commit a) <|> fmap Right (headChanged b)`.",
                       "infixl 3 <|>",
                       "(<|>) :: Event a -> Event a -> Event a",
                       "l <|> r = Event (l.eventWatches ++ r.eventWatches) (\\o -> case l.eventProject o of { Just a -> Just a; Nothing -> r.eventProject o })"] },
                // The interposition. `pumpEff` recurses on the BODY only, never on
                // the tick — that asymmetry IS the one-handler-at-a-time guarantee
                // for a subscription, and it is structural rather than enforced.
                { raw ["-- | Run `tick` before every effect `body` performs. The scoped",
                       "-- interposition `withHandler` is built from; see",
                       "-- plans/post-restart/worktree-lanes/L4-mechanism.md.",
                       "pumpEff :: Eff effs () -> Eff effs a -> Eff effs a",
                       "pumpEff _ (Val a) = Val a",
                       "pumpEff tick (E u q) = tick >> E u (tsingleton (\\x -> pumpEff tick (qApp q x)))"] },
                { raw ["-- | Drain everything this subscription has observed since the last",
                       "-- drain, applying the handler to each match in OBSERVATION ORDER.",
                       "-- A queue overflow aborts here rather than dropping a commit.",
                       "drainSubscription :: Event a -> (a -> M ()) -> SubscriptionId -> M ()",
                       "drainSubscription ev handler sub = do",
                       "  batch <- send (RepoEventDrain sub) >>= liftEither",
                       "  mapM_ (\\o -> case ev.eventProject o of { Just a -> handler a; Nothing -> pure () }) batch"] },
                { raw ["-- | `withHandler event handler body` registers atomically, runs `body`",
                       "-- with the handler live, and on exit closes intake, drains what was",
                       "-- already observed, and unregisters. Registration does not block, and",
                       "-- the subscription NEVER replays events older than itself.",
                       "--",
                       "-- The handler runs in the surrounding `M` row: it may send a typed",
                       "-- message, spawn a reviewer, ask the operator, or record a receipt,",
                       "-- and it may itself suspend. Its failure fails this scope.",
                       "withHandler :: Event a -> (a -> M ()) -> M b -> M b",
                       "withHandler ev handler body = do",
                       "  sub <- send (RepoEventSubscribe ev.eventWatches) >>= liftEither",
                       "  r <- pumpEff (drainSubscription ev handler sub) body",
                       "  drainSubscription ev handler sub",
                       "  send (RepoEventUnsubscribe sub) >>= liftEither",
                       "  pure r"] },
                { raw ["-- | Block until `sub` has queued at least one observation, or",
                       "-- `timeoutMs` elapses (negative blocks with no deadline). An elapsed",
                       "-- timeout is an EMPTY list — distinguishable from a real batch, never",
                       "-- an error; poison/source-loss still fail via the `Either`.",
                       "awaitSubscriptionRaw :: SubscriptionId -> Int -> M (Either EventError [RepositoryEvent])",
                       "awaitSubscriptionRaw sub timeoutMs = send (RepoEventAwait sub timeoutMs)"] },
                { raw ["eventIdOf :: RepositoryEvent -> EventId",
                       "eventIdOf (ObservedCommit eid _) = eid",
                       "eventIdOf (ObservedHeadChange eid _) = eid",
                       "eventIdOf (ObservedTick eid _) = eid"] },
                { raw ["-- | The first batch entry `ev` projects, paired with its own EventId,",
                       "-- in observation order.",
                       "firstMatch :: Event a -> [RepositoryEvent] -> Maybe (Observed a)",
                       "firstMatch _ [] = Nothing",
                       "firstMatch ev (o:os) = case ev.eventProject o of",
                       "  Just a -> Just (Observed (eventIdOf o) a)",
                       "  Nothing -> firstMatch ev os"] },
                { raw ["-- | Block until `ev` produces its first matching observation:",
                       "-- subscribe, block-await, then ALWAYS unsubscribe. The one-shot",
                       "-- sibling of `withHandler` — same registry, no-replay rule, queue",
                       "-- bound, and loud-overflow semantics — with no caller timeout: compose",
                       "-- a bound wait with `after` and `<|>`. The await itself blocks at the",
                       "-- HANDLER; the retry here only ever re-loops when a batch produced by",
                       "-- a merged, multi-source Event happens to carry no entry `ev` itself",
                       "-- projects, which is not spin-polling — each iteration is still one",
                       "-- genuine blocking round trip.",
                       "nextEvent :: Event a -> M (Observed a)",
                       "nextEvent ev = do",
                       "  sub <- send (RepoEventSubscribe ev.eventWatches) >>= liftEither",
                       "  r <- awaitFirst ev sub",
                       "  send (RepoEventUnsubscribe sub) >>= liftEither",
                       "  pure r"] },
                { raw ["awaitFirst :: Event a -> SubscriptionId -> M (Observed a)",
                       "awaitFirst ev sub = do",
                       "  batch <- awaitSubscriptionRaw sub (-1) >>= liftEither",
                       "  case firstMatch ev batch of",
                       "    Just observed -> pure observed",
                       "    Nothing -> awaitFirst ev sub"] },
                { raw ["-- | A deadline `ms` milliseconds from the moment it is SUBSCRIBED (not",
                       "-- from this call — `after` is pure data construction, no effect of its",
                       "-- own, so it needs no `Time` handler in the row), as a one-shot Event:",
                       "-- it fires exactly one Tick through the SAME subscription registry as",
                       "-- repository watches, so `nextEvent (someEvent <|> after ms)` reads as",
                       "-- an ordinary select with a timeout branch.",
                       "after :: Int -> M (Event Tick)",
                       "after ms = pure (Event [WatchDeadline ms] projectTick)"] },
                { raw ["projectTick :: RepositoryEvent -> Maybe Tick",
                       "projectTick (ObservedTick _ t) = Just t",
                       "projectTick _ = Nothing"] },
            ],
        }
    };
}

/// Subagent effect — single definition (PRD 18 LANE 1: the one-cycle coupled
/// spawn). PROVISIONAL by charter: this lane exists to inform PRD 18's
/// freezes, and every shape here may be renamed when root freezes the public
/// surface (in particular, the GADT is `Subagent` rather than `Agent` because
/// `Tidepool.Agent` is the harness answerer's module and the rename is root's
/// call — see `plans/post-restart/agent-lanes/inheritance-for-agent-core.md`
/// §8, and `lane1-scaffold-plan.md` for every other interpretation call).
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
/// `WorktreeHandle` / `WorktreeError` from `worktree_effect_def!`'s
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
            description [
                "Typed headless subagents (LANE 1: one-cycle coupled spawn). ",
                "`spawnAgentRaw spec schema` is ONE atomic call: it allocates or takes a ",
                "managed worktree, binds it to a fresh agent, dispatches the task to the ",
                "coding backend, runs ONE work cycle in that worktree, and returns a ",
                "`SpawnOutcome` (the WorkerRun pair, the terminal payload, and a receipt ",
                "naming the worktree, binding, backend thread, and EXACT resolved model) ",
                "— or a case-matchable `SpawnError` naming the saga stage that failed, ",
                "with the rollback already done: no binding is left active, and a created ",
                "worktree is retained and rebindable, never deleted. Build the spec with ",
                "`spawnSpec` (new worktree) or `spawnSpecIn` (existing unbound worktree). ",
                "`agentBeginRaw`/`agentResumeRaw` are the same saga driven one STOP at a ",
                "time, for an agent that holds dynamic tools: begin returns either a ",
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
                "same call for a child that may CALL BACK into your own Haskell handlers.",
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
            ],
            // Typed per-verb failure (#335) + PRD 18 addendum decision 2
            // (typed failure results everywhere; variant list is this lane's
            // contact with reality, deliberately provisional).
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
            ],
            helpers [
                { raw ["-- | RAW one-cycle coupled spawn: workspace + binding + agent + one",
                       "-- backend cycle, atomically; `schema` is the JSON Schema the terminal",
                       "-- result must conform to. Prefer the typed wrapper in",
                       "-- `Tidepool.Agent.Spawn` (schema derived from your result type's",
                       "-- Generic representation, payload decoded by its FromJSON instance);",
                       "-- this is its substrate.",
                       "spawnAgentRaw :: SpawnSpec -> Value -> M (Either SpawnError SpawnOutcome)",
                       "spawnAgentRaw spec schema = send (SubagentSpawn spec schema)"] },
                { raw ["-- | RAW begin of a coupled spawn that carries dynamic tools: it does",
                       "-- everything `spawnAgentRaw` does, then drives the turn to its FIRST",
                       "-- stop instead of to the end — either `StepToolCall agent callId tool",
                       "-- args` (the child called one of your tools and its turn is PARKED",
                       "-- until you answer) or `StepDone outcome` (it never called one).",
                       "-- `tools` is a JSON array of {name, description, inputSchema};",
                       "-- `schema` is the JSON Schema the terminal result must conform to.",
                       "-- Prefer `spawnAgentWithTools` in `Tidepool.Agent.Spawn`, which",
                       "-- compiles both from your types and runs the answer loop for you.",
                       "agentBeginRaw :: SpawnSpec -> Value -> Value -> M (Either SpawnError AgentStep)",
                       "agentBeginRaw spec tools schema = send (SubagentBegin spec tools schema)"] },
                { raw ["-- | RAW answer to the parked tool call, driving the turn on to its next",
                       "-- stop (another `StepToolCall`, or `StepDone`). `agent` and `callId`",
                       "-- are echoed from the `StepToolCall` you are answering; naming a",
                       "-- different agent or a different call is refused (`SpawnDriveFailed`)",
                       "-- rather than misrouted. `ok` False is a REFUSAL the child reads and",
                       "-- reacts to — an ordinary conversational fact, not a transport error,",
                       "-- and never a way to leave the call unanswered; `body` is then the",
                       "-- text it sees. Prefer `spawnAgentWithTools` in `Tidepool.Agent.Spawn`.",
                       "agentResumeRaw :: AgentId -> Text -> Bool -> Value -> M (Either SpawnError AgentStep)",
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
                { raw ["renderSpawnError :: SpawnError -> Text",
                       "renderSpawnError (SpawnWorktreeFailed st e) = \"spawn failed at \" <> show st <> \" (worktree): \" <> renderWorktreeError e",
                       "renderSpawnError (SpawnBindingFailed st e) = \"spawn failed at \" <> show st <> \" (binding): \" <> renderWorktreeError e",
                       "renderSpawnError (SpawnBackendFailed st b) = \"spawn failed at \" <> show st <> \" (backend): \" <> renderBackendFailure b",
                       "renderSpawnError (SpawnRollbackFailed st orig rb) = \"spawn failed at \" <> show st <> \" AND rollback failed: \" <> orig <> \"; rollback: \" <> rb",
                       "renderSpawnError (SpawnResultMalformed d) = \"spawn result malformed: \" <> d",
                       "renderSpawnError (SpawnDriveFailed st d) = \"spawn drive failed at \" <> show st <> \": \" <> d"] },
            ],
        }
    };
}

/// Journal effect — single definition (PRD 20, S1-L5 substrate slice).
///
/// A durable append-only run journal: a resident harness records completed
/// steps as it happens, mid-loop, so a crash loses only in-flight work —
/// never the record of what already finished. One verb, `record kind key
/// payload`: `kind`/`key` are caller-chosen labels, `payload` an opaque JSON
/// value (the PRD's locked lean — a fixed step shape, not a
/// harness-extensible one). Like Worktree/RepoEvent/Subagent, this is NOT in
/// `build_base_stack`'s row: which file a run journals to, and folding it
/// into boot-time resume, is the swarm driver's job, wired at merge.
// See `http_effect_def!` on why `crate::` (not `$crate`) is correct for
// `crate::effect_glue::JsonArg` here — it resolves at the EXPANSION site
// (tidepool-handlers' `effect_rust_projection!`), the only consumer of the
// Rust arg types.
#[allow(clippy::crate_in_macro_def)]
#[macro_export]
macro_rules! journal_effect_def {
    ($project:path) => {
        $project! {
            effect Journal,
            handler JournalHandler,
            req JournalReq,
            decl_fn journal_decl,
            description [
                "Durable append-only run journal: a resident harness records completed ",
                "steps as it happens, mid-loop, so progress survives a crash and resume ",
                "can fold the journal instead of redoing finished work. `record kind key ",
                "payload` appends ONE entry — `kind` and `key` are caller-chosen labels ",
                "(e.g. a step kind and the branch or task it concerns), `payload` is an ",
                "opaque JSON value. Every append is flushed immediately; the journal is ",
                "append-only forever — there is no rewrite or compaction verb.",
            ],
            type_defs [],
            verbs [
                { ctor RecordStep, method record_step,
                  args { kind: "Text" as String, key: "Text" as String, payload: "Value" as crate::effect_glue::JsonArg },
                  ret "()" },
            ],
            helpers [
                { raw ["-- | Append one durable journal entry. `kind` and `key` are",
                       "-- caller-chosen labels; `payload` is an opaque JSON value. Flushed",
                       "-- immediately; append-only — never rewritten or compacted.",
                       "record :: Text -> Text -> Value -> M ()",
                       "record kind key payload = send (RecordStep kind key payload)"] },
            ],
        }
    };
}

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
               getCurrentTime :: M UTCTime\n\
               getCurrentTime = UTCTime <$> send TimeNow"
            ]
        );
    }

    /// #335 Exec wave: the generated decl threads `Either ExecError` through
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
        assert!(
            d.type_defs.contains(&"data ExecError = ExecSpawn Text | ExecBadDir Text deriving (Show, Eq)\ninstance ToJSON ExecError where\n  toJSON e = case e of\n    ExecSpawn detail -> object [\"tag\" .= (\"ExecSpawn\" :: Text), \"detail\" .= detail]\n    ExecBadDir detail -> object [\"tag\" .= (\"ExecBadDir\" :: Text), \"detail\" .= detail]\n")
        );
        assert_eq!(d.helpers.len(), 3);
        assert_eq!(
            d.helpers[0],
            "-- | Run a shell command; returns a `Proc` record {exitCode, stdout, stderr}\n\
             -- (use `ok p` for the zero-exit check). Failure is TYPED (#335): `Left\n\
             -- (ExecSpawn _)` when the process can't be spawned, `Left (ExecBadDir _)`\n\
             -- for `runIn` with a bad/escaping directory. A nonzero EXIT is NOT a\n\
             -- failure — inspect `p.exitCode`. Natural spelling: `Right p <- run cmd`.\n\
             run :: Text -> M (Either ExecError Proc)\n\
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

    /// #335 mechanism: the `errors` grammar renders an `Either`-wrapped result
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

    /// #335 Fs wave: the generated `fs_decl()` threads `Either FsError` through
    /// the tagged verbs, leaves the untagged ones bare, and emits the ADT into
    /// `type_defs`.
    #[test]
    fn generated_fs_decl_threads_either_and_emits_error_adt() {
        let d = crate::fs_decl();
        assert!(
            d.constructors
                .contains(&"FsRead :: Text -> Fs (Either FsError Text)"),
            "{:?}",
            d.constructors
        );
        assert!(d
            .constructors
            .contains(&"FsExists :: Text -> Fs (Either FsError Bool)"));
        assert!(d
            .constructors
            .contains(&"FsGrep :: Text -> Text -> Fs (Either FsError [Hit])"));
        // Untagged verbs keep their bare result.
        assert!(d
            .constructors
            .contains(&"FsMetadata :: Text -> Fs (Maybe FileMeta)"));
        assert!(d
            .constructors
            .contains(&"FsReadGlob :: Text -> Fs [FileRead]"));
        // TryFsRead is gone.
        assert!(!d.constructors.iter().any(|c| c.starts_with("TryFsRead")));
        // Both the FileRead record and the error ADT land in type_defs.
        assert!(d.type_defs.contains(
            &"data FileRead = FileRead { path :: Text, contents :: Either FsError Text } deriving (Show, Eq)"
        ));
        // FileRead renders as a result value (the eval wrapper is `toJSON _r`),
        // so its ToJSON instance ships alongside the decl.
        assert!(d.type_defs.contains(
            &"instance ToJSON FileRead where\n  toJSON (FileRead p c) = object [\"path\" .= p, \"contents\" .= c]"
        ));
        assert!(d.type_defs.contains(&"data FsError = FsNotFound Text | FsNotUtf8 Text | FsSandbox Text | FsBadRegex Text | FsIo Text deriving (Show, Eq)\ninstance ToJSON FsError where\n  toJSON e = case e of\n    FsNotFound path -> object [\"tag\" .= (\"FsNotFound\" :: Text), \"path\" .= path]\n    FsNotUtf8 path -> object [\"tag\" .= (\"FsNotUtf8\" :: Text), \"path\" .= path]\n    FsSandbox detail -> object [\"tag\" .= (\"FsSandbox\" :: Text), \"detail\" .= detail]\n    FsBadRegex detail -> object [\"tag\" .= (\"FsBadRegex\" :: Text), \"detail\" .= detail]\n    FsIo detail -> object [\"tag\" .= (\"FsIo\" :: Text), \"detail\" .= detail]\n"));
    }

    /// #335 Git wave: every verb threads `Either GitError`, and the error ADT
    /// (with `GitFailed`'s 2-field variant) lands in type_defs.
    #[test]
    fn generated_git_decl_threads_either_and_emits_error_adt() {
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
            .type_defs.contains(&"data GitError = GitBadRevspec Text | GitFailed Int Text deriving (Show, Eq)\ninstance ToJSON GitError where\n  toJSON e = case e of\n    GitBadRevspec detail -> object [\"tag\" .= (\"GitBadRevspec\" :: Text), \"detail\" .= detail]\n    GitFailed code detail -> object [\"tag\" .= (\"GitFailed\" :: Text), \"code\" .= code, \"detail\" .= detail]\n"));
    }

    /// #335 Http wave: HttpGet/HttpPost thread `Either HttpError`,
    /// `HttpStatus` carries the status CODE as a field, and Try* is gone.
    /// JSON parsing is pure (`eitherDecode` over the JsonDecode primop) — the
    /// Http effect carries no parse verb and no bad-JSON error.
    #[test]
    fn generated_http_decl_threads_either_and_emits_error_adt() {
        let d = crate::http_decl();
        assert!(d
            .constructors
            .contains(&"HttpGet :: Text -> Http (Either HttpError Value)"));
        assert!(d
            .constructors
            .contains(&"HttpPost :: Text -> Value -> Http (Either HttpError Value)"));
        assert!(!d.constructors.iter().any(|c| c.contains("ParseJson")));
        assert!(!d.constructors.iter().any(|c| c.starts_with("Try")));
        assert!(d.type_defs.contains(&"data HttpError = HttpInvalidUrl Text | HttpRestricted Text | HttpNetwork Text | HttpStatus Int Text | HttpTooLarge Int deriving (Show, Eq)\ninstance ToJSON HttpError where\n  toJSON e = case e of\n    HttpInvalidUrl detail -> object [\"tag\" .= (\"HttpInvalidUrl\" :: Text), \"detail\" .= detail]\n    HttpRestricted detail -> object [\"tag\" .= (\"HttpRestricted\" :: Text), \"detail\" .= detail]\n    HttpNetwork detail -> object [\"tag\" .= (\"HttpNetwork\" :: Text), \"detail\" .= detail]\n    HttpStatus code body -> object [\"tag\" .= (\"HttpStatus\" :: Text), \"code\" .= code, \"body\" .= body]\n    HttpTooLarge nodes -> object [\"tag\" .= (\"HttpTooLarge\" :: Text), \"nodes\" .= nodes]\n"));
    }

    /// #335 Llm wave: LlmStructured threads `Either LlmError`, `LlmBudget` is
    /// a nullary constructor (budget exhaustion is DATA, not an abort), and
    /// TryLlmStructured is gone.
    #[test]
    fn generated_llm_decl_threads_either_and_emits_error_adt() {
        let d = crate::llm_decl();
        assert!(d
            .constructors
            .contains(&"LlmStructured :: Text -> Value -> Llm (Either LlmError Value)"));
        assert!(!d.constructors.iter().any(|c| c.starts_with("Try")));
        assert!(d.type_defs.contains(&"data LlmError = LlmApi Text | LlmRefusal Text | LlmBudget deriving (Show, Eq)\ninstance ToJSON LlmError where\n  toJSON e = case e of\n    LlmApi detail -> object [\"tag\" .= (\"LlmApi\" :: Text), \"detail\" .= detail]\n    LlmRefusal detail -> object [\"tag\" .= (\"LlmRefusal\" :: Text), \"detail\" .= detail]\n    LlmBudget -> object [\"tag\" .= (\"LlmBudget\" :: Text)]\n"));
    }

    /// #335 Lsp wave: MINIMAL tagging — only the seed (`lspWhere`) and the
    /// plain-list `lspDiags` thread `Either LspError`. The list-returning
    /// graph-walk verbs (lspCallers/lspCallees/lspRefs) are plain `[LspNode]`
    /// (round-2 ergonomics: empty = none, so they compose with concatMapM);
    /// lspDef/lspHover/lspRename keep `Maybe` (genuine per-node absence).
    #[test]
    fn generated_lsp_decl_threads_either_for_seed_and_diags_only() {
        let d = crate::lsp_decl();
        assert!(d
            .constructors
            .contains(&"LspWhere :: Text -> Lsp (Either LspError [LspNode])"));
        assert!(d
            .constructors
            .contains(&"LspDiagnostics :: Text -> Lsp (Either LspError [Diag])"));
        // List-returning graph-walk verbs are plain [LspNode], not Maybe-wrapped.
        assert!(d
            .constructors
            .contains(&"LspCallers :: LspNode -> Lsp [LspNode]"));
        assert!(d
            .constructors
            .contains(&"LspCallees :: LspNode -> Lsp [LspNode]"));
        assert!(d
            .constructors
            .contains(&"LspRefs :: LspNode -> Lsp [LspNode]"));
        // lspDef/lspHover keep Maybe — a node genuinely may lack one.
        assert!(d
            .constructors
            .contains(&"LspDef :: LspNode -> Lsp (Maybe LspNode)"));
        assert!(d
            .type_defs.contains(&"data LspError = LspDaemonDown Text deriving (Show, Eq)\ninstance ToJSON LspError where\n  toJSON e = case e of\n    LspDaemonDown detail -> object [\"tag\" .= (\"LspDaemonDown\" :: Text), \"detail\" .= detail]\n"));
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
                 say :: Text -> M ()\n\
                 say = send . Print",
                "-- | `say` on anything Showable (`say . show`).\n\
                 sayShow :: Show a => a -> M ()\n\
                 sayShow = say . show",
            ]
        );
    }
}
