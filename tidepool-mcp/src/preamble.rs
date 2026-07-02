//! Preamble, tool-description, and library-vocab assembly for the MCP server.
//!
//! [`build_preamble`] emits the eval expr module's header (pragmas + imports,
//! including `import Tidepool.Orchestrate`) plus the mode-selected
//! `paginateResult` alias. The pagination + orchestration helper BODIES live in
//! the generated [`orchestrate_module_source`] module (a pure function of the
//! effect set), imported rather than spliced into the expr module's scope — the
//! fix for the namespace-poison bug class where a session bind (`let glob = 5`)
//! captured a helper body's bare verb. [`build_eval_tool_description`] renders
//! the `eval` tool's human-facing description from the live effect set, and
//! [`library_vocab`] digests a user-library directory into a per-module
//! signature index.
//!
//! `build_preamble` is decomposed into named section builders
//! (`pragmas_and_imports`, `paginate_alias`) so the emitted-source seams are
//! legible.

use crate::EffectDecl;
use tidepool_runtime::session::ModuleEnv;

/// The `default` declaration emitted after all imports in the preamble.
/// Exported as the canonical injection-point marker: `insert_imports` in
/// `tidepool-repl` uses it to locate where session/user imports must go,
/// avoiding a magic-substring dependency on the literal text (AUDIT-3).
pub const PREAMBLE_DEFAULT_DECL: &str = "default (Int, Double, Text)\n";

/// The LANGUAGE pragma block shared by the eval `Expr` module and the session
/// Lane-A declaration modules (`Tidepool.Session.Lib.G<g>`). One dialect
/// everywhere: a helper written with `session_def` sees exactly the extensions
/// an `eval`/`session_eval` expression does. No trailing newline — callers that
/// emit it add their own (the eval preamble and `ModuleEnv::pragmas` both do).
// NB: NoMonomorphismRestriction is NOT in EVAL_PRAGMAS — the eval EXPRESSION
// module needs the MR so `__user = pure (…)` monomorphizes against its do-block
// context (with NMR + ExtendedDefaultRules it generalizes the `Applicative`
// constraint and mis-defaults). NMR is added ONLY to the session DECL module
// env (`session_decl_module_env`), where pure binds land as decls and MUST
// generalize (`n = 5` → `Num a => a`). See that fn and render.rs `ModuleEnv`.
pub const EVAL_PRAGMAS: &str = "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, UndecidableInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables, ExtendedDefaultRules, LambdaCase, TupleSections, MultiWayIf, RecordWildCards, NamedFieldPuns, ViewPatterns, BangPatterns, TypeApplications, BlockArguments, NumericUnderscores, MultilineStrings, DeriveFunctor, DeriveFoldable, DeriveTraversable, QuasiQuotes, DuplicateRecordFields, OverloadedRecordDot #-}";

/// EVAL_PRAGMAS with `NoMonomorphismRestriction` inserted — the pragma block
/// for the session DECLARATION module, where pure binds land as top-level
/// bindings that must generalize (see `session_decl_module_env`).
pub fn decl_pragmas() -> String {
    EVAL_PRAGMAS.replacen(
        "NoImplicitPrelude,",
        "NoImplicitPrelude, NoMonomorphismRestriction,",
        1,
    )
}

/// The canonical ordered import lines shared by the eval `Expr` module and the
/// session decl modules — the SINGLE source of truth for the eval vocabulary
/// (`M`, the effect verbs from `Tidepool.Effects`, the `Tidepool.Prelude`
/// shadows, and the `T.`/`Map.`/`L.`/`Set.`/… qualified namespaces). Excludes
/// the `module Expr where` header and the `default` decl (those are eval-module
/// scaffolding, not imports). `user_library` inserts `import Library` — eval
/// only; the session has no user `Library` module, it accumulates its own
/// `Tidepool.Session.Lib.G<g>` chain instead.
pub fn eval_import_lines(user_library: bool) -> Vec<&'static str> {
    let mut v = vec![
        "import Tidepool.Prelude hiding (error)",
        // Effect GADTs, `M`, the `error` shadow, and the send-wrapper helpers
        // live in the generated Tidepool.Effects module so library AND session
        // decl modules can import the SAME types and define effectful verbs.
        "import Tidepool.Effects",
        "import qualified Tidepool.Data.Text as T",
        "import qualified Data.Map.Strict as Map",
        // Merge/reconcile API (merge, zipWithMatched, …) — strict, matches Map.
        "import qualified Data.Map.Merge.Strict as MM",
        "import qualified Data.Set as Set",
        // `Aeson` qualifier: the `input` payload-lane injection emits
        // `input :: Aeson.Value` / `Aeson.String …` (json_to_haskell), so the
        // qualifier must be in scope for both eval AND session_eval.
        "import qualified Tidepool.Aeson as Aeson",
        "import qualified Tidepool.Aeson.KeyMap as KM",
        "import qualified Data.List as L",
        "import qualified Tidepool.TextFormat as TF",
        "import qualified Tidepool.Table as Tab",
        "import qualified Tidepool.Patch as Patch",
        // NOTE: Tidepool.Shell/Git/Cargo are NOT here — they depend on the Exec
        // (`runArgv`) + Http (`parseJson`) effect helpers, so they're imported
        // conditionally in `pragmas_and_imports` only when those effects are in
        // the stack (else they fail to load on a minimal/Console-only stack).
        "import Control.Monad.Freer hiding (run)",
    ];
    if user_library {
        v.push("import Library");
    }
    v.push("import qualified Prelude as P");
    v
}

