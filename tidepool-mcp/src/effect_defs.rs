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
            verbs [
                { ctor GitLog, method git_log,
                  args { n: "Int" as i64 },
                  ret "[Commit]" },
                { ctor GitStatus, method git_status,
                  args { },
                  ret "[StatusEntry]" },
                { ctor GitDiffStat, method git_diff_stat,
                  args { rev: "Text" as String },
                  ret "[FileDelta]" },
                { ctor GitShow, method git_show,
                  args { rev: "Text" as String },
                  ret "Commit" },
            ],
            helpers [
                { name gitLog, sig "Int -> M [Commit]",
                  doc ["Last N commits, newest-first. Each 'Commit' carries sha/subject/author/date/files."],
                  body pointfree GitLog },
                { name gitStatus, sig "M [StatusEntry]",
                  doc ["Working-tree status. Each 'StatusEntry' has path and 2-char XY state code",
                       "(e.g. \"M \", \"??\", \"A \")."],
                  body nullary GitStatus },
                { name gitDiffStat, sig "Text -> M [FileDelta]",
                  doc ["Per-file diff stats vs a revspec (\"HEAD~1\", \"main\", \"HEAD~3..HEAD\", etc.).",
                       "'FileDelta' carries path/adds/dels/binary."],
                  body pointfree GitDiffStat },
                { name gitShow, sig "Text -> M Commit",
                  doc ["Single commit by revspec. Fails the eval on an unknown or ambiguous revspec."],
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

/// Llm effect — single definition.
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
            verbs [
                { ctor LlmStructured, method llm_structured,
                  args { prompt: "Text" as String, schema: "Value" as tidepool_eval::value::Value },
                  ret "Value" },
                // Failure-isolating variant: an API/network error or refusal
                // becomes `Left err` instead of killing the eval. (Budget
                // exhaustion still aborts — that's a hard control limit.)
                { ctor TryLlmStructured, method llm_try_structured,
                  args { prompt: "Text" as String, schema: "Value" as tidepool_eval::value::Value },
                  ret "(Either Text Value)" },
            ],
            helpers [
                // schemaToValue lives in ask_decl (Ask is always present).
                { raw ["llm :: Schema -> Text -> M Value",
                       "llm schema prompt = send (LlmStructured prompt (schemaToValue schema))"] },
                // Isolating variant: an API failure/refusal becomes `Left err`
                // instead of aborting the eval (the LLM call-budget limit still
                // aborts — it is a hard control limit, not a probe failure).
                { raw ["tryLlm :: Schema -> Text -> M (Either Text Value)",
                       "tryLlm schema prompt = send (TryLlmStructured prompt (schemaToValue schema))"] },
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
                "Everything is a LspNode {name, container, kind, file, line, text} — the currency you thread. ",
                "`lspWhere name` → all definitions of NAME (the seed). Then walk the graph: ",
                "`lspCallers n` / `lspCallees n` (incoming/outgoing calls), `lspRefs n` (use sites), ",
                "`lspDef n` (any node → its definition), `lspHover n` (type/sig/docs), ",
                "`lspRename n new` (→ unified diff; review then `applyDiff`). Each returns LspNodes you feed ",
                "back in (e.g. `lspWhere \"x\" >>= concatMapM lspCallers`). `lspDiags file` for a file's errors. ",
                "Needs the `tidepool-lsp-daemon` running in the workspace; queries error cleanly if not.",
            ],
            type_defs [
                "data Position = Position { posLine :: Int, posChar :: Int }",
                "data LspNode = LspNode { nodeName :: Text, nodeContainer :: Text, nodeKind :: Text, nodeFile :: Text, nodePos :: Position, nodeText :: Text }",
                "data Diag = Diag { diagFile :: Text, diagLine :: Int, diagSeverity :: Text, diagMessage :: Text }",
                "nodeLine :: LspNode -> Int\nnodeLine = posLine . nodePos",
                "instance ToJSON Position where\n  toJSON (Position l c) = object [\"line\" .= l, \"char\" .= c]",
                "instance ToJSON LspNode where\n  toJSON nd@(LspNode n c k f _ t) = object [\"name\" .= n, \"container\" .= c, \"kind\" .= k, \"file\" .= f, \"line\" .= nodeLine nd, \"text\" .= t]",
                "instance ToJSON Diag where\n  toJSON (Diag f l s m) = object [\"file\" .= f, \"line\" .= l, \"severity\" .= s, \"message\" .= m]",
            ],
            verbs [
                { ctor LspWhere, method lsp_where,
                  args { symbol: "Text" as String },
                  ret "[LspNode]" },
                { ctor LspCallers, method lsp_callers,
                  args { n: "LspNode" as LspNode },
                  ret "(Maybe [LspNode])" },
                { ctor LspCallees, method lsp_callees,
                  args { n: "LspNode" as LspNode },
                  ret "(Maybe [LspNode])" },
                { ctor LspRefs, method lsp_refs,
                  args { n: "LspNode" as LspNode },
                  ret "(Maybe [LspNode])" },
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
                  ret "[Diag]" },
            ],
            helpers [
                { name lspWhere, sig "Text -> M [LspNode]",
                  doc ["Seed: every workspace definition named X (each a LspNode with container/file/line/source line)."],
                  body pointfree LspWhere },
                { name lspCallers, sig "LspNode -> M (Maybe [LspNode])",
                  doc ["Incoming calls. Nothing = node not callable; Just [] = callable, none. Unwrap with callersOf for plain chaining."],
                  body pointfree LspCallers },
                { name lspCallees, sig "LspNode -> M (Maybe [LspNode])",
                  doc ["Outgoing calls. Nothing = node not callable; Just [] = callable, none."],
                  body pointfree LspCallees },
                { name lspRefs, sig "LspNode -> M (Maybe [LspNode])",
                  doc ["Use sites of this node's symbol (kind = \"reference\"). Nothing = not a symbol."],
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
                { name lspDiags, sig "FilePath -> M [Diag]",
                  doc ["Diagnostics (errors / warnings) for FILE."],
                  body pointfree LspDiagnostics },
            ],
        }
    };
}

/// KV effect — single definition.
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
