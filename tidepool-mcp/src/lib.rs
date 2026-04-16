//! MCP (Model Context Protocol) server library for Tidepool.
//!
//! Wraps `tidepool-runtime` in an MCP server exposing `run_haskell`,
//! `compile_haskell`, and `eval` tools. Generic over effect handler stacks
//! via `TidepoolMcpServer<H>`.

use dyn_clone::{clone_trait_object, DynClone};
use parking_lot::Mutex;
use rmcp::{
    model::*, service::RequestContext, ErrorData as McpError, RoleServer, ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use tidepool_bridge::{FromCore, ToCore};
use tidepool_runtime::DispatchEffect;
use tokio::io::{stdin, stdout};
use tokio::time::{timeout, Duration};

const EVAL_TIMEOUT_SECS: u64 = 120;
const MAX_CONCURRENT_EVALS: usize = 4;
const MAX_ORPHANED_EVALS: usize = 10;

// ---------------------------------------------------------------------------
// Effect metadata — lives next to the handler, discovered via trait
// ---------------------------------------------------------------------------

/// Static metadata describing a Haskell effect type.
///
/// Each effect handler that wants to participate in the MCP templating system
/// implements `DescribeEffect` to provide its Haskell-side type declaration.
#[derive(Debug, Clone, Copy)]
pub struct EffectDecl {
    /// Haskell GADT type name, e.g. `"Console"`.
    pub type_name: &'static str,
    /// Human-readable description of what this effect does.
    pub description: &'static str,
    /// Haskell GADT constructor declarations (one per line inside `data T a where`).
    pub constructors: &'static [&'static str],
    /// Extra Haskell type/function definitions emitted before the GADT.
    /// Use for supporting types (e.g. `data Lang = ...`) and helper functions.
    pub type_defs: &'static [&'static str],
    /// Thin curried helper definitions emitted after the `type M` alias.
    /// Each string is one or more lines of Haskell (signature + definition).
    pub helpers: &'static [&'static str],
}

/// Parsed constructor info extracted from an EffectDecl constructor string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedConstructor {
    pub name: String,
    pub arity: u32,
}

/// Parse `"GitLog :: Text -> Int -> Git [Value]"` → `ParsedConstructor { name: "GitLog", arity: 2 }`
///
/// Arity = number of `->` in the type signature (each `->` separates one argument from the rest).
pub fn parse_constructor(decl: &str) -> Result<ParsedConstructor, String> {
    let (name_part, type_part) = decl
        .split_once("::")
        .ok_or_else(|| format!("constructor decl must contain '::': {:?}", decl))?;
    let name = name_part.trim().to_string();
    let arity = type_part.matches("->").count() as u32;
    Ok(ParsedConstructor { name, arity })
}

/// Trait for effect handlers that can describe their Haskell-side type.
pub trait DescribeEffect {
    fn effect_decl() -> EffectDecl;
}

/// Trait for collecting effect declarations from an HList of handlers.
pub trait CollectEffectDecls {
    fn collect_decls() -> Vec<EffectDecl>;
}

impl CollectEffectDecls for frunk::HNil {
    fn collect_decls() -> Vec<EffectDecl> {
        Vec::new()
    }
}

impl<H, T> CollectEffectDecls for frunk::HCons<H, T>
where
    H: DescribeEffect,
    T: CollectEffectDecls,
{
    fn collect_decls() -> Vec<EffectDecl> {
        let mut decls = vec![H::effect_decl()];
        decls.extend(T::collect_decls());
        decls
    }
}

// ---------------------------------------------------------------------------
// Standard effect declarations
// ---------------------------------------------------------------------------

/// Console effect: print text output.
pub fn console_decl() -> EffectDecl {
    EffectDecl {
        type_name: "Console",
        description: "Print text output.",
        constructors: &["Print :: Text -> Console ()"],
        type_defs: &[],
        helpers: &[],
    }
}

/// Key-value store effect.
pub fn kv_decl() -> EffectDecl {
    EffectDecl {
        type_name: "KV",
        description:
            "Persistent key-value store. State survives across calls within one server session.",
        constructors: &[
            "KvGet :: Text -> KV (Maybe Value)",
            "KvSet :: Text -> Value -> KV ()",
            "KvDelete :: Text -> KV ()",
            "KvKeys :: KV [Text]",
        ],
        type_defs: &[],
        helpers: &[
            "kvGet :: Text -> M (Maybe Value)\nkvGet = send . KvGet",
            "kvSet :: Text -> Value -> M ()\nkvSet k v = send (KvSet k v)",
            "kvDel :: Text -> M ()\nkvDel = send . KvDelete",
            "kvKeys :: M [Text]\nkvKeys = send KvKeys",
        ],
    }
}

/// File I/O effect (sandboxed).
pub fn fs_decl() -> EffectDecl {
    EffectDecl {
        type_name: "Fs",
        description: "Read and write files (sandboxed to server working directory).",
        constructors: &[
            "FsRead :: Text -> Fs Text",
            "FsWrite :: Text -> Text -> Fs ()",
            "FsListDir :: Text -> Fs [Text]",
            "FsGlob :: Text -> Fs [Text]",
            "FsExists :: Text -> Fs Bool",
            "FsMetadata :: Text -> Fs (Int, Bool, Bool)",
        ],
        type_defs: &[],
        helpers: &[
            "readFile :: Text -> M Text\nreadFile = send . FsRead",
            "writeFile :: Text -> Text -> M ()\nwriteFile f c = send (FsWrite f c)",
            "appendFile :: Text -> Text -> M ()\nappendFile p t = readFile p >>= \\old -> writeFile p (old <> t)",
            "listDirectory :: Text -> M [Text]\nlistDirectory = send . FsListDir",
            "doesFileExist :: Text -> M Bool\ndoesFileExist = send . FsExists",
            "doesDirectoryExist :: Text -> M Bool\ndoesDirectoryExist p = do { (_, _, d) <- send (FsMetadata p); pure d }",
            "getFileSize :: Text -> M Int\ngetFileSize p = do { (s, _, _) <- send (FsMetadata p); pure s }",
            "getCurrentDirectory :: M Text\ngetCurrentDirectory = do { (_, d, _) <- run \"pwd\"; pure (T.strip d) }",
            "glob :: Text -> M [Text]\nglob = send . FsGlob",
        ],
    }
}

/// Structural grep (ast-grep) effect.
pub fn sg_decl() -> EffectDecl {
    EffectDecl {
        type_name: "SG",
        description: concat!(
            "Structural code search via ast-grep. ",
            "Use patterns with $VAR for single-node captures and $$$VAR for multi-node. ",
            "Paths are relative to server working directory.",
        ),
        type_defs: &[
            "data Lang = Rust | Python | TypeScript | JavaScript | Go | Java | C | Cpp | Haskell | Nix | Html | Css | Json | Yaml | Toml",
            "data Match = Match { mText :: Text, mFile :: Text, mLine :: Int, mVars :: [(Text, Text)], mReplacement :: Text }",
            "instance ToJSON Match where\n  toJSON (Match t f l vs r) = object ([\"text\" .= t, \"file\" .= f, \"line\" .= l] ++ (if null vs then [] else [\"vars\" .= toJSON (Map.fromList vs)]) ++ (if T.null r then [] else [\"replacement\" .= r]))",
            "var :: Match -> Text -> Text",
            "var (Match _ _ _ vs _) k = case [v | (k', v) <- vs, k' == k] of { (x:_) -> x; _ -> \"\" }",
        ],
        constructors: &[
            "SgFind    :: Lang -> Text -> [Text] -> SG [Match]",
            "SgRuleFind    :: Lang -> Value -> [Text] -> SG [Match]",
        ],
        helpers: &[
            "sgFind :: Lang -> Text -> [Text] -> M [Match]\nsgFind l p fs = send (SgFind l p fs)",
            "sgRuleFind :: Lang -> Value -> [Text] -> M [Match]\nsgRuleFind l r fs = send (SgRuleFind l r fs)",
            "rPat :: Text -> Value\nrPat p = object [\"pattern\" .= p]",
            "rKind :: Text -> Value\nrKind k = object [\"kind\" .= k]",
            "rRegex :: Text -> Value\nrRegex r = object [\"regex\" .= r]",
            "rHas :: Value -> Value\nrHas r = object [\"has\" .= r]",
            "rInside :: Value -> Value\nrInside r = object [\"inside\" .= r]",
            "rFollows :: Value -> Value\nrFollows r = object [\"follows\" .= r]",
            "rPrecedes :: Value -> Value\nrPrecedes r = object [\"precedes\" .= r]",
            "rAll :: [Value] -> Value\nrAll rs = object [\"all\" .= rs]",
            "rAny :: [Value] -> Value\nrAny rs = object [\"any\" .= rs]",
            "rNot :: Value -> Value\nrNot r = object [\"not\" .= r]",
            // Object merge (primary combinator) — left-biased key union
            "infixr 6 .+.\n(.+.) :: Value -> Value -> Value\n(.+.) (Object a) (Object b) = Object (KM.unionWith const a b)\n(.+.) a _ = a",
            // Conjunction / Disjunction
            "infixr 5 .&.\n(.&.) :: Value -> Value -> Value\na .&. b = object [\"all\" .= [a, b]]",
            "infixr 4 .|.\n(.|.) :: Value -> Value -> Value\na .|. b = object [\"any\" .= [a, b]]",
            // Relational operators
            "infixl 7 ?>\n(?>) :: Value -> Value -> Value\nparent ?> child = parent .+. rHas child",
            "infixl 7 <?\n(<?) :: Value -> Value -> Value\nchild <? ancestor = child .+. rInside ancestor",
            // Extra field helpers
            "rField :: Text -> Value\nrField name = object [\"field\" .= name]",
        ],
    }
}

/// Http effect: fetch JSON from HTTP endpoints.
pub fn http_decl() -> EffectDecl {
    EffectDecl {
        type_name: "Http",
        description: "Fetch JSON from HTTP endpoints. Returns response body as Value.",
        constructors: &[
            "HttpGet :: Text -> Http Value",
            "HttpPost :: Text -> Value -> Http Value",
        ],
        type_defs: &[],
        helpers: &[
            "httpGet :: Text -> M Value\nhttpGet = send . HttpGet",
            "httpPost :: Text -> Value -> M Value\nhttpPost url body = send (HttpPost url body)",
        ],
    }
}