/// The [`ModuleEnv`] for session Lane-A declaration modules under the full
/// effect stack — the eval pragmas + import surface (via [`EVAL_PRAGMAS`] /
/// [`eval_import_lines`]) so `session_def` helpers share the eval vocabulary
/// (e.g. `sh :: Text -> M Text` using `run`, `L.sortBy`, `Set.`, …). The
/// lens-free [`ModuleEnv::standalone_default`] remains for the plain-toolchain
/// standalone REPL/tests; this requires the `with-packages` GHC (it imports
/// `Tidepool.Prelude`, which pulls `Control.Lens`).
///
/// `user_library`: whether a project/global `Library` facade is on the
/// include path (mirrors the `stmt`-path flag in `has_user_library`, passed
/// by the caller since this module has no filesystem access to check
/// `base_include` itself). When `true`, decl items get `import Library` —
/// previously ALWAYS omitted here regardless of the flag (a decl referencing
/// a Library-re-exported type like `EditOutcome` needed its own explicit
/// import even though `:vocab` listed it as available). The caller
/// (`render_module`) is responsible for adding a `hiding (...)` clause
/// against a session's own decl heads, the same guard the `stmt` path already
/// applies via `hide_module_names` — importing `Library` unqualified here
/// without it would reintroduce the ambiguous-occurrence class BUG-7 fixed
/// for statements (a decl defining e.g. `data Hit`, which `Library`
/// re-exports, would collide).
#[must_use]
pub fn session_decl_module_env(user_library: bool) -> ModuleEnv {
    let mut imports: Vec<String> = eval_import_lines(user_library)
        .into_iter()
        .map(String::from)
        .collect();
    // Full-stack repl decl surface: also import the shell-effect modules so
    // `session_def` helpers can use Git/Shell/Cargo verbs (the eval module gets
    // them via `pragmas_and_imports`, but the decl `ModuleEnv` needs them too,
    // else a decl like `dirty = Git.gitStatus` fails to resolve). Safe here
    // because this env is only used under the full stack (Exec+Http present);
    // the lens-free `standalone_default` minimal env deliberately omits them.
    imports.push("import qualified Tidepool.Shell as Shell".into());
    imports.push("import qualified Tidepool.Git as Git".into());
    imports.push("import qualified Tidepool.Cargo as Cargo".into());
    // Orchestration helpers (readGlob/searchFiles/memo/renderJson/…): the
    // stmt plane gets these via the expr module's imports; without this the
    // decl plane's import surface diverges — a decl using `readGlob` failed
    // "not in scope" with no hint (friction #23, found live 2026-07-01).
    imports.push("import Tidepool.Orchestrate".into());
    ModuleEnv {
        pragmas: decl_pragmas(),
        imports,
    }
}

/// Emit the LANGUAGE pragma block, the `module Expr` header, and the fixed
/// import set (plus the conditional `import Library`).
///
/// QuasiQuotes + ViewPatterns are ALWAYS-ON by root decision: one eval
/// dialect everywhere beats conditional grammar (the Tidepool.QQ IMPORT
/// is still token-gated in eval() — scope, not syntax).
//
// KNOWN COST / FIXME(root): GHC's enableCodeGenForTH keys on extension
// PRESENCE (needsTemplateHaskellOrQQ checks xopt, not splice usage), so
// every eval bytecode-provisions its home-module graph — +3.0s per
// uncached eval (measured: 10.35s vs 7.30s full-preamble extract,
// 3-run avg), paid even by evals that never splice. REQUIRES an
// unpoison-fixed tidepool-extract-bin (this branch: GhcPipeline.hs
// unsets Opt_IgnoreInterfacePragmas on the downgraded summaries between
// depanal and load'); under a pre-fix binary every eval dies with the
// clz# deopt class (see plans/qq-spike.md "deoptimization bug", and the
// spliton repro tests which pin exactly this). Fix later = token-gating
// (see 71d77fb, reverted) or upstream lazy provisioning.
// Dialect note: with QuasiQuotes on, `[x|x<-xs]` (comprehension with no
// space before `|`) parses as a quasi-quote — write `[x | x <- xs]`.
fn pragmas_and_imports(out: &mut String, effects: &[EffectDecl], user_library: bool) {
    out.push_str(EVAL_PRAGMAS);
    out.push('\n');
    out.push_str("module Expr where\n");
    for imp in eval_import_lines(user_library) {
        out.push_str(imp);
        out.push('\n');
    }
    // Shell-effect stdlib modules depend on the Exec (`runArgv`) + Http
    // (`parseJson`) helpers in the generated Tidepool.Effects — import them only
    // when both effects are present, else they fail to load (e.g. on a minimal
    // Console-only stack).
    let has_exec = effects.iter().any(|e| e.type_name == "Exec");
    let has_http = effects.iter().any(|e| e.type_name == "Http");
    if has_exec && has_http {
        out.push_str("import qualified Tidepool.Shell as Shell\n");
        out.push_str("import qualified Tidepool.Git as Git\n");
        out.push_str("import qualified Tidepool.Cargo as Cargo\n");
    }
    // The pagination / orchestration helper DEFINITIONS live in the generated
    // Tidepool.Orchestrate module (always written by `ensure_effects_module`,
    // co-located + hashed with Tidepool.Effects), NOT spliced into this expr
    // module's own scope. Splicing them here let a session's unqualified bind
    // (`let glob = 5`) capture a helper body's bare `glob`, type-erroring every
    // turn (no `hiding` clause can protect a same-module definition). As an
    // imported module, Orchestrate's internal `glob` resolves in ITS scope.
    out.push_str("import Tidepool.Orchestrate\n");
    out.push_str(PREAMBLE_DEFAULT_DECL);
    out.push('\n');
}

