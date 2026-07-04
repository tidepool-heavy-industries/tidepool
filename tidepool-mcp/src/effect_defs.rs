//! Single-source effect definitions (T6 spike).
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
//!     [, errors <RustErrorEnum>] RESERVED (#335): typed per-verb failure — see below
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
//! ## The `errors` slot (#335 — reserved, NOT implemented)
//!
//! `errors FsError` on a verb row is accepted by both projections and
//! currently ignored. Its designed semantics: the decl projection renders the
//! result as `<Effect> (Either FsError <ret>)`; the Rust projection tightens
//! the dispatch arm so the hand-written method returns typed
//! `Result<_, FsError>` data and the handler becomes total (never aborts the
//! eval). The error ADT itself is a Rust enum deriving `CoreRecord` +
//! `ToCore`, so its Haskell `data` decl is generated into `type_defs` by the
//! existing record single-source. See `notes/effect-single-source-spike.md`.

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

/// The tidepool-mcp projection: expand a definition into its `EffectDecl`
/// builder fn. Constructor strings render as
/// `<Ctor> :: <arg-hs> -> ... -> <Effect> <ret>`; helpers render via
/// [`helper_text!`]. The `handler`/`req`/`method` facts (and the reserved
/// `errors` slot) are Rust-side and ignored here.
macro_rules! effect_decl_projection {
    (
        effect $eff:ident,
        handler $handler:ident,
        req $req:ident,
        decl_fn $decl_fn:ident,
        description [$($desc:literal),* $(,)?],
        type_defs [$($td:literal),* $(,)?],
        verbs [
            $({ ctor $ctor:ident,
                method $method:ident,
                args { $($an:ident : $ah:literal as $ar:ty),* $(,)? },
                ret $ret:literal
                $(, errors $err:ty)?
                $(,)?
            }),* $(,)?
        ],
        helpers [ $($helper:tt),* $(,)? ] $(,)?
    ) => {
        pub fn $decl_fn() -> $crate::EffectDecl {
            $crate::EffectDecl {
                type_name: stringify!($eff),
                description: concat!($($desc),*),
                constructors: &[
                    $( concat!(
                        stringify!($ctor), " :: ",
                        $($ah, " -> ",)*
                        stringify!($eff), " ", $ret
                    ) ),*
                ],
                type_defs: &[$($td),*],
                helpers: &[ $( crate::effect_defs::helper_text!($helper) ),* ],
            }
        }
    };
}
pub(crate) use effect_decl_projection;

/// Console effect — THE single definition (prototype effect for the T6 spike).
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
            verbs [
                { ctor Run, method exec_run,
                  args { cmd: "Text" as String },
                  ret "(Int, Text, Text)" },
                { ctor RunIn, method exec_run_in,
                  args { dir: "Text" as String, cmd: "Text" as String },
                  ret "(Int, Text, Text)" },
                // Failure-isolating spawn: Left only when the process cannot be
                // SPAWNED (sandbox/exec error). A command that runs and exits
                // nonzero is Right (code, out, err) — the eval inspects the code.
                { ctor TryRun, method exec_try_run,
                  args { cmd: "Text" as String },
                  ret "(Either Text (Int, Text, Text))" },
                { ctor TryRunIn, method exec_try_run_in,
                  args { dir: "Text" as String, cmd: "Text" as String },
                  ret "(Either Text (Int, Text, Text))" },
                // Shell-free exec: argv list, no sh -c. Safe with metachars ($1, globs).
                { ctor RunArgv, method exec_run_argv,
                  args { argv: "[Text]" as Vec<String> },
                  ret "(Int, Text, Text)" },
            ],
            helpers [
                { raw ["callCommand :: Text -> M ()",
                       "callCommand cmd = do { p <- run cmd; when (not (ok p)) (error (\"command failed (\" <> show p.exitCode <> \"): \" <> p.stderr)) }"] },
                { raw ["readProcess :: Text -> M Text",
                       "readProcess cmd = do { p <- run cmd; if ok p then pure p.stdout else error (\"command failed (\" <> show p.exitCode <> \"): \" <> p.stderr) }"] },
                { raw ["-- | Run a shell command; returns a `Proc` record {exitCode, stdout, stderr}",
                       "-- (use `ok p` for the zero-exit check).",
                       "run :: Text -> M Proc",
                       "run cmd = (\\(ec, o, e) -> Proc ec o e) <$> send (Run cmd)"] },
                { raw ["runIn :: Text -> Text -> M Proc",
                       "runIn dir cmd = (\\(ec, o, e) -> Proc ec o e) <$> send (RunIn dir cmd)"] },
                // Isolating variants: spawn failure becomes `Left err` instead of
                // aborting the eval. A nonzero exit is NOT a failure here — it
                // arrives as `Right (code, out, err)`, so the common eval-killer
                // (readProcess on nonzero exit) is avoided by inspecting the code.
                { raw ["tryRun :: Text -> M (Either Text Proc)",
                       "tryRun cmd = send (TryRun cmd) <&> fmap (\\(ec, o, e) -> Proc ec o e)"] },
                { raw ["tryRunIn :: Text -> Text -> M (Either Text Proc)",
                       "tryRunIn dir cmd = send (TryRunIn dir cmd) <&> fmap (\\(ec, o, e) -> Proc ec o e)"] },
                // Shell-free: argv list, no sh -c. $1/$VAR/globs are literal — safe.
                { raw ["runArgv :: [Text] -> M Proc",
                       "runArgv argv = (\\(ec, o, e) -> Proc ec o e) <$> send (RunArgv argv)"] },
            ],
        }
    };
}