/// Exec effect: run shell commands.
pub fn exec_decl() -> EffectDecl {
    EffectDecl {
        type_name: "Exec",
        description: "Run shell commands and capture output.",
        constructors: &[
            "Run :: Text -> Exec (Int, Text, Text)",
            "RunIn :: Text -> Text -> Exec (Int, Text, Text)",
        ],
        type_defs: &[],
        helpers: &[
            "callCommand :: Text -> M ()\ncallCommand cmd = do { (ec, _, err) <- send (Run cmd); when (ec /= 0) (error (\"command failed (\" <> show ec <> \"): \" <> err)) }",
            "readProcess :: Text -> M Text\nreadProcess cmd = do { (ec, out, err) <- send (Run cmd); if ec == 0 then pure out else error (\"command failed (\" <> show ec <> \"): \" <> err) }",
            "run :: Text -> M (Int, Text, Text)\nrun = send . Run",
            "runIn :: Text -> Text -> M (Int, Text, Text)\nrunIn dir cmd = send (RunIn dir cmd)",
        ],
    }
}

/// Meta effect: self-mirror for querying runtime metadata.
pub fn meta_decl() -> EffectDecl {
    EffectDecl {
        type_name: "Meta",
        description:
            "Self-mirror for the runtime. Query constructors, primops, effects, diagnostics.",
        constructors: &[
            "MetaConstructors :: Meta [(Text, Int)]",
            "MetaLookupCon    :: Text -> Meta (Maybe (Int, Int))",
            "MetaPrimOps      :: Meta [Text]",
            "MetaEffects      :: Meta [Text]",
            "MetaDiagnostics  :: Meta [Text]",
            "MetaVersion      :: Meta Text",
            "MetaHelp         :: Meta [Text]",
        ],
        type_defs: &[],
        helpers: &[
            "metaConstructors :: M [(Text, Int)]\nmetaConstructors = send MetaConstructors",
            "metaLookupCon :: Text -> M (Maybe (Int, Int))\nmetaLookupCon = send . MetaLookupCon",
            "metaPrimOps :: M [Text]\nmetaPrimOps = send MetaPrimOps",
            "metaEffects :: M [Text]\nmetaEffects = send MetaEffects",
            "metaDiagnostics :: M [Text]\nmetaDiagnostics = send MetaDiagnostics",
            "metaVersion :: M Text\nmetaVersion = send MetaVersion",
            "metaHelp :: M [Text]\nmetaHelp = send MetaHelp",
        ],
    }
}

/// Ask effect: suspend execution to ask the calling LLM a question.
pub fn ask_decl() -> EffectDecl {
    EffectDecl {
        type_name: "Ask",
        description: "Suspend execution and ask the calling LLM a question. The LLM calls the resume tool with an answer, and execution continues.",
        constructors: &["Ask :: Text -> Ask Value"],
        type_defs: &[],
        helpers: &[
            "ask :: Text -> M Value\nask = send . Ask",
            "getLine :: Text -> M Text\ngetLine prompt = do { v <- ask prompt; case v of { String s -> pure s; _ -> pure (show v) } }",
        ],
    }
}

/// LLM effect: call an LLM for classification, extraction, or judgment.
pub fn llm_decl() -> EffectDecl {
    EffectDecl {
        type_name: "Llm",
        description: "Call an LLM for classification, extraction, or judgment.",
        constructors: &[
            "LlmChat       :: Text -> Llm Text",
            "LlmStructured :: Text -> Value -> Llm Value",
        ],
        type_defs: &[
            "data Schema = SObj [(Text, Schema)] | SArr Schema | SStr | SNum | SBool | SEnum [Text] | SOpt Schema",
        ],
        helpers: &[
            "llm :: Text -> M Text\nllm = send . LlmChat",
            "llmJson :: Text -> Schema -> M Value\nllmJson prompt schema = send (LlmStructured prompt (schemaToValue schema))",
            "isOpt :: Schema -> Bool\nisOpt (SOpt _) = True\nisOpt _ = False",
            "innerSchema :: Schema -> Schema\ninnerSchema (SOpt s) = s\ninnerSchema s = s",
            "schemaToValue :: Schema -> Value\nschemaToValue SStr = object [\"type\" .= (\"string\" :: Text)]\nschemaToValue SNum = object [\"type\" .= (\"number\" :: Text)]\nschemaToValue SBool = object [\"type\" .= (\"boolean\" :: Text)]\nschemaToValue (SEnum vs) = object [\"type\" .= (\"string\" :: Text), \"enum\" .= vs]\nschemaToValue (SArr item) = object [\"type\" .= (\"array\" :: Text), \"items\" .= schemaToValue item]\nschemaToValue (SOpt s) = schemaToValue s\nschemaToValue (SObj fields) = object [\"type\" .= (\"object\" :: Text), \"properties\" .= object (map (\\(k,s) -> k .= schemaToValue (innerSchema s)) fields), \"required\" .= map fst (filter (not . isOpt . snd) fields)]",
        ],
    }
}

/// All standard effects in canonical order.
pub fn standard_decls() -> Vec<EffectDecl> {
    vec![
        console_decl(),
        kv_decl(),
        fs_decl(),
        sg_decl(),
        http_decl(),
        exec_decl(),
        llm_decl(),
        ask_decl(),
    ]
}

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

/// Request parameters for the `eval` tool.
///
/// Provide a Haskell do-block as a single string. The server wraps it in a
/// full module with the effect stack type, LANGUAGE pragmas, and imports.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EvalRequest {
    /// Haskell do-notation code. Each line is indented into a do-block.
    /// Use `pure x` as the last line to return a value.
    /// Use `send (Constructor args)` to invoke effects.
    pub code: String,
    /// Additional Haskell imports, one per line (e.g. "Data.List (sort)").
    #[serde(default)]
    pub imports: String,
    /// Top-level helper definitions placed before the main do-block.
    /// Function definitions only — custom `data` declarations are not supported.
    #[serde(default)]
    pub helpers: String,
    /// Optional JSON input injected as `input :: Aeson.Value` binding.
    #[serde(default)]
    pub input: Option<serde_json::Value>,
    /// Optional maximum character budget for paginated output.
    /// Controls both `say` output and return value truncation.
    /// Default: 4096.
    #[serde(default)]
    pub max_len: Option<u32>,
}

/// Request parameters for the `resume` tool.
///
/// Used to continue a suspended evaluation that hit an `Ask` effect.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResumeRequest {
    /// The continuation ID returned by a suspended eval call.
    pub continuation_id: String,
    /// The response text to feed back to the suspended Haskell program.
    pub response: String,
}

// ---------------------------------------------------------------------------
// Templating
// ---------------------------------------------------------------------------