/// Generate the complete `Tidepool.Orchestrate` module source: the pagination /
/// auto-truncation helpers (`putStrLn`, `valSize`, the `trunc*` family,
/// `renderJson`, `paginateInteractive`/`paginateTrunc`) plus the KV/file/process
/// orchestration helpers (`memo`, `readGlob`, `searchFiles`, …).
///
/// A PURE function of `effects` only (no `interactive_pagination` /
/// `user_library` dependence) so it can be co-located with — and content-hashed
/// alongside — the generated `Tidepool.Effects` module in the same staging dir
/// (see [`crate::ensure_effects_module`]). Every helper that references an effect
/// verb is gated on that effect's presence so the module compiles on any stack.
///
/// These helpers used to be spliced as raw text into every eval's own expr
/// module; living in their own importable module is the fix for the
/// namespace-poison bug class — a session bind like `let glob = 5` can no longer
/// capture a helper body's bare `glob` (Orchestrate's internal `glob` resolves
/// in ITS own scope), and the expr module hides colliding names on the import.
pub fn orchestrate_module_source(effects: &[EffectDecl]) -> String {
    let mut out = String::new();
    out.push_str(EVAL_PRAGMAS);
    out.push('\n');
    out.push_str("-- GENERATED by the tidepool MCP server (pure fn of the effect set).\n");
    out.push_str("-- Pagination + KV/file/process orchestration helpers, imported by\n");
    out.push_str("-- the eval expr module instead of spliced into its scope.\n");
    out.push_str("module Tidepool.Orchestrate where\n");
    out.push_str("import Tidepool.Prelude hiding (error)\n");
    out.push_str("import Tidepool.Effects\n");
    // `send` (used by `putStrLn`) comes from freer — Tidepool.Effects does not
    // re-export it (no export list ⇒ only its own decls), so import it here too.
    out.push_str("import Control.Monad.Freer hiding (run)\n");
    out.push_str("import qualified Tidepool.Data.Text as T\n");
    out.push_str("import qualified Data.Map.Strict as Map\n");
    out.push_str("import qualified Tidepool.Aeson.KeyMap as KM\n");
    out.push_str("import qualified Data.List as L\n");
    let has_exec = effects.iter().any(|e| e.type_name == "Exec");
    let has_http = effects.iter().any(|e| e.type_name == "Http");
    if has_exec && has_http {
        out.push_str("import qualified Tidepool.Shell as Shell\n");
        out.push_str("import qualified Tidepool.Git as Git\n");
        out.push_str("import qualified Tidepool.Cargo as Cargo\n");
    }
    out.push('\n');

    // ToWire: result-rendering class for show-default mode (tidepool-repl).
    // Always emitted (no effect dep) so template_haskell_show_default's
    // `toWire _r` resolves. Text renders bare; Value passes through as JSON;
    // any Show type via show.
    //
    // INVESTIGATED (2026-07-01): tried adding `instance {-# OVERLAPPING #-}
    // ToJSON a => ToWire a where toWire = toJSON` to prefer ToJSON over Show
    // when both are available. Does NOT work — GHC rejects it as "duplicate
    // instance declarations", because both instances have the IDENTICAL head
    // `ToWire a` (fully polymorphic) and differ only in their CONSTRAINT
    // (`Show a` vs `ToJSON a`), not in type structure. GHC's
    // OVERLAPPING/OVERLAPPABLE resolution only orders instances whose heads
    // differ structurally (e.g. `C [a]` vs `C a`) — it cannot pick between
    // two equally-general heads based on which constraint happens to be
    // satisfiable. That would need constraint-based backtracking Haskell's
    // instance resolution doesn't do (the closest real mechanisms —
    // `IncoherentInstances`, `Data.Reflection`-style dictionary reflection —
    // don't give the desired "prefer X if satisfiable, else Y" semantics
    // safely/deterministically). Do not re-attempt this exact approach
    // without a fundamentally different mechanism (e.g. a closed type family
    // computing a type-level `Bool` for "has ToJSON", dispatched via a
    // separate class hierarchy) — treat as a design question, not a quick fix.
    out.push_str(concat!(
        "class ToWire a where toWire :: a -> Value\n",
        "instance {-# OVERLAPPABLE #-} Show a => ToWire a where toWire = String . show\n",
        "instance ToWire Text where toWire = String\n",
        "instance ToWire Value where toWire = id\n",
        "\n",
    ));

    let has_ask = effects.iter().any(|e| e.type_name == "Ask");
    let has_console = effects.iter().any(|e| e.type_name == "Console");
    let has_kv = effects.iter().any(|e| e.type_name == "KV");
    let has_fs = effects.iter().any(|e| e.type_name == "Fs");
    let has_sg = effects.iter().any(|e| e.type_name == "SG");

    out.push_str("-- Pagination\n");
    out.push_str(concat!("showI :: Int -> Text\n", "showI n = show n\n",));
    // putStrLn: Print effect + char counter in KV (when available)
    if has_console && has_kv {
        out.push_str(concat!(
            "putStrLn :: Text -> M ()\n",
            "putStrLn t = do\n",
            "  send (Print t)\n",
            "  v <- kvGet \"__sayChars\"\n",
            "  let cur = case v of { Just b -> case b ^? _Number of { Just n -> round n; _ -> 0 }; Nothing -> 0 }\n",
            "  kvSet \"__sayChars\" (toJSON (cur + T.length t))\n",
        ));
    } else if has_console {
        out.push_str(concat!(
            "putStrLn :: Text -> M ()\n",
            "putStrLn = send . Print\n",
        ));
    }

    // valSize..renderJson are PURE (reference no effect verbs, only Value/KM/T)
    // — always emitted; the paginate variants below depend on them.
    out.push_str(concat!(
        "valSize :: Value -> Int\n",
        "valSize v = case v of\n",
        "  String t -> T.length t + 2\n",
        "  Number _ -> 8\n",
        "  NumberI _ -> 8\n",
        "  Bool b -> if b then 4 else 5\n",
        "  Null -> 4\n",
        "  Array xs -> arrSz xs 2\n",
        "  Object m -> objSz (KM.toList m) 2\n",
    ));
    out.push_str(concat!(
        "arrSz :: [Value] -> Int -> Int\n",
        "arrSz [] acc = acc\n",
        "arrSz [x] acc = acc + valSize x\n",
        "arrSz (x:xs) acc = arrSz xs (acc + valSize x + 2)\n",
    ));
    out.push_str(concat!(
        "objSz :: [(Key, Value)] -> Int -> Int\n",
        "objSz [] acc = acc\n",
        "objSz [(k,v)] acc = acc + T.length (KM.toText k) + 4 + valSize v\n",
        "objSz ((k,v):rest) acc = objSz rest (acc + T.length (KM.toText k) + 4 + valSize v + 2)\n",
    ));
    out.push_str(concat!(
        "truncArr :: Int -> Int -> [Value] -> ([Value], Int, [(Int, Value)])\n",
        "truncArr _ nid [] = ([], nid, [])\n",
        "truncArr bud nid (x:xs)\n",
        "  | bud <= 30 = ([marker], nid + 1, [(nid, Array (x:xs))])\n",
        "  | sz <= bud = let (r, nid', s) = truncArr (bud - sz - 2) nid xs in (x : r, nid', s)\n",
        "  | otherwise = let m = String (\"[~\" <> showI sz <> \" chars -> stub_\" <> showI nid <> \"]\")\n",
        "                    (r, nid', s) = truncArr (bud - 50) (nid + 1) xs\n",
        "                in (m : r, nid', (nid, x) : s)\n",
        "  where sz = valSize x\n",
        "        n = 1 + length xs\n",
        "        tsz = sz + arrSz xs 0\n",
        "        marker = String (\"[\" <> showI n <> \" more, ~\" <> showI tsz <> \" chars -> stub_\" <> showI nid <> \"]\")\n",
    ));
    out.push_str(concat!(
        "truncKvs :: Int -> Int -> [(Key, Value)] -> ([(Key, Value)], Int, [(Int, Value)])\n",
        "truncKvs _ nid [] = ([], nid, [])\n",
        "truncKvs bud nid ((k,v):rest)\n",
        "  | bud <= 30 = ([(KM.fromText \"...\", String marker)], nid + 1, [(nid, object (map (\\(k',v') -> KM.toText k' .= v') ((k,v):rest)))])\n",
        "  | sz <= bud = let (r, nid', s) = truncKvs (bud - sz - 2) nid rest in ((k,v) : r, nid', s)\n",
        "  | otherwise = let m = String (\"[~\" <> showI (valSize v) <> \" chars -> stub_\" <> showI nid <> \"]\")\n",
        "                    (r, nid', s) = truncKvs (bud - 50) (nid + 1) rest\n",
        "                in ((k, m) : r, nid', (nid, v) : s)\n",
        "  where sz = T.length (KM.toText k) + 4 + valSize v\n",
        "        n = 1 + length rest\n",
        "        tsz = sz + objSz rest 0\n",
        "        marker = \"[\" <> showI n <> \" more fields, ~\" <> showI tsz <> \" chars -> stub_\" <> showI nid <> \"]\"\n",
    ));
    out.push_str(concat!(
        "truncGo :: Int -> Int -> Value -> (Value, Int, [(Int, Value)])\n",
        "truncGo bud nid v\n",
        "  | valSize v <= bud = (v, nid, [])\n",
        "  | otherwise = case v of\n",
        "      Array xs -> let (items, nid', stubs) = truncArr bud nid xs in (Array items, nid', stubs)\n",
        "      Object m -> let (pairs, nid', stubs) = truncKvs bud nid (KM.toList m)\n",
        "                  in (object (map (\\(k',v') -> KM.toText k' .= v') pairs), nid', stubs)\n",
        "      String t -> let keep = max' 10 (bud - 30)\n",
        "                  in (String (T.take keep t <> \"...[\" <> showI (T.length t) <> \" chars]\"), nid, [])\n",
        "      _ -> (v, nid, [])\n",
    ));
    out.push_str(concat!(
        "truncVal :: Int -> Value -> (Value, [(Int, Value)])\n",
        "truncVal budget val = let (v, _, stubs) = truncGo budget 0 val in (v, stubs)\n",
    ));
    out.push_str(concat!(
        "lookupStub :: Int -> [(Int, Value)] -> Maybe Value\n",
        "lookupStub _ [] = Nothing\n",
        "lookupStub sid ((k,v):rest) = if sid == k then Just v else lookupStub sid rest\n",
    ));

    out.push_str(concat!(
        "renderJson :: Value -> Text\n",
        "renderJson v = case v of\n",
        "  Object m -> \"{\" <> T.intercalate \",\" (map (\\(k,v') -> \"\\\"\" <> KM.toText k <> \"\\\":\" <> renderJson v') (KM.toList m)) <> \"}\"\n",
        "  Array xs -> \"[\" <> T.intercalate \",\" (map renderJson xs) <> \"]\"\n",
        "  String t -> \"\\\"\" <> T.concatMap (\\c -> case c of { '\\\\' -> \"\\\\\\\\\"; '\"' -> \"\\\\\\\"\"; '\\n' -> \"\\\\n\"; '\\t' -> \"\\\\t\"; '\\r' -> \"\\\\r\"; _ -> T.singleton c }) t <> \"\\\"\"\n",
        "  Number n -> show n\n",
        "  NumberI n -> show n\n",
        "  Bool b -> if b then \"true\" else \"false\"\n",
        "  Null -> \"null\"\n",
    ));

    // paginate* reference `M`, which only exists on a non-empty stack
    // (`type M = Eff '[…]` is emitted only then). Gate accordingly.
    if !effects.is_empty() {
        // Interactive: suspend to the caller via `ask` to fetch stub chunks.
        if has_ask {
            out.push_str(concat!(
                "paginateInteractive :: Int -> Value -> M Value\n",
                "paginateInteractive budget val\n",
                "  | valSize val <= budget = pure val\n",
                "  | otherwise = do\n",
                "      let (truncated, stubs) = truncVal budget val\n",
                "      case stubs of\n",
                "        [] -> pure truncated\n",
                "        _ -> do\n",
                "          let stubInfo = Array (map (\\(sid, sv) -> object [\"id\" .= (\"stub_\" <> showI sid), \"size\" .= toJSON (valSize sv)]) stubs)\n",
                "          resp <- ask SStr (\"[Pagination] truncated: \" <> renderJson truncated <> \" stubs: \" <> renderJson stubInfo <> \" | Reply with a stub id (e.g. stub_0) to fetch that chunk; any other reply ends pagination and returns the current chunk.\")\n",
                "          case resp ^? _String of\n",
                "            Just s -> case parseIntM (T.drop 5 s) of\n",
                "              Just sid -> case lookupStub sid stubs of\n",
                "                Just subtree -> paginateInteractive budget subtree\n",
                "                Nothing -> pure truncated\n",
                "              Nothing -> pure truncated\n",
                "            _ -> pure truncated\n",
            ));
        }
        // Truncate-only: always emitted so every non-empty stack has a
        // paginateTrunc. Console variant emits a note (needs putStrLn); the
        // pure variant truncates silently when there is no Console.
        if has_console {
            out.push_str(concat!(
                "paginateTrunc :: Int -> Value -> M Value\n",
                "paginateTrunc budget val\n",
                "  | valSize val <= budget = pure val\n",
                "  | otherwise = do\n",
                "      let (truncated, _) = truncVal budget val\n",
                "      putStrLn \"[truncated \u{2014} bind the result and re-query]\"\n",
                "      pure truncated\n",
            ));
        } else {
            out.push_str(concat!(
                "paginateTrunc :: Int -> Value -> M Value\n",
                "paginateTrunc budget val\n",
                "  | valSize val <= budget = pure val\n",
                "  | otherwise = let (truncated, _) = truncVal budget val in pure truncated\n",
            ));
        }
    }

    // KV + file orchestration helpers — each gated on its effect so the module
    // compiles on any stack. (Previously gated behind `user_library`; now a pure
    // fn of the effect set, so they're available in every eval.)
    if has_kv {
        out.push_str("-- KV orchestration helpers\n");
        out.push_str(concat!(
            "memo :: Text -> M Value -> M Value\n",
            "memo k compute = do\n",
            "  cached <- kvGet k\n",
            "  case cached of\n",
            "    Just v  -> pure v\n",
            "    Nothing -> do { v <- compute; kvSet k v; pure v }\n",
        ));
        out.push_str(concat!(
            "kvModify :: Text -> (Maybe Value -> Value) -> M Value\n",
            "kvModify k f = do\n",
            "  old <- kvGet k\n",
            "  let new = f old\n",
            "  kvSet k new\n",
            "  pure new\n",
        ));
        out.push_str(concat!(
            "kvIncr :: Text -> M Int\n",
            "kvIncr k = do\n",
            "  old <- kvGet k\n",
            "  let n = case old >>= (^? _Int) of { Just i -> i; _ -> 0 }\n",
            "  let n' = n + 1\n",
            "  kvSet k (toJSON n')\n",
            "  pure n'\n",
        ));
        out.push_str(concat!(
            "kvAppend :: Text -> Value -> M [Value]\n",
            "kvAppend k v = do\n",
            "  old <- kvGet k\n",
            "  let xs = case old >>= (^? _Array) of { Just arr -> arr; _ -> [] }\n",
            "  let xs' = xs ++ [v]\n",
            "  kvSet k (toJSON xs')\n",
            "  pure xs'\n",
        ));
        out.push_str(concat!(
            "kvAll :: M [(Text, Value)]\n",
            "kvAll = do\n",
            "  ks <- kvKeys\n",
            "  vs <- mapM kvGet ks\n",
            "  pure (zipWith (\\k mv -> (k, maybe Null id mv)) ks vs)\n",
        ));
        out.push_str(concat!(
            "kvClear :: M ()\n",
            "kvClear = kvKeys >>= mapM_ kvDel\n",
        ));
    }
    if has_fs {
        out.push_str("-- File orchestration helpers\n");
        out.push_str(concat!(
            "mapFiles :: [Text] -> (Text -> Text -> M Text) -> M [Text]\n",
            "mapFiles paths transform = mapM (\\p -> do\n",
            "  content <- readFile p\n",
            "  result <- transform p content\n",
            "  writeFile p result\n",
            "  pure p) paths\n",
        ));
        out.push_str(concat!(
            "readGlob :: Text -> M [Doc]\n",
            "readGlob pat = glob pat >>= mapM (\\p -> Doc p <$> readFile p)\n",
        ));
        out.push_str(concat!(
            "mapFile :: Text -> (Text -> Text) -> M ()\n",
            "mapFile path f = readFile path >>= \\c -> writeFile path (f c)\n",
        ));
        out.push_str(concat!(
            "mapFileM :: Text -> (Text -> M Text) -> M ()\n",
            "mapFileM path f = readFile path >>= f >>= writeFile path\n",
        ));
        out.push_str(concat!(
            "searchFiles :: Text -> Text -> M [Hit]\n",
            "searchFiles pat needle = do\n",
            "  files <- glob pat\n",
            "  fmap concat $ forM files $ \\p -> do\n",
            "    content <- readFile p\n",
            "    let ls = zip [(1::Int)..] (T.lines content)\n",
            "    pure [Hit p n l | (n, l) <- ls, T.isInfixOf needle l]\n",
        ));
        out.push_str(concat!(
            "lineCount :: Text -> M Int\n",
            "lineCount path = length . T.lines <$> readFile path\n",
        ));
        out.push_str(concat!(
            "fileContains :: Text -> Text -> M Bool\n",
            "fileContains path needle = T.isInfixOf needle <$> readFile path\n",
        ));
    }
    if has_sg {
        out.push_str(concat!(
            "searchProcess :: Lang -> Text -> [Text] -> (Match -> M a) -> M [a]\n",
            "searchProcess lang pat paths process = do\n",
            "  matches <- sgFind lang pat paths\n",
            "  mapM process matches\n",
        ));
    }
    if has_exec {
        out.push_str("runChecked :: Text -> M Text\nrunChecked = readProcess\n");
        out.push_str(concat!(
            "runAll :: [Text] -> M [Proc]\n",
            "runAll = mapM run\n",
        ));
    }

    out
}