/// Http effect — single definition.
#[macro_export]
macro_rules! http_effect_def {
    ($project:path) => {
        $project! {
            effect Http,
            handler HttpHandler,
            req HttpReq,
            decl_fn http_decl,
            description [
                "JSON I/O. Fetch JSON from HTTP endpoints (returns Value), or ",
                "parse a JSON Text into a Value with `parseJson`/`tryParseJson` ",
                "(spec-compliant, parsed Rust-side via serde_json).",
            ],
            type_defs [],
            verbs [
                { ctor HttpGet, method http_get,
                  args { url: "Text" as String },
                  ret "Value" },
                { ctor HttpPost, method http_post,
                  args { url: "Text" as String, body: "Value" as tidepool_eval::value::Value },
                  ret "Value" },
                // Failure-isolating variants: a network error or non-2xx status
                // becomes `Left err` instead of killing the eval.
                { ctor TryHttpGet, method http_try_get,
                  args { url: "Text" as String },
                  ret "(Either Text Value)" },
                { ctor TryHttpPost, method http_try_post,
                  args { url: "Text" as String, body: "Value" as tidepool_eval::value::Value },
                  ret "(Either Text Value)" },
                // Parse a JSON string Rust-side (serde_json) into a Value. ParseJson
                // raises on invalid JSON; TryParseJson returns Left.
                { ctor ParseJson, method http_parse_json,
                  args { s: "Text" as String },
                  ret "Value" },
                { ctor TryParseJson, method http_try_parse_json,
                  args { s: "Text" as String },
                  ret "(Either Text Value)" },
            ],
            helpers [
                { name httpGet, sig "Text -> M Value",
                  doc ["Fetch JSON from an HTTP endpoint."],
                  body pointfree HttpGet },
                { name httpPost, sig "Text -> Value -> M Value",
                  doc ["POST a JSON body; returns the response Value."],
                  body applied HttpPost(url, body) },
                // Isolating variants: a 404/network failure becomes `Left err`
                // (carrying the URL + cause) instead of aborting the eval.
                { name tryHttpGet, sig "Text -> M (Either Text Value)",
                  doc ["`httpGet` with failure isolation: network/status errors arrive as `Left err`."],
                  body pointfree TryHttpGet },
                { name tryHttpPost, sig "Text -> Value -> M (Either Text Value)",
                  doc ["`httpPost` with failure isolation: network/status errors arrive as `Left err`."],
                  body applied TryHttpPost(url, body) },
                // Parse JSON Text into ANY FromJSON type: the result type drives the
                // decode (`FromJSON Value` is identity, so `parseJson t :: M Value`
                // gives the raw value; `:: M Cfg` decodes a record). Raises on a parse
                // OR decode failure.
                { raw ["parseJson :: FromJSON a => Text -> M a",
                       "parseJson t = send (ParseJson t) >>= \\v -> case fromJSON v of { Success a -> pure a; Error e -> error (T.pack e) }"] },
                // Failure-isolating: a parse OR decode error becomes `Left err`.
                { raw ["tryParseJson :: FromJSON a => Text -> M (Either Text a)",
                       "tryParseJson t = send (TryParseJson t) >>= \\r -> pure (r >>= resultToEither . fromJSON)"] },
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
            &["-- | Current UTC time as an opaque UTCTime (epoch-millisecond resolution).\n\
               getCurrentTime :: M UTCTime\n\
               getCurrentTime = UTCTime <$> send TimeNow"]
        );
    }

    /// Exec migrated with EXACT transport: generated decl must be
    /// byte-identical to the hand-written baseline it replaced.
    #[test]
    fn generated_exec_decl_matches_handwritten_baseline() {
        let d = crate::exec_decl();
        assert_eq!(d.type_name, "Exec");
        assert_eq!(d.description, "Run shell commands and capture output.");
        assert_eq!(
            d.constructors,
            &[
                "Run :: Text -> Exec (Int, Text, Text)",
                "RunIn :: Text -> Text -> Exec (Int, Text, Text)",
                "TryRun :: Text -> Exec (Either Text (Int, Text, Text))",
                "TryRunIn :: Text -> Text -> Exec (Either Text (Int, Text, Text))",
                "RunArgv :: [Text] -> Exec (Int, Text, Text)",
            ]
        );
        assert_eq!(d.helpers.len(), 7);
        assert_eq!(
            d.helpers[2],
            "-- | Run a shell command; returns a `Proc` record {exitCode, stdout, stderr}\n\
             -- (use `ok p` for the zero-exit check).\n\
             run :: Text -> M Proc\n\
             run cmd = (\\(ec, o, e) -> Proc ec o e) <$> send (Run cmd)"
        );
    }

    /// Meta's generated decl — pins the (deliberately normalized) output.
    #[test]
    fn generated_meta_decl_shape() {
        let d = crate::meta_decl();
        assert_eq!(d.type_name, "Meta");
        assert_eq!(d.constructors.len(), 7);
        assert_eq!(d.constructors[1], "MetaLookupCon :: Text -> Meta (Maybe (Int, Int))");
        assert_eq!(d.helpers.len(), 7);
        assert!(d.helpers[0].ends_with("metaConstructors = send MetaConstructors"));
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
