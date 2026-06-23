//! Preamble, tool-description, and library-vocab assembly for the MCP server.
//!
//! [`build_preamble`] emits the Haskell module header (pragmas + imports) plus
//! the generated pagination and orchestration helper bodies that every eval
//! sees. [`build_eval_tool_description`] renders the `eval` tool's human-facing
//! description from the live effect set, and [`library_vocab`] digests a
//! user-library directory into a per-module signature index.
//!
//! `build_preamble` is decomposed into named section builders
//! (`pragmas_and_imports`, `pagination_helpers`, `orchestration_helpers`) so the
//! emitted-source seams are legible; the concatenation is byte-identical to the
//! original monolithic function.

use crate::EffectDecl;

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
fn pragmas_and_imports(out: &mut String, user_library: bool) {
    out.push_str("{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables, ExtendedDefaultRules, LambdaCase, TupleSections, MultiWayIf, RecordWildCards, NamedFieldPuns, ViewPatterns, BangPatterns, TypeApplications, BlockArguments, NumericUnderscores, MultilineStrings, DeriveFunctor, DeriveFoldable, DeriveTraversable, QuasiQuotes #-}\n");
    out.push_str("module Expr where\n");
    out.push_str("import Tidepool.Prelude hiding (error)\n");
    // Effect GADTs, `M`, the `error` shadow, and the send-wrapper helpers
    // live in the generated Tidepool.Effects module (see
    // `effects_module_source`) so .tidepool/lib modules can import the
    // SAME types and define effectful verbs.
    out.push_str("import Tidepool.Effects\n");
    out.push_str("import qualified Tidepool.Data.Text as T\n");
    out.push_str("import qualified Data.Map.Strict as Map\n");
    // Merge/reconcile API (merge, zipWithMatched, mapMissing, …) — strict, to
    // match Map. Needs its own qualifier; not covered by the Map. submodule.
    out.push_str("import qualified Data.Map.Merge.Strict as MM\n");
    out.push_str("import qualified Data.Set as Set\n");
    out.push_str("import qualified Tidepool.Aeson.KeyMap as KM\n");
    out.push_str("import qualified Data.List as L\n");
    out.push_str("import qualified Tidepool.TextFormat as TF\n");
    out.push_str("import qualified Tidepool.Table as Tab\n");
    out.push_str("import Control.Monad.Freer hiding (run)\n");
    if user_library {
        out.push_str("import Library\n");
    }
    out.push_str("import qualified Prelude as P\n");
    out.push_str("default (Int, Double, Text)\n");
    out.push('\n');
}

/// Emit the pagination / auto-truncation helper bodies (`putStrLn`, `valSize`,
/// the `trunc*` family, `renderJson`, `paginateResult`). Console/KV/Ask
/// presence selects the `putStrLn` and `paginateResult` variants.
fn pagination_helpers(out: &mut String, effects: &[EffectDecl]) {
    if effects.is_empty() {
        return;
    }
    let has_ask = effects.iter().any(|e| e.type_name == "Ask");
    let has_console = effects.iter().any(|e| e.type_name == "Console");
    let has_kv = effects.iter().any(|e| e.type_name == "KV");

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

    out.push_str(concat!(
        "valSize :: Value -> Int\n",
        "valSize v = case v of\n",
        "  String t -> T.length t + 2\n",
        "  Number _ -> 8\n",
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
        "  Bool b -> if b then \"true\" else \"false\"\n",
        "  Null -> \"null\"\n",
    ));

    if has_ask {
        out.push_str(concat!(
            "paginateResult :: Int -> Value -> M Value\n",
            "paginateResult budget val\n",
            "  | valSize val <= budget = pure val\n",
            "  | otherwise = do\n",
            "      let (truncated, stubs) = truncVal budget val\n",
            "      case stubs of\n",
            "        [] -> pure truncated\n",
            "        _ -> do\n",
            "          let stubInfo = Array (map (\\(sid, sv) -> object [\"id\" .= (\"stub_\" <> showI sid), \"size\" .= toJSON (valSize sv)]) stubs)\n",
            "          resp <- ask (\"[Pagination] truncated: \" <> renderJson truncated <> \" stubs: \" <> renderJson stubInfo <> \" | Reply with a stub id (e.g. stub_0) to fetch that chunk; any other reply ends pagination and returns the current chunk.\")\n",
            "          case resp ^? _String of\n",
            "            Just s -> case parseIntM (T.drop 5 s) of\n",
            "              Just sid -> case lookupStub sid stubs of\n",
            "                Just subtree -> paginateResult budget subtree\n",
            "                Nothing -> pure truncated\n",
            "              Nothing -> pure truncated\n",
            "            _ -> pure truncated\n",
        ));
    } else {
        out.push_str(concat!(
            "paginateResult :: Int -> Value -> M Value\n",
            "paginateResult budget val\n",
            "  | valSize val <= budget = pure val\n",
            "  | otherwise = let (truncated, _) = truncVal budget val in pure truncated\n",
        ));
    }
    out.push('\n');
}