/// Emit the mode-selected `paginateResult` alias into the eval expr module.
/// `Tidepool.Orchestrate` exports `paginateInteractive` and/or `paginateTrunc`;
/// the result wrapper in `eval_prep.rs` calls `paginateResult`, so the expr
/// module aliases one of them by mode. Interactive eval with an `Ask` effect
/// suspends (`paginateInteractive`); the repl + every Ask-less stack truncates
/// (`paginateTrunc`, always emitted for a non-empty stack). Emitted only when
/// the stack has effects — `M`/`paginateResult` are otherwise unused.
fn paginate_alias(out: &mut String, effects: &[EffectDecl], interactive: bool) {
    if effects.is_empty() {
        return;
    }
    let has_ask = effects.iter().any(|e| e.type_name == "Ask");
    let target = if interactive && has_ask {
        "paginateInteractive"
    } else {
        "paginateTrunc"
    };
    out.push_str("paginateResult :: Int -> Value -> M Value\n");
    out.push_str(&format!("paginateResult = {target}\n"));
    out.push('\n');
}

/// Assemble the full Haskell preamble for an eval: module header + imports
/// (incl. `import Tidepool.Orchestrate`) and the mode-selected `paginateResult`
/// alias. The helper bodies live in the imported `Tidepool.Orchestrate` module,
/// not spliced here (the namespace-poison fix).
pub fn build_preamble(effects: &[EffectDecl], user_library: bool) -> String {
    let mut out = String::new();
    pragmas_and_imports(&mut out, effects, user_library);
    paginate_alias(&mut out, effects, true);
    out
}