/// Generate the Haskell module preamble that wraps user code in `eval` calls.
///
/// Emits: language pragmas, `module Expr`, standard imports (`Tidepool.Prelude`,
/// `Control.Monad.Freer`, qualified `Data.Text`/`Data.Map`/etc.), the user `Library`
/// import (if present), GADT declarations for each registered effect, the `type M`
/// alias over the full effect list, and thin helper functions (e.g. `say`, `kvGet`).
///
/// When `user_library` is true and both `Llm` and `Ask` effects are present, also
/// emits the heuristic combinator definitions (`Q`, `??`, `pick`, `yn`, etc.).
pub fn build_preamble(effects: &[EffectDecl], user_library: bool) -> String {
    let mut out = String::new();
    out.push_str("{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables #-}\n");
    out.push_str("module Expr where\n");
    out.push_str("import Tidepool.Prelude hiding (error)\n");
    out.push_str("import qualified Data.Text as T\n");
    out.push_str("import qualified Data.Map.Strict as Map\n");
    out.push_str("import qualified Data.Set as Set\n");
    out.push_str("import qualified Tidepool.Aeson.KeyMap as KM\n");
    out.push_str("import qualified Data.List as L\n");
    out.push_str("import qualified Tidepool.Text as TT\n");
    out.push_str("import qualified Tidepool.Table as Tab\n");
    out.push_str("import Control.Monad.Freer hiding (run)\n");
    if user_library {
        out.push_str("import Library\n");
    }
    out.push_str("import qualified Prelude as P\n");
    out.push_str("default (Int, Text)\n");
    out.push_str("error :: Text -> a\nerror = P.error . T.unpack\n");
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

    // Emit thin effect helpers
    let has_helpers = effects.iter().any(|e| !e.helpers.is_empty());
    if has_helpers {
        for eff in effects {
            for h in eff.helpers {
                out.push_str(h);
                out.push('\n');
            }
        }
        out.push('\n');
    }

    // Pagination support — auto-truncation of large eval results
    if !effects.is_empty() {
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
                "          resp <- ask (\"[Pagination] truncated: \" <> show truncated <> \" stubs: \" <> show stubInfo)\n",
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

    // Effect orchestration helpers (require M, Value, Text, ask, kvGet, say, etc.)
    if user_library && !effects.is_empty() {
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

        // --- Heuristic combinators: Q a (Haiku-first, Ask-on-uncertainty) ---

        let has_llm = effects.iter().any(|e| e.type_name == "Llm");
        let has_ask_eff = effects.iter().any(|e| e.type_name == "Ask");
        if has_llm && has_ask_eff {
            out.push_str("-- Heuristic combinators\n");
            out.push_str(concat!(
                "data Q a = Q Schema (Value -> a) Double\n",
                "data Judged a = Sure a | Unsure Double a\n",
            ));
            out.push_str(concat!(
                "instance Functor Q where\n",
                "  fmap f (Q s p t) = Q s (f . p) t\n",
            ));
            out.push_str(concat!(
                "instance Applicative Q where\n",
                "  pure a = Q (SObj []) (const a) 0.6\n",
                "  Q (SObj fs1) p1 t1 <*> Q (SObj fs2) p2 t2 = Q (SObj (fs1 ++ fs2)) (\\v -> p1 v (p2 v)) (if t1 >= t2 then t1 else t2)\n",
                "  Q s1 p1 t1 <*> Q s2 p2 t2 = Q s1 (\\v -> p1 v (p2 v)) (if t1 >= t2 then t1 else t2)\n",
            ));
            // Internal helpers: augment schema with rubric, extract confidence, strip rubric
            out.push_str(concat!(
                "h_aug :: Schema -> Schema\n",
                "h_aug (SObj fs) = SObj (fs ++ [(\"_understood\", SBool), (\"_confident\", SBool), (\"_unambiguous\", SBool)])\n",
                "h_aug s = SObj [(\"value\", s), (\"_understood\", SBool), (\"_confident\", SBool), (\"_unambiguous\", SBool)]\n",
            ));
            out.push_str(concat!(
                "h_conf :: Value -> Double\n",
                "h_conf v =\n",
                "  let b k = case v ^? key k . _Bool of { Just True -> 1.0; _ -> 0.0 }\n",
                "  in (b \"_understood\" + b \"_confident\" + b \"_unambiguous\") / 3.0\n",
            ));
            out.push_str(concat!(
                "h_strip :: Value -> Value\n",
                "h_strip (Object kvs) = Object (KM.delete (KM.fromText \"_unambiguous\") (KM.delete (KM.fromText \"_confident\") (KM.delete (KM.fromText \"_understood\") kvs)))\n",
                "h_strip v = v\n",
            ));
            // ?? operator: ask Haiku, auto-escalate on low confidence
            out.push_str(concat!(
                "infixl 1 ??\n",
                "(??) :: Q a -> Text -> M a\n",
                "(Q schema parse threshold) ?? prompt = do\n",
                "  r <- llmJson prompt (h_aug schema)\n",
                "  let c = h_conf r\n",
                "  v <- if c >= threshold then pure (h_strip r)\n",
                "       else ask (prompt <> \"\\n[haiku \" <> pack (showDouble c) <> \"]: \" <> show (h_strip r))\n",
                "  pure (parse v)\n",
            ));
            // ?! operator: ask with evidence, returns Judged
            out.push_str(concat!(
                "infixl 1 ?!\n",
                "(?!) :: Q a -> Text -> M (Judged a)\n",
                "(Q schema parse threshold) ?! prompt = do\n",
                "  r <- llmJson prompt (h_aug schema)\n",
                "  let c = h_conf r\n",
                "  if c >= threshold\n",
                "    then pure (Sure (parse (h_strip r)))\n",
                "    else do\n",
                "      v <- ask (prompt <> \"\\n[haiku \" <> pack (showDouble c) <> \"]: \" <> show (h_strip r))\n",
                "      pure (Unsure c (parse v))\n",
            ));
            // Smart constructors
            out.push_str(concat!(
                "pick :: [Text] -> Q Text\n",
                "pick cats = Q (SObj [(\"pick\", SEnum cats)]) (\\v -> case v ^? key \"pick\" . _String of { Just s -> s; _ -> error \"Q: missing 'pick' in response\" }) 0.6\n",
            ));
            out.push_str(concat!(
                "yn :: Q Bool\n",
                "yn = Q (SObj [(\"answer\", SBool)]) (\\v -> case v ^? key \"answer\" . _Bool of { Just b -> b; _ -> error \"Q: missing 'answer' in response\" }) 0.6\n",
            ));
            out.push_str(concat!(
                "obj :: Schema -> Q Value\n",
                "obj s = Q s id 0.6\n",
            ));
            out.push_str(concat!(
                "txt :: Text -> Q Text\n",
                "txt k = Q (SObj [(k, SStr)]) (\\v -> case v ^? key k . _String of { Just s -> s; _ -> error (\"Q: missing '\" <> k <> \"' in response\") }) 0.6\n",
            ));
            out.push_str(concat!(
                "num :: Text -> Q Double\n",
                "num k = Q (SObj [(k, SNum)]) (\\v -> case v ^? key k . _Number of { Just n -> n; _ -> error (\"Q: missing '\" <> k <> \"' in response\") }) 0.6\n",
            ));
            out.push_str(concat!(
                "bar :: Double -> Q a -> Q a\n",
                "bar t (Q s p _) = Q s p t\n",
            ));
            // Batch helpers
            out.push_str(concat!(
                "triage :: Q b -> (a -> Text) -> [a] -> M [(a, b)]\n",
                "triage q render = mapM (\\x -> (,) x <$> (q ?? render x))\n",
            ));
            out.push_str(concat!(
                "findTally :: Eq a => a -> [(a, Int)] -> Maybe [(a, Int)]\n",
                "findTally _ [] = Nothing\n",
                "findTally x ((k, n):rest) = if x == k then Just ((k, n + 1) : rest) else case findTally x rest of { Just rest' -> Just ((k, n) : rest'); Nothing -> Nothing }\n",
            ));
            out.push_str(concat!(
                "tallyList :: Eq a => [a] -> [(a, Int)]\n",
                "tallyList = foldl' (\\acc x -> case findTally x acc of { Just acc' -> acc'; Nothing -> acc ++ [(x, 1)] }) []\n",
            ));
            out.push_str(concat!(
                "survey :: Eq b => Q b -> (a -> Text) -> [a] -> M [(b, Int)]\n",
                "survey q render xs = do\n",
                "  bs <- mapM (\\x -> q ?? render x) xs\n",
                "  pure (tallyList bs)\n",
            ));
            out.push_str(concat!(
                "sift :: Q Bool -> (a -> Text) -> [a] -> M ([a], [a])\n",
                "sift q render xs = do\n",
                "  tagged <- mapM (\\x -> (,) x <$> (q ?? render x)) xs\n",
                "  pure (map fst (filter snd tagged), map fst (filter (not . snd) tagged))\n",
            ));
        }

        out.push('\n');
    }

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

pub fn build_effect_stack_type(effects: &[EffectDecl]) -> String {
    if effects.is_empty() {
        "'[]".to_string()
    } else {
        let names: Vec<&str> = effects.iter().map(|e| e.type_name).collect();
        format!("'[{}]", names.join(", "))
    }
}

fn build_eval_tool_description(effects: &[EffectDecl]) -> String {
    let mut desc = String::from(concat!(
        "Write Haskell do-notation in `code`. The server wraps it in a module ",
        "with the effect stack, pragmas, and imports. ",
        "Use `pure x` as the last line to return a value. ",
        "Use `send (Constructor args)` to invoke effects. ",
        "First call is slow (~2s). Subsequent calls are cached.\n",
        "Return values are automatically rendered to JSON by the Rust runtime \u{2014} ",
        "Int becomes a number, [Char] becomes a string, Bool becomes true/false, ",
        "lists become arrays, etc. Prefer `pure x` over `send (Print (show x))` ",
        "for returning results.",
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
                "User library: `Library` is auto-imported from `.tidepool/lib/Library.hs`. ",
                "Other modules in `.tidepool/lib/` can be imported explicitly via the `imports` field.\n\n",
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
                "\n\nHeuristic combinators (Library, auto-imported):\n",
                "  Q a — first-class question (schema + parser + confidence gate)\n",
                "  pick cats ?? prompt      -- classify (M Text)\n",
                "  yn ?? prompt             -- yes/no (M Bool)\n",
                "  obj schema ?? prompt     -- structured extraction (M Value)\n",
                "  txt \"field\" ?? prompt    -- single text field (M Text)\n",
                "  num \"field\" ?? prompt    -- single number field (M Double)\n",
                "  (,) <$> pick cs <*> num \"n\" ?? p  -- Applicative: merged schema, one call\n",
                "  bar 0.95 q ?? prompt     -- raise threshold\n",
                "  q ?! prompt             -- returns Sure a | Unsure Double a\n",
                "  triage q render items    -- batch: [(item, answer)]\n",
                "  survey q render items    -- tally: [(answer, count)]\n",
                "  sift yn render items     -- partition: ([true], [false])\n",
            ));
        }
    }

    desc
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
    let mut out = String::new();

    // Preamble contains: pragmas, module header, standard imports, default decl,
    // data declarations, type alias. User imports must go after standard imports
    // (after "import Control.Monad.Freer\n") and before "default".
    if !imports.is_empty() {
        let insert_point = preamble.find("default (Int").unwrap_or(preamble.len());
        out.push_str(&preamble[..insert_point]);
        for imp in imports.lines().map(|l| l.trim()).filter(|l| !l.is_empty()) {
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
    if let Some(val) = input {
        out.push_str("input :: Aeson.Value\n");
        out.push_str(&format!("input = {}\n\n", json_to_haskell(val)));
    }

    out.push_str(&format!("result :: Eff {} Value\n", effect_stack));
    out.push_str("result = do\n");
    if budget.is_some() {
        out.push_str("  kvSet \"__sayChars\" (toJSON (0 :: Int))\n");
    }
    out.push_str("  _r <- do\n");
    for line in code.lines() {
        out.push_str(&format!("    {}\n", line));
    }
    if let Some(b) = budget {
        out.push_str("  _scV <- kvGet \"__sayChars\"\n");
        out.push_str("  let _sayC = case _scV of { Just b -> case b ^? _Number of { Just n -> round n; _ -> 0 }; Nothing -> 0 }\n");
        out.push_str(&format!(
            "  paginateResult (max' 100 ({} - _sayC)) (toJSON _r)\n",
            b
        ));
    } else {
        out.push_str("  paginateResult 4096 (toJSON _r)\n");
    }

    out
}

/// Render a serde_json::Value as a Haskell aeson literal expression.
fn json_to_haskell(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::Null => "Aeson.Null".into(),
        serde_json::Value::Bool(b) => {
            format!("Aeson.Bool {}", if *b { "True" } else { "False" })
        }
        serde_json::Value::Number(n) => {
            format!("Aeson.Number (fromIntegral ({} :: Int))", n)
        }
        serde_json::Value::String(s) => {
            let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
            format!("Aeson.String \"{}\"", escaped)
        }
        serde_json::Value::Array(arr) => {
            let elems: Vec<String> = arr.iter().map(json_to_haskell).collect();
            format!("toJSON [{}]", elems.join(", "))
        }
        serde_json::Value::Object(map) => {
            let pairs: Vec<String> = map
                .iter()
                .map(|(k, v)| {
                    let escaped_k = k.replace('\\', "\\\\").replace('"', "\\\"");
                    format!("\"{}\" .= {}", escaped_k, json_to_haskell(v))
                })
                .collect();
            format!("object [{}]", pairs.join(", "))
        }
    }
}

// ---------------------------------------------------------------------------
// Error formatting
// ---------------------------------------------------------------------------

fn format_panic_payload(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else {
        "unknown panic".to_string()
    }
}

fn format_error_with_source(title: &str, error: &str, source: &str) -> String {
    // Extract user-written code: everything after the "-- [user]" marker.
    let user_section = source
        .find("-- [user]\n")
        .map(|pos| &source[pos + "-- [user]\n".len()..])
        .unwrap_or(source);
    format!(
        "## {}\n{}\n\n## User Code\n```haskell\n{}\n```",
        title, error, user_section
    )
}

// ---------------------------------------------------------------------------
// Import blocklist
// ---------------------------------------------------------------------------

/// Blocked module prefixes. Returns the module name if the import should be rejected.
fn rejected_import(import_str: &str) -> Option<&str> {
    const BLOCKED: &[&str] = &[
        "System.IO.Unsafe",
        "System.IO",
        "System.Process",
        "System.Posix",
        "System.Directory",
        "System.Environment",
        "GHC.IO",
        "GHC.Conc",
        "Foreign",
        "Network",
        "Control.Concurrent",
    ];
    // Extract module name: skip 'qualified' if present, then take the first token
    let mut parts = import_str.split_whitespace();
    let mut module = parts.next().unwrap_or("");
    if module == "qualified" {
        module = parts.next().unwrap_or("");
    }
    // Remove anything from '(' onwards (for imports like "Data.Map (Map)")
    let module = module.split('(').next().unwrap_or("").trim();

    for prefix in BLOCKED {
        if module.starts_with(prefix) {
            return Some(module);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Output capture
// ---------------------------------------------------------------------------

/// Captured output from effect handlers (e.g., Console Print).
///
/// Clone is cheap (Arc-backed). Thread-safe for use across spawn_blocking.
#[derive(Clone, Default)]
pub struct CapturedOutput {
    lines: Arc<std::sync::Mutex<Vec<String>>>,
}

impl CapturedOutput {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a line of output.
    pub fn push(&self, line: String) {
        self.lines
            .lock()
            .unwrap_or_else(|e| {
                tracing::warn!("CapturedOutput mutex was poisoned, recovering");
                e.into_inner()
            })
            .push(line);
    }

    /// Drain all captured lines, returning them and clearing the buffer.
    pub fn drain(&self) -> Vec<String> {
        let mut lines = self.lines.lock().unwrap_or_else(|e| {
            tracing::warn!("CapturedOutput mutex was poisoned, recovering");
            e.into_inner()
        });
        std::mem::take(&mut *lines)
    }

    /// Snapshot current captured lines without clearing the buffer.
    pub fn snapshot(&self) -> Vec<String> {
        self.lines
            .lock()
            .unwrap_or_else(|e| {
                tracing::warn!("CapturedOutput mutex was poisoned, recovering");
                e.into_inner()
            })
            .clone()
    }
}

// ---------------------------------------------------------------------------
// Ask effect — channel-based suspension
// ---------------------------------------------------------------------------

/// Messages from the eval thread to the MCP server.
enum SessionMessage {
    /// The program hit an Ask effect and is waiting for a response.
    Suspended { prompt: String },
    /// The program completed successfully.
    Completed { result: String },
    /// The program encountered an error.
    Error { error: String },
}

/// A suspended evaluation session, waiting for a resume call.
struct EvalSession {
    /// Send a response string to unblock the eval thread's Ask handler.
    response_tx: std::sync::mpsc::Sender<String>,
    /// Receive the next message (Completed, Suspended, or Error) from the eval thread.
    session_rx: tokio::sync::mpsc::UnboundedReceiver<SessionMessage>,
    /// The Haskell source code, for error formatting on resume.
    source: Arc<str>,
    /// When this session was created, for eviction ordering.
    created_at: std::time::Instant,
    /// Output capture for this session.
    captured_output: CapturedOutput,
}

/// Wraps an existing effect dispatcher and intercepts the Ask effect tag.
///
/// When the Ask tag is hit, sends a `Suspended` message via the session channel
/// and blocks the current thread until a response arrives.
struct AskDispatcher {
    inner: Box<dyn McpEffectHandler>,
    ask_tag: u64,
    session_tx: tokio::sync::mpsc::UnboundedSender<SessionMessage>,
    response_rx: std::sync::mpsc::Receiver<String>,
}

impl DispatchEffect<CapturedOutput> for AskDispatcher {
    fn dispatch(
        &mut self,
        tag: u64,
        request: &tidepool_eval::value::Value,
        cx: &tidepool_effect::dispatch::EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_eval::value::Value, tidepool_effect::error::EffectError> {
        if tag == self.ask_tag {
            // Extract prompt from Ask constructor: Con(Ask, [prompt_val])
            let prompt = extract_ask_prompt(request, cx.table())
                .map_err(tidepool_effect::error::EffectError::Handler)?;

            // Signal suspension to the MCP server
            let _ = self.session_tx.send(SessionMessage::Suspended { prompt });

            // Block until the MCP server sends a response via the resume tool
            let response = self.response_rx.recv().map_err(|_| {
                tidepool_effect::error::EffectError::Handler(
                    "Ask session closed (timeout or client disconnected)".into(),
                )
            })?;

            // Parse response as JSON → aeson Value; plain text wraps as Aeson.String
            let json_val: serde_json::Value =
                serde_json::from_str(&response).unwrap_or(serde_json::Value::String(response));
            let core_val = json_val
                .to_value(cx.table())
                .map_err(tidepool_effect::error::EffectError::Bridge)?;
            Ok(core_val)
        } else {
            self.inner.dispatch(tag, request, cx)
        }
    }
}

/// Extract the prompt string from an Ask request Value.
///
/// The request is `Con(Ask, [prompt_val])` where `prompt_val` is a Text value.
/// Returns an error if the prompt cannot be extracted (e.g., unevaluated closure
/// due to a crash in the string-building expression).
fn extract_ask_prompt(
    request: &tidepool_eval::value::Value,
    table: &tidepool_repr::DataConTable,
) -> Result<String, String> {
    use tidepool_eval::value::Value;

    let Value::Con(_, fields) = request else {
        return Err(format!(
            "ask received unexpected request shape (expected Con(Ask, [text])): {:?}",
            request
        ));
    };

    let Some(prompt_val) = fields.first() else {
        return Err(format!(
            "ask received unexpected request shape (expected Con(Ask, [text])): {:?}",
            request
        ));
    };

    // Try using FromCore (handles Text, LitString, [Char])
    match String::from_value(prompt_val, table) {
        Ok(s) => Ok(s),
        Err(e) => {
            // Provide diagnostic: the prompt text couldn't be extracted,
            // likely because the string-building expression crashed
            // (e.g., unresolved external, partial evaluation).
            Err(format!(
                "ask prompt could not be evaluated to Text: {e}. \
                 The expression passed to `ask` likely crashed during evaluation \
                 (check for unresolved externals or runtime errors in the prompt string)."
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Server internals
// ---------------------------------------------------------------------------

/// Trait combining effect dispatch with cloning for the MCP server.
pub trait McpEffectHandler:
    DispatchEffect<CapturedOutput> + DynClone + Send + Sync + 'static
{
}
clone_trait_object!(McpEffectHandler);

impl<T> McpEffectHandler for T where
    T: DispatchEffect<CapturedOutput> + Clone + Send + Sync + 'static
{
}

/// Generic MCP server wrapper that compiles and runs Haskell via Tidepool.
#[derive(Clone)]
pub struct TidepoolMcpServer<H> {
    inner: TidepoolMcpServerImpl,
    _phantom: PhantomData<H>,
}

/// Non-generic internal implementation to satisfy trait requirements.
#[derive(Clone)]
pub struct TidepoolMcpServerImpl {
    handler_factory: Arc<dyn McpEffectHandler>,
    include: Vec<PathBuf>,
    haskell_preamble: String,
    effect_stack_type: String,
    eval_tool_description: String,
    // User library support
    has_user_library: bool,
    // Ask effect support
    ask_tag: u64,
    // Effect names for error annotation (indexed by tag)
    effect_names: Vec<String>,
    continuations: Arc<Mutex<HashMap<String, EvalSession>>>,
    next_cont_id: Arc<AtomicU64>,
    eval_semaphore: Arc<tokio::sync::Semaphore>,
    orphaned_threads: Arc<AtomicUsize>,
}

impl TidepoolMcpServerImpl {
    fn next_continuation_id(&self) -> String {
        let id = self.next_cont_id.fetch_add(1, Ordering::Relaxed);
        format!("cont_{}", id)
    }

    /// Evict the oldest continuation, freeing its semaphore permit.
    /// Dropping `EvalSession` drops `response_tx` → blocked eval thread's
    /// `response_rx.recv()` returns Err → thread exits → permit freed.
    fn evict_oldest_continuation(&self) {
        let mut conts = self.continuations.lock();
        if let Some(oldest_key) = conts
            .iter()
            .min_by_key(|(_, s)| s.created_at)
            .map(|(k, _)| k.clone())
        {
            tracing::info!(cont_id = %oldest_key, "evicting oldest continuation under pressure");
            conts.remove(&oldest_key);
        }
    }

    async fn handle_session_result(
        &self,
        op: &str,
        mut session_rx: tokio::sync::mpsc::UnboundedReceiver<SessionMessage>,
        source: Arc<str>,
        response_tx: std::sync::mpsc::Sender<String>,
        captured_output: CapturedOutput,
        mut handle: Option<JoinHandle<()>>,
    ) -> Result<CallToolResult, McpError> {
        let eval_timeout = Duration::from_secs(EVAL_TIMEOUT_SECS);
        match timeout(eval_timeout, session_rx.recv()).await {
            Ok(Some(message)) => {
                let output = match &message {
                    SessionMessage::Completed { .. } | SessionMessage::Error { .. } => {
                        captured_output.drain()
                    }
                    SessionMessage::Suspended { .. } => captured_output.snapshot(),
                };

                match message {
                    SessionMessage::Completed { result } => {
                        tracing::info!("{} completed", op);
                        let mut response = String::new();
                        if !output.is_empty() {
                            response.push_str("## Output\n");
                            for line in &output {
                                response.push_str(line);
                                response.push('\n');
                            }
                            response.push_str("\n## Result\n");
                        }
                        response.push_str(&result);
                        Ok(CallToolResult::success(vec![Content::text(response)]))
                    }
                    SessionMessage::Suspended { prompt } => {
                        tracing::info!(prompt = %prompt, "{} suspended on Ask", op);
                        let cont_id = self.next_continuation_id();
                        let mut json_obj = serde_json::json!({
                            "suspended": true,
                            "continuation_id": cont_id,
                            "prompt": prompt,
                        });
                        if !output.is_empty() {
                            if let Some(obj) = json_obj.as_object_mut() {
                                obj.insert("output".into(), serde_json::Value::from(output));
                            }
                        }
                        self.continuations.lock().insert(
                            cont_id.clone(),
                            EvalSession {
                                response_tx,
                                session_rx,
                                source: Arc::clone(&source),
                                created_at: std::time::Instant::now(),
                                captured_output,
                            },
                        );
                        Ok(CallToolResult::success(vec![Content::text(
                            json_obj.to_string(),
                        )]))
                    }
                    SessionMessage::Error { error } => {
                        let mut error_msg = format_error_with_source("Error", &error, &source);
                        if !output.is_empty() {
                            error_msg.push_str("\n\n## Output So Far\n");
                            for line in &output {
                                error_msg.push_str(line);
                                error_msg.push('\n');
                            }
                        }
                        tracing::error!("{} failed: {}", op, error);
                        Ok(CallToolResult::error(vec![Content::text(error_msg)]))
                    }
                }
            }
            Ok(None) => {
                tracing::error!("{} thread crashed", op);
                let mut crash_info = String::new();

                // If we have the handle, joining it gives us the panic payload
                if let Some(h) = handle.take() {
                    if let Err(e) = h.join() {
                        crash_info.push_str("\n\n## Thread Panic\n");
                        crash_info.push_str(&format_panic_payload(e));
                    }
                }

                let crash_log = async {
                    use tokio::io::{AsyncReadExt, AsyncSeekExt};
                    let mut file = tokio::fs::File::open(".tidepool/crash.log").await.ok()?;
                    let meta = file.metadata().await.ok()?;
                    let len = meta.len();
                    const MAX_CRASH_LOG_BYTES: u64 = 65536;
                    if len > MAX_CRASH_LOG_BYTES {
                        file.seek(std::io::SeekFrom::End(-(MAX_CRASH_LOG_BYTES as i64)))
                            .await
                            .ok()?;
                    }
                    let mut buf = Vec::new();
                    file.read_to_end(&mut buf).await.ok()?;
                    Some(String::from_utf8_lossy(&buf).into_owned())
                }
                .await;

                if let Some(content) = crash_log {
                    let lines: Vec<&str> = content.lines().rev().take(5).collect();
                    if !lines.is_empty() {
                        crash_info.push_str("\n\n## Recent Crash Log Entries\n```\n");
                        for line in lines.into_iter().rev() {
                            crash_info.push_str(line);
                            crash_info.push('\n');
                        }
                        crash_info.push_str("```\n");
                    }
                }
                let error_msg = format_error_with_source(
                    "Crash",
                    &format!(
                        "{} thread crashed (likely SIGILL from exhausted case branch or SIGSEGV from invalid memory access). Set RUST_LOG=debug for JIT diagnostics on stderr.{}",
                        op, crash_info
                    ),
                    &source,
                );
                Ok(CallToolResult::error(vec![Content::text(error_msg)]))
            }
            Err(_elapsed) => {
                tracing::error!("{} timed out after {}s", op, EVAL_TIMEOUT_SECS);

                // Orphan thread cleanup: move handle to a background task that sleeps a grace period then joins.
                // Use std::thread instead of tokio::task::spawn_blocking to avoid starving the runtime's
                // blocking pool if the eval thread is in a tight infinite loop.
                if let Some(h) = handle.take() {
                    let orphan_count = Arc::clone(&self.orphaned_threads);
                    orphan_count.fetch_add(1, Ordering::Relaxed);
                    std::thread::spawn(move || {
                        // Grace period for the thread to hopefully hit an Ask or return naturally
                        std::thread::sleep(Duration::from_secs(2));
                        let _ = h.join();
                        orphan_count.fetch_sub(1, Ordering::Relaxed);
                    });
                }

                let error_msg = format_error_with_source(
                    "Timeout",
                    &format!(
                        "{} timed out after {}s. This usually means an infinite loop or unbounded recursion.",
                        op, EVAL_TIMEOUT_SECS
                    ),
                    &source,
                );
                Ok(CallToolResult::error(vec![Content::text(error_msg)]))
            }
        }
    }

    async fn eval(&self, req: EvalRequest) -> Result<CallToolResult, McpError> {
        tracing::info!(len = req.code.len(), "eval request");

        if self.orphaned_threads.load(Ordering::Relaxed) >= MAX_ORPHANED_EVALS {
            return Ok(CallToolResult::error(vec![Content::text(
                "Server overloaded: too many timed-out evaluations still running. Please wait.",
            )]));
        }

        // Reject unsafe/IO imports before compilation
        for imp in req
            .imports
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
        {
            if let Some(module) = rejected_import(imp) {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Blocked import: `{}` is not available in the Tidepool sandbox.",
                    module,
                ))]));
            }
        }

        let mut all_imports = aeson_imports();
        all_imports.push_str(&req.imports);
        let source: Arc<str> = template_haskell(
            &self.haskell_preamble,
            &self.effect_stack_type,
            &req.code,
            &all_imports,
            &req.helpers,
            req.input.as_ref(),
            Some(req.max_len.unwrap_or(4096)),
        )
        .into();

        let handlers = dyn_clone::clone_box(&*self.handler_factory);
        let include_refs: Vec<PathBuf> = self.include.clone();
        let source_for_blocking = Arc::clone(&source);
        let captured = CapturedOutput::new();
        let captured_for_blocking = captured.clone();
        let ask_tag = self.ask_tag;
        let effect_names = self.effect_names.clone();

        // Create channels for Ask effect communication
        let (session_tx, session_rx) = tokio::sync::mpsc::unbounded_channel::<SessionMessage>();
        let (response_tx, response_rx) = std::sync::mpsc::channel::<String>();

        let permit = match self.eval_semaphore.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                // All slots busy — evict oldest suspended eval to free a permit
                self.evict_oldest_continuation();
                // Brief yield to let the evicted thread's permit release propagate
                tokio::task::yield_now().await;
                self.eval_semaphore
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| {
                        McpError::internal_error(
                            "Server busy: too many concurrent evaluations. Please try again in a moment.",
                            None,
                        )
                    })?
            }
        };

        // Spawn eval thread — communicates via channels; joined on timeout or completion
        let thread_session_tx = session_tx;
        let handle = std::thread::Builder::new()
            .name("tidepool-eval".into())
            .stack_size(256 * 1024 * 1024)
            .spawn(move || {
                let _permit = permit;
                // Install signal handlers so SIGILL/SIGSEGV from JIT code
                // are caught via sigsetjmp/siglongjmp instead of killing
                // the whole server process.
                tidepool_codegen::signal_safety::install();

                let include_paths: Vec<&Path> = include_refs.iter().map(|p| p.as_path()).collect();
                let mut ask_dispatcher = AskDispatcher {
                    inner: handlers,
                    ask_tag,
                    session_tx: thread_session_tx.clone(),
                    response_rx,
                };

                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    tidepool_runtime::compile_and_run(
                        &source_for_blocking,
                        "result",
                        &include_paths,
                        &mut ask_dispatcher,
                        &captured_for_blocking,
                    )
                }));

                match result {
                    Ok(Ok(eval_result)) => {
                        let _ = thread_session_tx.send(SessionMessage::Completed {
                            result: eval_result.to_string_pretty(),
                        });
                    }
                    Ok(Err(e)) => {
                        let diagnostics = tidepool_runtime::drain_diagnostics();
                        let mut error_detail = e.to_string();
                        // Annotate UnhandledEffect with effect names
                        if let Some(tag_str) = error_detail.strip_prefix("Unhandled effect at tag ")
                        {
                            if let Ok(tag) = tag_str.trim().parse::<usize>() {
                                if tag < effect_names.len() {
                                    let effect_name = &effect_names[tag];
                                    error_detail =
                                        format!("{} (effect: {})", error_detail, effect_name);
                                }
                            }
                            let effects_list: String = effect_names
                                .iter()
                                .enumerate()
                                .map(|(i, name)| format!("  {} = {}", i, name))
                                .collect::<Vec<_>>()
                                .join("\n");
                            error_detail
                                .push_str(&format!("\n\nRegistered effects:\n{}", effects_list));
                        }
                        if !diagnostics.is_empty() {
                            error_detail.push_str("\n\n## JIT Diagnostics\n");
                            for d in &diagnostics {
                                error_detail.push_str(d);
                                error_detail.push('\n');
                            }
                        }
                        let _ = thread_session_tx.send(SessionMessage::Error {
                            error: error_detail,
                        });
                    }
                    Err(panic_payload) => {
                        let diagnostics = tidepool_runtime::drain_diagnostics();
                        let mut error_detail = format_panic_payload(panic_payload);
                        if !diagnostics.is_empty() {
                            error_detail.push_str("\n\n## JIT Diagnostics\n");
                            for d in &diagnostics {
                                error_detail.push_str(d);
                                error_detail.push('\n');
                            }
                        }
                        let _ = thread_session_tx.send(SessionMessage::Error {
                            error: error_detail,
                        });
                    }
                }
            })
            .map_err(|e| McpError::internal_error(format!("thread spawn error: {}", e), None))?;

        // Await first message from the eval thread
        self.handle_session_result(
            "eval",
            session_rx,
            source,
            response_tx,
            captured,
            Some(handle),
        )
        .await
    }

    async fn resume(&self, req: ResumeRequest) -> Result<CallToolResult, McpError> {
        tracing::info!(continuation_id = %req.continuation_id, "resume request");

        let session = {
            let mut conts = self.continuations.lock();
            conts.remove(&req.continuation_id).ok_or_else(|| {
                McpError::invalid_params(
                    format!(
                        "Unknown or expired continuation_id: {}",
                        req.continuation_id
                    ),
                    None,
                )
            })?
        };

        // Send the response to the blocked eval thread
        session
            .response_tx
            .send(req.response)
            .map_err(|_| McpError::internal_error("eval thread is no longer running", None))?;

        let source = session.source.clone();
        let response_tx = session.response_tx.clone();
        let captured = session.captured_output.clone();

        // Await the next message from the eval thread
        self.handle_session_result(
            "resume",
            session.session_rx,
            source,
            response_tx,
            captured,
            None,
        )
        .await
    }
}