/// Emit the Library-preamble effect-orchestration helpers (`converse`,
/// `askUntil`, `memo`, the `kv*` combinators, file/process verbs, …). Only
/// emitted when a user library is present AND the stack has effects.
fn orchestration_helpers(out: &mut String, effects: &[EffectDecl], user_library: bool) {
    if !user_library || effects.is_empty() {
        return;
    }
    out.push_str("-- Effect orchestration (from Library preamble)\n");
    out.push_str(concat!(
        "converse :: (s -> Value -> Either a (Text, s)) -> Text -> s -> M a\n",
        "converse decide firstQ s0 = do\n",
        "  v <- ask firstQ\n",
        "  case decide s0 v of\n",
        "    Left a        -> pure a\n",
        "    Right (q, s') -> converse decide q s'\n",
    ));
    out.push_str(concat!(
        "askUntil :: (Value -> Maybe a) -> Text -> M a\n",
        "askUntil check prompt = do\n",
        "  v <- ask prompt\n",
        "  case check v of\n",
        "    Just a  -> pure a\n",
        "    Nothing -> askUntil check (prompt <> \" (invalid, try again)\")\n",
    ));
    out.push_str(concat!(
        "askChoice :: Text -> [(Text, a)] -> M a\n",
        "askChoice prompt choices = do\n",
        "  let choiceText = T.intercalate \", \" (map fst choices)\n",
        "  v <- ask (prompt <> \" [\" <> choiceText <> \"]\")\n",
        "  let answer = case v ^? _String of { Just s -> s; _ -> \"\" }\n",
        "  case lookup answer choices of\n",
        "    Just a  -> pure a\n",
        "    Nothing -> askChoice prompt choices\n",
    ));
    out.push_str(concat!(
        "confirm :: Text -> M Bool\n",
        "confirm prompt = do\n",
        "  v <- ask (prompt <> \" [yes/no]\")\n",
        "  let answer = case v ^? _String of { Just s -> toLower s; _ -> \"\" }\n",
        "  pure (answer == \"yes\" || answer == \"y\")\n",
    ));
    out.push_str(concat!(
        "repl :: Text -> (Text -> M (Maybe a)) -> M a\n",
        "repl prompt dispatch = do\n",
        "  v <- ask prompt\n",
        "  let cmd = case v ^? _String of { Just s -> s; _ -> \"\" }\n",
        "  r <- dispatch cmd\n",
        "  case r of\n",
        "    Just a  -> pure a\n",
        "    Nothing -> repl prompt dispatch\n",
    ));
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
        "supervised :: Text -> M Value -> (Value -> Maybe a) -> M a\n",
        "supervised label body check = do\n",
        "  putStrLn (\"[\" <> label <> \"] running...\")\n",
        "  v <- body\n",
        "  case check v of\n",
        "    Just a  -> putStrLn (\"[\" <> label <> \"] done\") >> pure a\n",
        "    Nothing -> do\n",
        "      correction <- ask (\"[\" <> label <> \"] result: \" <> show v <> \"\\nHow should I adjust?\")\n",
        "      supervised label body check\n",
    ));
    out.push_str(concat!(
        "gather :: [(Text, Value -> a)] -> M [a]\n",
        "gather [] = pure []\n",
        "gather ((q, parse):rest) = do\n",
        "  v <- ask q\n",
        "  as <- gather rest\n",
        "  pure (parse v : as)\n",
    ));
    out.push_str(concat!(
        "mapFiles :: [Text] -> (Text -> Text -> M Text) -> M [Text]\n",
        "mapFiles paths transform = mapM (\\p -> do\n",
        "  content <- readFile p\n",
        "  result <- transform p content\n",
        "  writeFile p result\n",
        "  pure p) paths\n",
    ));
    out.push_str(concat!(
        "searchProcess :: Lang -> Text -> [Text] -> (Match -> M a) -> M [a]\n",
        "searchProcess lang pat paths process = do\n",
        "  matches <- sgFind lang pat paths\n",
        "  mapM process matches\n",
    ));
    out.push_str(concat!(
        "readGlob :: Text -> M [(Text, Text)]\n",
        "readGlob pat = glob pat >>= mapM (\\p -> (,) p <$> readFile p)\n",
    ));
    out.push_str("runChecked :: Text -> M Text\nrunChecked = readProcess\n");
    out.push_str(concat!(
        "mapFile :: Text -> (Text -> Text) -> M ()\n",
        "mapFile path f = readFile path >>= \\c -> writeFile path (f c)\n",
    ));
    out.push_str(concat!(
        "mapFileM :: Text -> (Text -> M Text) -> M ()\n",
        "mapFileM path f = readFile path >>= f >>= writeFile path\n",
    ));
    out.push_str(concat!(
        "searchFiles :: Text -> Text -> M [(Text, Int, Text)]\n",
        "searchFiles pat needle = do\n",
        "  files <- glob pat\n",
        "  fmap concat $ forM files $ \\path -> do\n",
        "    content <- readFile path\n",
        "    let ls = zip [(1::Int)..] (T.lines content)\n",
        "    pure [(path, n, l) | (n, l) <- ls, T.isInfixOf needle l]\n",
    ));
    out.push_str(concat!(
        "lineCount :: Text -> M Int\n",
        "lineCount path = length . T.lines <$> readFile path\n",
    ));
    out.push_str(concat!(
        "fileContains :: Text -> Text -> M Bool\n",
        "fileContains path needle = T.isInfixOf needle <$> readFile path\n",
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
    out.push_str(concat!(
        "runAll :: [Text] -> M [(Int, Text, Text)]\n",
        "runAll = mapM run\n",
    ));

    // Q-builders (pick/yn/obj/txt/num/bar) and the named runner `askQ`
    // live in the generated Tidepool.Effects module (ask_decl type_defs +
    // helpers) — always present, since the Ask effect is always in the
    // stack — so .tidepool/lib modules can define Q-driven verbs.

    out.push('\n');
}

/// Assemble the full Haskell preamble for an eval: module header + imports,
/// pagination helpers, and (when a user library is present) the orchestration
/// helper set. The emitted string is concatenated from the section builders.
pub fn build_preamble(effects: &[EffectDecl], user_library: bool) -> String {
    let mut out = String::new();
    pragmas_and_imports(&mut out, user_library);
    pagination_helpers(&mut out, effects);
    orchestration_helpers(&mut out, effects, user_library);
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
        "Return values are automatically rendered to JSON by the Rust runtime \u{2014} ",
        "Int becomes a number, [Char] becomes a string, Bool becomes true/false, ",
        "lists become arrays, etc. Prefer `pure x` over `send (Print (show x))` ",
        "for returning results.\n",
        "The `input` param is the PAYLOAD LANE: pass large or quote-heavy ",
        "content (file bodies, generated source) as a real JSON value there \u{2014} ",
        "no Haskell string escaping \u{2014} and keep `code` a short verb consuming ",
        "the `input` binding. E.g. whole-file writes: code = ",
        "`writeFile \".tidepool/lib/Mod.hs\" src where src = case input of { String s -> s; _ -> \"\" }` ",
        "with the file content in `input`.",
    ));

    if !effects.is_empty() {
        desc.push_str("\nAvailable effects (use `send` to invoke):\n");
        effects.iter().for_each(|eff| {
            desc.push_str(&format!("\n{}: {}\n", eff.type_name, eff.description));
        });

        // List built-in helpers
        let has_console = effects.iter().any(|e| e.type_name == "Console");
        let has_helpers = has_console || effects.iter().any(|e| !e.helpers.is_empty());
        if has_helpers {
            desc.push_str("\nBuilt-in helpers (always available, no need to define):\n");
            if has_console {
                desc.push_str("  putStrLn :: Text -> M ()\n");
            }
            effects.iter().flat_map(|e| e.helpers).for_each(|h| {
                // Extract just the type signature line
                if let Some(sig) = h.lines().next() {
                    desc.push_str(&format!("  {}\n", sig));
                }
            });
            desc.push_str(
                "\nPrefer helpers over raw `send`: `putStrLn \"hi\"` not `send (Print \"hi\")`.\n",
            );
            desc.push_str("Use `>>=` chains and `<$>`/`<*>` for dense composition. Named bindings as escape hatch.\n");
            desc.push('\n');
            desc.push_str(concat!(
                "Prelude polymorphic ops: `len` for length of Text or [a], ",
                "`isNull` for emptiness of Text or [a], ",
                "`stake`/`sdrop` for take/drop on both Text and [a]. ",
                "`intercalate` joins Text (not lists). ",
                "`joinText` is an alias. `tReverse` reverses Text. ",
                "List-only: `length`, `take`, `drop`, `null` remain unchanged.",
            ));
        }

        let has_llm = effects.iter().any(|e| e.type_name == "Llm");
        let has_ask_desc = effects.iter().any(|e| e.type_name == "Ask");
        if has_llm && has_ask_desc {
            desc.push_str(concat!(
                "\n\nQ-builders + the named runner `askQ` (Library, auto-imported):\n",
                "  Q a — first-class question (schema + parser + confidence gate)\n",
                "  pick cats `askQ` prompt    -- classify; suspends to caller (M Text)\n",
                "  yn `askQ` prompt           -- yes/no (M Bool)\n",
                "  obj schema `askQ` prompt   -- structured extraction (M Value)\n",
                "  txt \"field\" `askQ` prompt  -- single text field (M Text)\n",
                "  num \"field\" `askQ` prompt  -- single number field (M Double)\n",
                "  (,) <$> pick cs <*> num \"n\" `askQ` p  -- Applicative: merged schema, one ask\n",
                "Two runners (same Q-builders, different cost):\n",
                "  q `askQ` prompt   -- SUSPEND to the calling LLM; no autonomous tokens\n",
                "  q `llmQ` prompt   -- AUTONOMOUS server-side model call (costs tokens); e.g. pick cats `llmQ` p\n",
                "Or `llmJson prompt schema` for a raw structured call.\n",
            ));
        }

        let has_fs = effects.iter().any(|e| e.type_name == "Fs");
        if has_fs {
            desc.push_str(concat!(
                "\nExamples (idiomatic — expression-first):\n",
                "  glob \"**/*.rs\" >>= mapM (\\p -> (,) p <$> getFileSize p)\n",
                "  do { src <- readFile \"CLAUDE.md\"; pure (stake 5 (lines src)) }  -- explicit do when sequencing\n",
            ));
            if has_llm && has_ask_desc {
                desc.push_str(
                    "  glob \"**/*.hs\" >>= filterM (readFile >=> \\s -> yn `askQ` (\"test-only?\\n\" <> stake 2000 s))\n",
                );
            }
        }

        // Failure isolation: be explicit about the half-promise — what the
        // try* verbs catch AND what they deliberately don't.
        let has_try = effects
            .iter()
            .any(|e| matches!(e.type_name, "Http" | "Exec" | "Llm" | "Fs"));
        if has_try {
            desc.push_str(concat!(
                "\nFailure isolation (long-running evals): the try* verbs return ",
                "`M (Either Text a)` so one bad probe doesn't kill an eval. An ",
                "EXTERNAL failure \u{2014} bad URL, 404/network error, LLM API ",
                "error or refusal, exec spawn failure, unreadable file \u{2014} ",
                "becomes `Left err` (carrying the cause) and the eval continues:\n",
                "  tryRun, tryRunIn      :: ... -> M (Either Text (Int, Text, Text))\n",
                "  tryHttpGet, tryHttpPost :: ... -> M (Either Text Value)\n",
                "  tryLlm                :: Text -> M (Either Text Text)\n",
                "  tryLlmJson            :: Text -> Schema -> M (Either Text Value)\n",
                "  tryReadFile           :: Text -> M (Either Text Text)\n",
                "They do NOT catch: Haskell `error`/partial functions (including ",
                "readProcess/callCommand on a nonzero exit), other runtime faults, ",
                "eval cancellation/timeout, or the LLM call-budget limit \u{2014} ",
                "those still abort the eval. A command that RUNS but exits nonzero ",
                "is NOT a failure: tryRun returns `Right (code, out, err)`; inspect ",
                "`code` yourself instead of using readProcess (which calls `error`).\n",
            ));
        }
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

/// Scan a user-library directory for top-level type signatures (plus
/// `data`/`type` heads) and render a per-module vocabulary digest for
/// the eval tool description. This is the affordance that keeps eval
/// code shape-first: the combinators a user would otherwise re-invent
/// are visible at every call site instead of requiring a read of the
/// lib sources. Snapshot at server start; restart to refresh.
pub(crate) fn library_vocab(dir: &std::path::Path) -> String {
    // Diagnostic modules, not vocabulary.
    const SKIP: &[&str] = &["Probe", "SelfTest"];
    const SIG_MAX: usize = 120;
    const TOTAL_MAX: usize = 8000;

    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "hs"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();

    let mut out = String::new();
    'files: for path in files {
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if SKIP.contains(&stem) {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let sigs = extract_sigs(&src);
        if sigs.is_empty() {
            continue;
        }
        out.push_str(&format!("  {stem}:\n"));
        for s in sigs {
            let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
            let s: String = s.chars().take(SIG_MAX).collect();
            out.push_str(&format!("    {s}\n"));
            if out.len() > TOTAL_MAX {
                out.push_str("  …(truncated)\n");
                break 'files;
            }
        }
    }
    out
}