/// Like [`build_preamble`] but with non-interactive (truncate-only) pagination:
/// oversized results are truncated in-place with a marker instead of suspending
/// via `ask`. Used by `tidepool-repl` where bindings persist across turns and
/// the caller can re-query a truncated result by its bound name.
pub fn build_preamble_non_interactive(effects: &[EffectDecl], user_library: bool) -> String {
    let mut out = String::new();
    pragmas_and_imports(&mut out, effects, user_library);
    paginate_alias(&mut out, effects, false);
    out
}

/// Qualified aeson imports for MCP eval. Unqualified symbols now come from Tidepool.Prelude.
/// These provide `Aeson.` prefix (used by json_to_haskell for input injection) and
/// qualified access to KeyMap/Vector for power users.
pub fn aeson_imports() -> String {
    concat!(
        "qualified Tidepool.Aeson as Aeson\n",
        "qualified Tidepool.Aeson.KeyMap as KM\n",
    )
    .into()
}

pub(crate) fn build_eval_tool_description(effects: &[EffectDecl]) -> String {
    let mut desc = String::from(concat!(
        "`code` is a single Haskell EXPRESSION of type `M a`; its value is the ",
        "eval's result. The server wraps it in a module with the effect stack, ",
        "pragmas, and imports. Compose with `>>=`, `<&>`, `>=>`, point-free ",
        "pipelines; attach a trailing `where` for local bindings. For ",
        "step-by-step sequencing write an explicit `do` block — bare statement ",
        "lines do NOT parse. ",
        "Use `send (Constructor args)` to invoke effects. ",
        "First call is slow (~2s). Subsequent calls are cached.\n",
        "Qualified namespaces always in scope: T. (Data.Text), L. (Data.List), ",
        "Map. (Data.Map.Strict), MM. (Data.Map.Merge.Strict), Set. (Data.Set), ",
        "KM. (Tidepool.Aeson.KeyMap), TF. (Tidepool.TextFormat), Tab. (Tidepool.Table), ",
        "P. (Prelude) \u{2014} ",
        "prefer the unqualified Prelude shadows where they exist (they are ",
        "the JIT-safe versions).\n",
        "The final value of `code` is rendered to JSON for the caller \u{2014} Int → ",
        "number, [Char] → string, Bool → true/false, lists → arrays, and a ",
        "`Value` → that JSON directly. In the REPL (`session_run`), results render ",
        "via `Show` by default — `Text` is bare, custom ADTs work without `ToJSON`. ",
        "JSON is opt-in: return a `Value` (via ",
        "`object`/`toJSON`/`parseJson`/`llm`/",
        "`tryHttpGet`, …) for structured output, e.g. ",
        "`tryHttpGet \"https://api.github.com/repos/o/r\"`; use `putStrLn`/`say` only ",
        "for human-readable debug traces, not to stringify results. Extract from a ",
        "`Value` with optics: `v ^? key \"f\" . _String` (also `_Int`, `_Double`, ",
        "`_Bool`, `_Array`); `renderJson :: Value -> Text` renders one to compact JSON. ",
        "Prefer `pure x` over `send (Print (show x))`.\n",
        "The `input` param is the PAYLOAD LANE: pass large or quote-heavy ",
        "content (file bodies, generated source) as a real JSON value there \u{2014} ",
        "no Haskell string escaping \u{2014} and keep `code` a short verb consuming ",
        "the `input` binding. E.g. whole-file writes: code = ",
        "`writeFile \".tidepool/lib/Mod.hs\" src where src = case input of { String s -> s; _ -> \"\" }` ",
        "with the file content in `input`.",
    ));

    if !effects.is_empty() {
        desc.push_str(concat!(
            "\nPrefer typed effects for common operations: `glob`/`grepGlob` (Fs) for ",
            "filesystem search, `sgFind` (SG) for structural code search, `lspWhere`/",
            "`lspDefs` (Lsp) for symbol navigation. Use `run \"...\"` only as a shell ",
            "fallback for things the typed effects don\u{2019}t cover.\n",
            "Effects (invoke via the helper verbs; read tidepool://effect/{name} for each one\u{2019}s constructors + helpers):\n",
        ));
        for eff in effects {
            desc.push_str(&format!("  {}: {}\n", eff.type_name, eff.description));
        }

        let has_llm = effects.iter().any(|e| e.type_name == "Llm");
        let has_ask = effects.iter().any(|e| e.type_name == "Ask");
        if has_llm && has_ask {
            desc.push_str(concat!(
                "\nStructured LLM / Ask (one Schema vocabulary; full detail in tidepool://schema):\n",
                "  Schema = SObj [(Text,Schema)] | SArr Schema | SStr | SNum | SBool | SEnum [Text] | SOpt Schema\n",
                "  ask schema prompt  -- SUSPEND to the caller; reply validated vs schema, no token burn\n",
                "  llm schema prompt  -- AUTONOMOUS server-side call (costs tokens); structured Value, no fences\n",
                "  extract with optics, e.g. llm (SObj [(\"k\", SEnum [\"a\",\"b\"])]) p <&> (^? key \"k\" . _String)\n",
            ));
        }

        desc.push_str(concat!(
            "\nEdit files: `update path old new` — exact str-replace, errors if the text is ",
            "not-found or ambiguous (add surrounding context); `planUpdate` previews the diff. ",
            "The full editing surface (Edit DSL, diffs, ast-grep) is in tidepool://edits.\n",
        ));

        desc.push_str(concat!(
            "\nResources — this description is a FLOOR; pull the depth on demand via resources/read:\n",
            "  tidepool://guide           full guide: returning JSON, the input lane, examples, failure isolation\n",
            "  tidepool://effect/{name}   per-effect constructors, types, and helper signatures\n",
            "  tidepool://schema          the Schema grammar + ask/llm/tryLlm in full\n",
            "  tidepool://edits           the declarative Edit verb JSON schema\n",
            "  tidepool://vocab           live project-library verb signatures (.tidepool/lib)\n",
            "  tidepool://patterns        worked examples\n",
            "  tidepool://stdlib/{module} vendored stdlib module source (e.g. Tidepool.Prelude)\n",
        ));
    }

    desc
}