impl ServerHandler for TidepoolMcpServerImpl {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(self.eval_tool_description.clone()),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = request.arguments.unwrap_or_default();
        match request.name.as_ref() {
            "eval" => {
                let req: EvalRequest = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| {
                        McpError::invalid_params(format!("invalid params: {}", e), None)
                    })?;
                self.eval(req).await
            }
            "resume" => {
                let req: ResumeRequest = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| {
                        McpError::invalid_params(format!("invalid params: {}", e), None)
                    })?;
                self.resume(req).await
            }
            _ => Err(McpError {
                code: ErrorCode::METHOD_NOT_FOUND,
                message: format!("Tool not found: {}", request.name).into(),
                data: None,
            }),
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        fn schema_to_map(
            schema: schemars::Schema,
        ) -> Result<Arc<serde_json::Map<String, serde_json::Value>>, McpError> {
            let json = serde_json::to_value(&schema).map_err(|e| {
                McpError::internal_error(format!("Failed to serialize schema: {}", e), None)
            })?;
            match json {
                serde_json::Value::Object(o) => Ok(Arc::new(o)),
                _ => Ok(Arc::new(serde_json::Map::new())),
            }
        }

        let tools = vec![
            Tool {
                name: "eval".into(),
                title: None,
                description: Some(self.eval_tool_description.clone().into()),
                input_schema: schema_to_map(schemars::schema_for!(EvalRequest))?,
                output_schema: None,
                annotations: None,
                icons: None,
                meta: None,
                execution: None,
            },
            Tool {
                name: "resume".into(),
                title: None,
                description: Some(
                    "Resume a suspended Haskell evaluation. When eval returns \
                     {\"suspended\": true, \"continuation_id\": \"...\", \"prompt\": \"...\"}, \
                     call this tool with the continuation_id and your response to the prompt."
                        .into(),
                ),
                input_schema: schema_to_map(schemars::schema_for!(ResumeRequest))?,
                output_schema: None,
                annotations: None,
                icons: None,
                meta: None,
                execution: None,
            },
        ];

        Ok(ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        })
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