/// True if `line` opens a top-level type signature (`name ::` or
/// `(op) ::` at column 0) — comment lines and indented code never match.
fn sig_start(line: &str) -> bool {
    if line.starts_with(char::is_whitespace) {
        return false;
    }
    let Some((head, _)) = line.split_once("::") else {
        return false;
    };
    let h = head.trim_end();
    if h.is_empty() || h.contains(' ') {
        return false;
    }
    (h.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
        && h.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '\''))
        || (h.starts_with('(') && h.ends_with(')'))
}

/// Extract top-level type signatures (joining indented continuation
/// lines) plus `data`/`type` heads from Haskell source.
pub(crate) fn extract_sigs(src: &str) -> Vec<String> {
    let mut sigs: Vec<String> = Vec::new();
    let mut cur: Option<String> = None;
    for line in src.lines() {
        if sig_start(line) {
            if let Some(s) = cur.take() {
                sigs.push(s);
            }
            cur = Some(line.to_string());
        } else if let Some(s) = cur.as_mut() {
            let t = line.trim();
            // Indented continuation of a multi-line signature.
            if line.starts_with(char::is_whitespace) && !t.is_empty() && !t.starts_with("--") {
                s.push(' ');
                s.push_str(t);
            } else {
                sigs.push(cur.take().unwrap());
            }
        } else if (line.starts_with("data ") || line.starts_with("type "))
            && !line.contains("where")
        {
            sigs.push(line.to_string());
        }
    }
    if let Some(s) = cur.take() {
        sigs.push(s);
    }
    sigs
}

/// Parse the `module Library ( module A, module B, … ) where` re-export list
/// to learn which verb modules are actually IN SCOPE bare (the auto-imported
/// `Library` facade re-exports a curated subset — sibling modules like
/// `RustAudit`/`MechDemo` are excluded, usually because their names would clash).
/// Returns the set of re-exported module stems, or `None` if no `Library.hs` is
/// found / its export list can't be parsed (callers fall back to listing all).
fn library_inscope_modules(
    dirs: &[std::path::PathBuf],
) -> Option<std::collections::HashSet<String>> {
    for dir in dirs {
        let path = dir.join("Library.hs");
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mods = parse_library_exports(&src);
        if !mods.is_empty() {
            return Some(mods);
        }
    }
    None
}

/// Pure parse of a `module Library ( module A, module B, … ) where` header into
/// the set of re-exported module stems. Returns empty if the header is absent.
fn parse_library_exports(src: &str) -> std::collections::HashSet<String> {
    let mut mods = std::collections::HashSet::new();
    // The export list runs from `module Library` to the closing `)`.
    let Some(after) = src.split_once("module Library").map(|(_, r)| r) else {
        return mods;
    };
    let list = after.split_once(')').map(|(l, _)| l).unwrap_or(after);
    for raw in list.split(',') {
        // Each entry looks like `module Schemes` (the FIRST also carries the
        // opening `(`, e.g. `( module Schemes`); strip the paren, then take the
        // token after `module`.
        let entry = raw.trim().trim_start_matches('(').trim();
        if let Some(rest) = entry.strip_prefix("module ") {
            if let Some(name) = rest.split_whitespace().next() {
                mods.insert(name.to_string());
            }
        }
    }
    mods
}