impl<H> TidepoolMcpServer<H>
where
    H: DispatchEffect<CapturedOutput> + Clone + Send + Sync + 'static + CollectEffectDecls,
{
    /// Create a new server with the given effect handler stack.
    ///
    /// Effect declarations are collected automatically from handlers that
    /// implement `DescribeEffect`.
    pub fn new(handler: H) -> Self {
        let mut decls = H::collect_decls();
        let ask_tag = decls.len() as u64;
        decls.push(ask_decl());
        let effect_names: Vec<String> = decls.iter().map(|d| d.type_name.to_string()).collect();
        Self {
            inner: TidepoolMcpServerImpl {
                handler_factory: Arc::new(handler),
                include: Vec::new(),
                haskell_preamble: build_preamble(&decls, false),
                effect_stack_type: build_effect_stack_type(&decls),
                eval_tool_description: build_eval_tool_description(&decls),
                has_user_library: false,
                ask_tag,
                effect_names,
                continuations: Arc::new(Mutex::new(HashMap::new())),
                next_cont_id: Arc::new(AtomicU64::new(1)),
                eval_semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_EVALS)),
                orphaned_threads: Arc::new(AtomicUsize::new(0)),
            },
            _phantom: PhantomData,
        }
    }

    /// Add include paths for Haskell module resolution.
    pub fn with_include(mut self, paths: Vec<PathBuf>) -> Self {
        self.inner.include = paths;
        self
    }

    /// Add the bundled Tidepool prelude to the include paths.
    ///
    /// Looks for the prelude in this order:
    /// 1. `TIDEPOOL_PRELUDE_DIR` environment variable
    /// 2. The provided fallback path
    ///
    /// The prelude provides source definitions for common Prelude functions
    /// (reverse, splitAt, sort, etc.) whose GHC base library workers lack
    /// unfoldings in .hi files.
    pub fn with_prelude(mut self, fallback: PathBuf) -> Self {
        let prelude_dir = std::env::var_os("TIDEPOOL_PRELUDE_DIR")
            .map(PathBuf::from)
            .unwrap_or(fallback);
        self.inner.include.push(prelude_dir);

        // Probe for user library directory
        let user_lib = PathBuf::from(".tidepool/lib");
        if user_lib.is_dir() {
            self.inner.has_user_library = user_lib.join("Library.hs").exists();
            self.inner.include.push(user_lib);
            if self.inner.has_user_library {
                // Rebuild preamble with user library import
                let mut decls = H::collect_decls();
                decls.push(ask_decl());
                self.inner.haskell_preamble = build_preamble(&decls, true);
                // Append note to tool description
                self.inner.eval_tool_description.push_str(
                    "\n\nUser library: `Library` is auto-imported from `.tidepool/lib/Library.hs`. \
                     Other modules in `.tidepool/lib/` can be imported explicitly via the `imports` field."
                );
            }
        }

        self
    }

    /// Start the MCP server on stdio transport.
    pub async fn serve_stdio(self) -> Result<(), Box<dyn std::error::Error>> {
        self.inner
            .serve((stdin(), stdout()))
            .await?
            .waiting()
            .await?;
        Ok(())
    }

    /// Start the MCP server on streamable HTTP transport.
    pub async fn serve_http(
        self,
        addr: std::net::SocketAddr,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use rmcp::transport::streamable_http_server::{
            session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
        };
        use std::sync::Arc;

        let template = self.inner;
        let config = StreamableHttpServerConfig::default();
        let cancel = config.cancellation_token.clone();
        let service = StreamableHttpService::new(
            move || Ok(template.clone()),
            Arc::new(LocalSessionManager::default()),
            config,
        );
        async fn health() -> axum::Json<serde_json::Value> {
            axum::Json(serde_json::json!({"status": "ok"}))
        }

        let router = axum::Router::new()
            .route("/health", axum::routing::get(health))
            .nest_service("/mcp", service);
        let listener = tokio::net::TcpListener::bind(addr).await?;
        eprintln!(
            "Tidepool MCP v{} listening on http://{}/mcp",
            env!("CARGO_PKG_VERSION"),
            addr,
        );
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                tokio::signal::ctrl_c().await.ok();
                cancel.cancel();
            })
            .await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eval_request_string_code() {
        let json = serde_json::json!({"code": "let x = 1\npure x"});
        let req: EvalRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.code, "let x = 1\npure x");
        assert!(req.imports.is_empty());
        assert!(req.helpers.is_empty());
    }

    #[test]
    fn test_eval_request_string_imports() {
        let json = serde_json::json!({"code": "pure 42", "imports": "Data.List (sort)\nData.Char"});
        let req: EvalRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.imports, "Data.List (sort)\nData.Char");
    }

    #[test]
    fn test_rejected_imports() {
        assert!(rejected_import("System.IO.Unsafe (unsafePerformIO)").is_some());
        assert!(rejected_import("System.Process (callCommand)").is_some());
        assert!(rejected_import("System.Posix.Signals").is_some());
        assert!(rejected_import("GHC.IO.Handle").is_some());
        assert!(rejected_import("Network.Socket").is_some());
        assert!(rejected_import("Control.Concurrent (forkIO)").is_some());
        assert!(rejected_import("Foreign.Ptr").is_some());
        // Safe imports should pass
        assert!(rejected_import("Data.List (sort)").is_none());
        assert!(rejected_import("Data.Map.Strict").is_none());
        assert!(rejected_import("Tidepool.Text").is_none());
        assert!(rejected_import("qualified Data.Text as T").is_none());
    }

    #[test]
    fn test_build_preamble() {
        let effects = vec![
            EffectDecl {
                type_name: "Console",
                description: "Print output",
                constructors: &["Print :: Text -> Console ()"],
                type_defs: &[],
                helpers: &[],
            },
            EffectDecl {
                type_name: "KV",
                description: "Key-value store",
                constructors: &[
                    "KvGet :: Text -> KV (Maybe Text)",
                    "KvSet :: Text -> Text -> KV ()",
                ],
                type_defs: &[],
                helpers: &[],
            },
        ];
        let preamble = build_preamble(&effects, false);
        assert!(preamble.contains("data Console a where"));
        assert!(preamble.contains("  Print :: Text -> Console ()"));
        assert!(preamble.contains("data KV a where"));
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
    fn test_template_haskell() {
        let effects = vec![EffectDecl {
            type_name: "Console",
            description: "",
            constructors: &["Print :: Text -> Console ()"],
            type_defs: &[],
            helpers: &[],
        }];
        let preamble = build_preamble(&effects, false);
        let stack = build_effect_stack_type(&effects);
        let source = "let x = 42\npure x";

        let result = template_haskell(&preamble, &stack, source, "", "", None, None);

        assert!(result.contains("module Expr where"));
        assert!(result.contains("import Control.Monad.Freer hiding (run)"));
        assert!(result.contains("data Console a where"));
        assert!(result.contains("result :: Eff '[Console] Value"));
        assert!(result.contains("result = do"));
        assert!(result.contains("  let x = 42"));
        assert!(result.contains("  pure x"));
    }

    #[test]
    fn test_eval_tool_description_includes_effects() {
        let effects = vec![EffectDecl {
            type_name: "Console",
            description: "Print to console",
            constructors: &["Print :: Text -> Console ()"],
            type_defs: &[],
            helpers: &["putStrLn :: Text -> M ()\nputStrLn = send . Print"],
        }];
        let desc = build_eval_tool_description(&effects);
        assert!(desc.contains("Console: Print to console"));
        // Constructors not shown separately (helpers section covers them)
        assert!(desc.contains("putStrLn :: Text -> M ()"));
        assert!(desc.contains("Built-in helpers"));
    }

    #[test]
    fn test_preamble_includes_helpers() {
        let decls = standard_decls();
        let preamble = build_preamble(&decls, false);
        // Standard Haskell names as primary
        assert!(preamble.contains("putStrLn :: Text -> M ()"));
        assert!(preamble.contains("readFile :: Text -> M Text\nreadFile = send . FsRead"));
        assert!(preamble.contains("writeFile :: Text -> Text -> M ()"));
        assert!(preamble.contains("appendFile :: Text -> Text -> M ()"));
        assert!(preamble.contains("listDirectory :: Text -> M [Text]"));
        assert!(preamble.contains("doesFileExist :: Text -> M Bool"));
        assert!(preamble.contains("getFileSize :: Text -> M Int"));
        assert!(preamble.contains("glob :: Text -> M [Text]"));
        assert!(preamble.contains("callCommand :: Text -> M ()"));
        assert!(preamble.contains("readProcess :: Text -> M Text"));
        assert!(preamble.contains("getLine :: Text -> M Text"));
        // No old aliases
        assert!(!preamble.contains("fsRead"));
        assert!(!preamble.contains("fsWrite"));
        assert!(!preamble.contains("\nsay "));
        // Other helpers unchanged
        assert!(preamble.contains("kvGet :: Text -> M (Maybe Value)\nkvGet = send . KvGet"));
        assert!(preamble.contains("httpGet :: Text -> M Value\nhttpGet = send . HttpGet"));
        assert!(preamble.contains("ask :: Text -> M Value\nask = send . Ask"));
    }

    #[test]
    fn test_format_panic_payload() {
        use std::any::Any;

        let s = "string panic".to_string();
        let payload: Box<dyn Any + Send> = Box::new(s);
        assert_eq!(format_panic_payload(payload), "string panic");

        let s = "str panic";
        let payload: Box<dyn Any + Send> = Box::new(s);
        assert_eq!(format_panic_payload(payload), "str panic");

        let payload: Box<dyn Any + Send> = Box::new(42);
        assert_eq!(format_panic_payload(payload), "unknown panic");
    }

    #[test]
    fn test_format_error_with_source() {
        let title = "Error";
        let error = "Type mismatch";
        let source = "preamble stuff\n-- [user]\nresult = do\n  pure 42\n";
        let formatted = format_error_with_source(title, error, source);

        assert!(formatted.contains("## Error"));
        assert!(formatted.contains("Type mismatch"));
        assert!(formatted.contains("## User Code"));
        assert!(formatted.contains("```haskell\nresult = do\n  pure 42\n\n```"));
        // Preamble should be trimmed
        assert!(!formatted.contains("preamble stuff"));
    }

    #[test]
    fn test_format_error_no_marker_shows_full() {
        let formatted = format_error_with_source("Error", "oops", "full source");
        assert!(formatted.contains("full source"));
    }

    #[test]
    fn test_ask_decl() {
        let decl = ask_decl();
        assert_eq!(decl.type_name, "Ask");
        assert_eq!(decl.constructors.len(), 1);
        assert!(decl.constructors[0].contains("Ask :: Text -> Ask Value"));
    }

    #[test]
    fn test_standard_decls_includes_ask() {
        let decls = standard_decls();
        assert_eq!(decls.len(), 8);
        assert_eq!(decls[4].type_name, "Http");
        assert_eq!(decls[5].type_name, "Exec");
        assert_eq!(decls[6].type_name, "Llm");
        assert_eq!(decls[7].type_name, "Ask");
    }

    #[test]
    fn test_resume_request_parse() {
        let json = serde_json::json!({
            "continuation_id": "cont_1",
            "response": "hello"
        });
        let req: ResumeRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.continuation_id, "cont_1");
        assert_eq!(req.response, "hello");
    }

    #[test]
    fn test_ask_in_preamble() {
        let decls = standard_decls();
        let preamble = build_preamble(&decls, false);
        assert!(preamble.contains("data Ask a where"));
        assert!(preamble.contains("  Ask :: Text -> Ask Value"));
        assert!(preamble.contains("type M = Eff '[Console, KV, Fs, SG, Http, Exec, Llm, Ask]"));
    }

    #[test]
    fn test_ask_in_effect_stack_type() {
        let decls = standard_decls();
        let stack = build_effect_stack_type(&decls);
        assert_eq!(stack, "'[Console, KV, Fs, SG, Http, Exec, Llm, Ask]");
    }

    #[test]
    fn test_preamble_hides_run_from_freer() {
        let decls = standard_decls();
        let preamble = build_preamble(&decls, false);
        assert!(preamble.contains("import Control.Monad.Freer hiding (run)"));
        // Our run helper should still be present
        assert!(preamble.contains("run :: Text -> M (Int, Text, Text)\nrun = send . Run"));
    }

    #[test]
    fn test_preamble_text_error_shadow() {
        let decls = standard_decls();
        let preamble = build_preamble(&decls, false);
        // Prelude error (String-based) is hidden
        assert!(preamble.contains("import Tidepool.Prelude hiding (error)"));
        // Text-taking error is defined via qualified Prelude
        assert!(preamble.contains("import qualified Prelude as P"));
        assert!(preamble.contains("error :: Text -> a\nerror = P.error . T.unpack"));
    }

    #[test]
    fn test_exec_decl() {
        let decl = exec_decl();
        assert_eq!(decl.type_name, "Exec");
        assert!(decl
            .constructors
            .iter()
            .any(|c| c.contains("Run :: Text -> Exec (Int, Text, Text)")));
        assert!(decl
            .constructors
            .iter()
            .any(|c| c.contains("RunIn :: Text -> Text -> Exec (Int, Text, Text)")));
    }

    #[test]
    fn test_preamble_orchestration_helpers() {
        let decls = standard_decls();
        let preamble = build_preamble(&decls, true);
        // runChecked is now an alias for readProcess
        assert!(preamble.contains("runChecked :: Text -> M Text\nrunChecked = readProcess"));
        // File manipulation helpers
        assert!(preamble.contains("mapFile :: Text -> (Text -> Text) -> M ()"));
        assert!(preamble.contains("mapFileM :: Text -> (Text -> M Text) -> M ()"));
        assert!(preamble.contains("searchFiles :: Text -> Text -> M [(Text, Int, Text)]"));
        assert!(preamble.contains("lineCount :: Text -> M Int"));
        assert!(preamble.contains("fileContains :: Text -> Text -> M Bool"));
        // KV batch helpers
        assert!(preamble.contains("kvAll :: M [(Text, Value)]"));
        assert!(preamble.contains("kvClear :: M ()"));
        assert!(preamble.contains("runAll :: [Text] -> M [(Int, Text, Text)]"));
        // Heuristic combinators
        assert!(preamble.contains("data Q a = Q Schema (Value -> a) Double"));
        assert!(preamble.contains("data Judged a = Sure a | Unsure Double a"));
        assert!(preamble.contains("(??) :: Q a -> Text -> M a"));
        assert!(preamble.contains("(?!) :: Q a -> Text -> M (Judged a)"));
        assert!(preamble.contains("pick :: [Text] -> Q Text"));
        assert!(preamble.contains("yn :: Q Bool"));
        assert!(preamble.contains("obj :: Schema -> Q Value"));
        assert!(preamble.contains("txt :: Text -> Q Text"));
        assert!(preamble.contains("num :: Text -> Q Double"));
        assert!(preamble.contains("bar :: Double -> Q a -> Q a"));
        assert!(preamble.contains("triage :: Q b -> (a -> Text) -> [a] -> M [(a, b)]"));
        assert!(preamble.contains("survey :: Eq b => Q b -> (a -> Text) -> [a] -> M [(b, Int)]"));
        assert!(preamble.contains("sift :: Q Bool -> (a -> Text) -> [a] -> M ([a], [a])"));
    }

    #[test]
    fn test_preamble_no_orchestration_without_library() {
        let decls = standard_decls();
        let preamble = build_preamble(&decls, false);
        // Orchestration helpers only appear with user_library=true
        assert!(!preamble.contains("runChecked"));
    }

    #[test]
    fn test_preamble_sg_rule_operators() {
        let decls = standard_decls();
        let preamble = build_preamble(&decls, false);
        // Object merge operator
        assert!(preamble.contains("infixr 6 .+."));
        assert!(preamble.contains("(.+.) :: Value -> Value -> Value"));
        assert!(preamble.contains("KM.unionWith const"));
        // Conjunction / disjunction
        assert!(preamble.contains("infixr 5 .&."));
        assert!(preamble.contains("infixr 4 .|."));
        // Relational operators
        assert!(preamble.contains("infixl 7 ?>"));
        assert!(preamble.contains("infixl 7 <?"));
        // Extra helpers
        assert!(preamble.contains("rField :: Text -> Value"));
    }

    #[test]
    fn test_parse_constructor_no_args() {
        let p = parse_constructor("GitBranches :: Git [Value]").unwrap();
        assert_eq!(
            p,
            ParsedConstructor {
                name: "GitBranches".into(),
                arity: 0
            }
        );
    }

    #[test]
    fn test_parse_constructor_two_args() {
        let p = parse_constructor("GitLog :: Text -> Int -> Git [Value]").unwrap();
        assert_eq!(
            p,
            ParsedConstructor {
                name: "GitLog".into(),
                arity: 2
            }
        );
    }

    #[test]
    fn test_parse_constructor_nested_types() {
        let p = parse_constructor("FakeReq :: Text -> Text -> [(Text,Text)] -> Text -> Fake Value")
            .unwrap();
        assert_eq!(
            p,
            ParsedConstructor {
                name: "FakeReq".into(),
                arity: 4
            }
        );
    }

    #[test]
    fn test_preamble_required_imports() {
        let decls = standard_decls();
        let preamble = build_preamble(&decls, false);
        assert!(preamble.contains("import Tidepool.Prelude hiding (error)"));
        assert!(preamble.contains("import qualified Data.Text as T"));
        assert!(preamble.contains("import Control.Monad.Freer hiding (run)"));
        assert!(preamble.contains("import qualified Tidepool.Aeson.KeyMap as KM"));
    }

    #[test]
    fn test_template_haskell_truncation() {
        let effects = vec![EffectDecl {
            type_name: "Console",
            description: "",
            constructors: &["Print :: Text -> Console ()"],
            type_defs: &[],
            helpers: &[],
        }];
        let preamble = build_preamble(&effects, false);
        let stack = build_effect_stack_type(&effects);
        let source = "pure 42";

        // With budget
        let result = template_haskell(&preamble, &stack, source, "", "", None, Some(1024));
        assert!(result.contains("kvSet \"__sayChars\" (toJSON (0 :: Int))"));
        assert!(result.contains("paginateResult (max' 100 (1024 - _sayC)) (toJSON _r)"));

        // Without budget (defaults to 4096)
        let result = template_haskell(&preamble, &stack, source, "", "", None, None);
        assert!(result.contains("paginateResult 4096 (toJSON _r)"));
    }

    #[test]
    fn test_template_haskell_input() {
        let effects = vec![EffectDecl {
            type_name: "Console",
            description: "",
            constructors: &["Print :: Text -> Console ()"],
            type_defs: &[],
            helpers: &[],
        }];
        let preamble = build_preamble(&effects, false);
        let stack = build_effect_stack_type(&effects);
        let source = "pure 42";
        let input = serde_json::json!({"val": 123});

        let result = template_haskell(&preamble, &stack, source, "", "", Some(&input), None);

        assert!(result.contains("input :: Aeson.Value"));
        assert!(
            result.contains("input = object [\"val\" .= Aeson.Number (fromIntegral (123 :: Int))]")
        );
    }

    #[test]
    fn test_eval_timeout_value() {
        assert_eq!(EVAL_TIMEOUT_SECS, 120);
    }

    #[test]
    fn test_effect_decls_basic_validation() {
        let console = console_decl();
        assert_eq!(console.type_name, "Console");
        assert!(console.constructors[0].contains("Print"));

        let kv = kv_decl();
        assert_eq!(kv.type_name, "KV");
        assert!(kv.constructors.iter().any(|c| c.contains("KvGet")));

        let fs = fs_decl();
        assert_eq!(fs.type_name, "Fs");
        assert!(fs.constructors.iter().any(|c| c.contains("FsRead")));

        let http = http_decl();
        assert_eq!(http.type_name, "Http");
        assert!(http.constructors.iter().any(|c| c.contains("HttpGet")));
    }

    #[test]
    fn test_eval_request_helpers() {
        let json = serde_json::json!({
            "code": "pure 42",
            "helpers": "foo :: Int -> Int\nfoo x = x + 1"
        });
        let req: EvalRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.helpers, "foo :: Int -> Int\nfoo x = x + 1");
    }

    #[test]
    fn test_eval_request_input() {
        let json = serde_json::json!({
            "code": "pure 42",
            "input": {"key": "value", "num": 123}
        });
        let req: EvalRequest = serde_json::from_value(json).unwrap();
        assert!(req.input.is_some());
        let input = req.input.unwrap();
        assert_eq!(input["key"], "value");
        assert_eq!(input["num"], 123);
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
        assert!(haskell.contains("\"num\" .= Aeson.Number (fromIntegral (42 :: Int))"));
        assert!(haskell.contains("\"arr\" .= toJSON [Aeson.Number (fromIntegral (1 :: Int)), Aeson.Number (fromIntegral (2 :: Int))]"));
        assert!(
            haskell.contains("\"obj\" .= object [\"a\" .= Aeson.Number (fromIntegral (1 :: Int))]")
        );
    }

    #[tokio::test]
    async fn test_handle_session_result_completed() {
        let server = create_mock_server();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (resp_tx, _resp_rx) = std::sync::mpsc::channel();
        let source: Arc<str> = "test source".into();
        let captured = CapturedOutput::new();
        captured.push("log1".into());

        tx.send(SessionMessage::Completed {
            result: "42".into(),
        })
        .unwrap();

        let res = server
            .handle_session_result("eval", rx, source, resp_tx, captured, None)
            .await
            .unwrap();
        assert_eq!(res.is_error, Some(false));
        let text = match &res.content[0].raw {
            RawContent::Text(t) => &t.text,
            _ => panic!("Expected text content"),
        };
        assert!(text.contains("## Output\nlog1\n"));
        assert!(text.contains("\n## Result\n42"));
    }

    #[tokio::test]
    async fn test_handle_session_result_suspended() {
        let server = create_mock_server();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (resp_tx, _resp_rx) = std::sync::mpsc::channel();
        let source: Arc<str> = "test source".into();
        let captured = CapturedOutput::new();

        tx.send(SessionMessage::Suspended {
            prompt: "what is your name?".into(),
        })
        .unwrap();

        let res = server
            .handle_session_result("eval", rx, source, resp_tx, captured, None)
            .await
            .unwrap();
        assert_eq!(res.is_error, Some(false));
        let text = match &res.content[0].raw {
            RawContent::Text(t) => &t.text,
            _ => panic!("Expected text content"),
        };
        let json: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(json["suspended"], true);
        assert_eq!(json["prompt"], "what is your name?");
        assert!(json["continuation_id"]
            .as_str()
            .unwrap()
            .starts_with("cont_"));

        // Check if it's in the continuations map
        let cont_id = json["continuation_id"].as_str().unwrap();
        let conts = server.continuations.lock();
        assert!(conts.contains_key(cont_id));
    }

    #[tokio::test]
    async fn test_handle_session_result_error() {
        let server = create_mock_server();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (resp_tx, _resp_rx) = std::sync::mpsc::channel();
        let source: Arc<str> = "test source".into();
        let captured = CapturedOutput::new();

        tx.send(SessionMessage::Error {
            error: "oops".into(),
        })
        .unwrap();

        let res = server
            .handle_session_result("eval", rx, source, resp_tx, captured, None)
            .await
            .unwrap();
        assert_eq!(res.is_error, Some(true));
        let text = match &res.content[0].raw {
            RawContent::Text(t) => &t.text,
            _ => panic!("Expected text content"),
        };
        assert!(text.contains("## Error"));
        assert!(text.contains("oops"));
    }

    #[tokio::test]
    async fn test_handle_session_result_crash() {
        let server = create_mock_server();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (resp_tx, _resp_rx) = std::sync::mpsc::channel();
        let source: Arc<str> = "test source".into();
        let captured = CapturedOutput::new();

        // Close the channel without sending anything
        drop(tx);

        let res = server
            .handle_session_result("eval", rx, source, resp_tx, captured, None)
            .await
            .unwrap();
        assert_eq!(res.is_error, Some(true));
        let text = match &res.content[0].raw {
            RawContent::Text(t) => &t.text,
            _ => panic!("Expected text content"),
        };
        assert!(text.contains("## Crash"));
        assert!(text.contains("eval thread crashed"));
    }

    #[tokio::test]
    async fn test_handle_session_result_timeout() {
        tokio::time::pause();

        let server = create_mock_server();
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (resp_tx, _resp_rx) = std::sync::mpsc::channel();
        let source: Arc<str> = "test source".into();
        let captured = CapturedOutput::new();

        let handle = tokio::spawn(async move {
            server
                .handle_session_result("eval", rx, source, resp_tx, captured, None)
                .await
        });

        // Advance time past EVAL_TIMEOUT_SECS
        tokio::time::advance(Duration::from_secs(EVAL_TIMEOUT_SECS + 1)).await;

        let res = handle.await.unwrap().unwrap();
        assert_eq!(res.is_error, Some(true));
        let text = match &res.content[0].raw {
            RawContent::Text(t) => &t.text,
            _ => panic!("Expected text content"),
        };
        assert!(text.contains("## Timeout"));
        assert!(text.contains("timed out"));
    }

    #[tokio::test]
    async fn test_eval_orphaned_overload() {
        let server = create_mock_server();
        // Manually saturate the orphan count
        server
            .orphaned_threads
            .store(MAX_ORPHANED_EVALS, Ordering::SeqCst);

        let req = EvalRequest {
            code: "pure 42".into(),
            imports: String::new(),
            helpers: String::new(),
            input: None,
            max_len: None,
        };

        let res = server.eval(req).await.unwrap();
        assert_eq!(res.is_error, Some(true));
        let text = match &res.content[0].raw {
            RawContent::Text(t) => &t.text,
            _ => panic!("Expected text content"),
        };
        assert!(text.contains("Server overloaded"));
        assert!(text.contains("too many timed-out evaluations"));
    }

    fn create_mock_server() -> TidepoolMcpServerImpl {
        #[derive(Clone)]
        struct MockHandler;
        impl DispatchEffect<CapturedOutput> for MockHandler {
            fn dispatch(
                &mut self,
                _tag: u64,
                _request: &tidepool_eval::value::Value,
                _cx: &tidepool_effect::EffectContext<'_, CapturedOutput>,
            ) -> Result<tidepool_eval::value::Value, tidepool_effect::error::EffectError>
            {
                Ok(tidepool_eval::value::Value::Lit(
                    tidepool_repr::Literal::LitInt(0),
                ))
            }
        }

        TidepoolMcpServerImpl {
            handler_factory: Arc::new(MockHandler),
            include: Vec::new(),
            haskell_preamble: String::new(),
            effect_stack_type: String::new(),
            eval_tool_description: String::new(),
            has_user_library: false,
            ask_tag: 0,
            effect_names: Vec::new(),
            continuations: Arc::new(Mutex::new(HashMap::new())),
            next_cont_id: Arc::new(AtomicU64::new(1)),
            eval_semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_EVALS)),
            orphaned_threads: Arc::new(AtomicUsize::new(0)),
        }
    }

    #[test]
    fn test_rejected_import_edge_cases() {
        // Qualified unsafe
        assert!(rejected_import("qualified System.IO.Unsafe as Safe").is_some());
        // Extra whitespace
        assert!(rejected_import("  System.IO.Unsafe  ").is_some());
        // Safe Data imports
        assert!(rejected_import("Data.Map (Map, fromList)").is_none());
        // Tidepool modules
        assert!(rejected_import("Tidepool.Table").is_none());
        // Empty string
        assert!(rejected_import("").is_none());
    }

    #[test]
    fn test_format_error_with_source_multiline() {
        let title = "Compile Error";
        let error = "Variable not in scope: x";
        let source = "module Test where\n-- [user]\nmain = do\n  print x\n  print y\n  print z";
        let formatted = format_error_with_source(title, error, source);

        assert!(formatted.contains("## Compile Error"));
        assert!(formatted.contains("Variable not in scope: x"));
        assert!(formatted.contains("## User Code"));
        assert!(formatted.contains("main = do\n  print x\n  print y\n  print z"));
        assert!(!formatted.contains("module Test where"));
    }

    #[test]
    fn test_format_error_empty_source() {
        let formatted = format_error_with_source("Error", "msg", "");
        assert!(formatted.contains("## Error"));
        assert!(formatted.contains("msg"));
        assert!(formatted.contains("## User Code"));
    }

    #[test]
    fn test_captured_output_drain() {
        let output = CapturedOutput::new();
        output.push("line 1".to_string());
        output.push("line 2".to_string());

        let drained = output.drain();
        assert_eq!(drained, vec!["line 1", "line 2"]);

        let empty = output.drain();
        assert!(empty.is_empty());
    }
}