/// Scan a user-library directory for top-level type signatures (plus
/// `data`/`type` heads) and render a per-module vocabulary digest for
/// the eval tool description. This is the affordance that keeps eval
/// code shape-first: the combinators a user would otherwise re-invent
/// are visible at every call site instead of requiring a read of the
/// lib sources. Snapshot at server start; restart to refresh.
///
/// Only modules actually re-exported by the `Library` facade (hence in scope
/// bare) are listed — otherwise the digest would advertise verbs that fail with
/// "not in scope" (e.g. `RustAudit.panicSites`). Falls back to listing every
/// module when no parseable `Library.hs` is present.
///
/// `only`, when `Some(module)`, scopes the digest to that one module — a
/// deliberate `:vocab <module>` ask bypasses the in-scope gate (the caller
/// explicitly named it, so "not auto-imported" isn't a reason to hide it) and
/// returns an explicit "no module" note on no match instead of an empty blob.
pub fn library_vocab(dirs: &[std::path::PathBuf], only: Option<&str>) -> String {
    // Diagnostic modules, not vocabulary.
    const SKIP: &[&str] = &["Probe", "SelfTest"];
    const SIG_MAX: usize = 120;
    const TOTAL_MAX: usize = 8000;

    // Modules in scope bare (re-exported by `Library`); `None` ⇒ list all.
    let inscope = library_inscope_modules(dirs);

    // Layered across dirs (project first, then global): a module name seen in an
    // earlier dir shadows the same name later, so a project module overrides a
    // global one — matching GHC's first-match-wins include search.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut found = false;
    let mut out = String::new();
    for dir in dirs {
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().is_some_and(|x| x == "hs"))
                    .collect()
            })
            .unwrap_or_default();
        files.sort();

        for path in files {
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if SKIP.contains(&stem) || !seen.insert(stem.to_string()) {
                continue;
            }
            if let Some(m) = only {
                if stem != m {
                    continue;
                }
            } else if let Some(ref ins) = inscope {
                // Only list modules the `Library` facade re-exports (in scope
                // bare). `Library` itself has no sigs of its own, so it's
                // naturally absent. An explicit `only` bypasses this gate.
                if stem != "Library" && !ins.contains(stem) {
                    continue;
                }
            }
            let Ok(src) = std::fs::read_to_string(&path) else {
                continue;
            };
            let sigs = extract_sigs(&src);
            if sigs.is_empty() {
                continue;
            }
            found = true;
            out.push_str(&format!("  {stem}:\n"));
            for s in sigs {
                let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
                let s: String = s.chars().take(SIG_MAX).collect();
                out.push_str(&format!("    {s}\n"));
                if out.len() > TOTAL_MAX {
                    out.push_str("  …(truncated)\n");
                    return out;
                }
            }
        }
    }
    if let Some(m) = only {
        if !found {
            return format!("  no module '{m}' found in the library search path\n");
        }
    }
    out
}

#[cfg(test)]
mod vocab_tests {
    use super::parse_library_exports;

    #[test]
    fn parses_reexport_list_and_excludes_siblings() {
        // Mirrors the real `.tidepool/lib/Library.hs` header shape.
        let src = "\
-- | Re-export facade.
module Library
  ( module Schemes
  , module Explore
  , module Lsp
  ) where

import Schemes
import Explore
import Lsp
";
        let mods = parse_library_exports(src);
        assert!(mods.contains("Schemes"), "Schemes should be in scope");
        assert!(mods.contains("Explore"));
        assert!(mods.contains("Lsp"));
        // A sibling module that exists on disk but is NOT re-exported (the
        // `RustAudit.panicSites`-not-in-scope friction) must be absent.
        assert!(!mods.contains("RustAudit"), "RustAudit is not re-exported");
        assert_eq!(mods.len(), 3);
    }

    #[test]
    fn absent_header_yields_empty_set() {
        // No `module Library` header ⇒ empty ⇒ library_vocab falls back to all.
        assert!(parse_library_exports("module Schemes where\nfoo :: Int\n").is_empty());
    }

    /// A self-contained lib dir (no repo dependency) for `library_vocab` scoping tests.
    struct FixtureDir(std::path::PathBuf);
    impl FixtureDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "tidepool-vocab-fixture-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::create_dir_all(&dir).expect("create fixture dir");
            std::fs::write(
                dir.join("Alpha.hs"),
                "module Alpha where\n\nfooAlpha :: Int -> Int\n",
            )
            .unwrap();
            std::fs::write(
                dir.join("Beta.hs"),
                "module Beta where\n\nfooBeta :: Text -> Text\n",
            )
            .unwrap();
            // No `Library.hs` here ⇒ `inscope` is `None` ⇒ unscoped `:vocab` lists all.
            FixtureDir(dir)
        }
    }
    impl Drop for FixtureDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn library_vocab_scoped_to_one_module() {
        let fixture = FixtureDir::new("scoped");
        let dirs = vec![fixture.0.clone()];

        let all = super::library_vocab(&dirs, None);
        assert!(all.contains("Alpha:"), "unscoped should list Alpha: {all}");
        assert!(all.contains("Beta:"), "unscoped should list Beta: {all}");

        let alpha_only = super::library_vocab(&dirs, Some("Alpha"));
        assert!(
            alpha_only.contains("fooAlpha"),
            "scoped to Alpha should show fooAlpha: {alpha_only}"
        );
        assert!(
            !alpha_only.contains("Beta") && !alpha_only.contains("fooBeta"),
            "scoped to Alpha must NOT show Beta: {alpha_only}"
        );
    }

    #[test]
    fn library_vocab_scoped_to_missing_module_reports_not_found() {
        let fixture = FixtureDir::new("missing");
        let dirs = vec![fixture.0.clone()];

        let out = super::library_vocab(&dirs, Some("NoSuchModule"));
        assert!(
            out.contains("no module 'NoSuchModule' found"),
            "missing module should report clearly, not silently return empty: {out}"
        );
    }
}
