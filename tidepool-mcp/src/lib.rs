//! MCP (Model Context Protocol) server library for Tidepool.
//!
//! Wraps `tidepool-runtime` in an MCP server exposing `run_haskell`,
//! `compile_haskell`, and `eval` tools. Generic over effect handler stacks
//! via `TidepoolMcpServer<H>`.

pub mod validate;

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
            "FsGrep :: Text -> Text -> Fs [(Text, Int, Text)]",
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
            "-- | Search for a regex pattern in files matching a glob.\ngrepGlob :: Text -> Text -> M [(Text, Int, Text)]\ngrepGlob pat g = send (FsGrep pat g)",
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
            "Supported: Rust | Python | TypeScript | JavaScript | Go | Java | C | Cpp | Haskell | Nix | Html | Css | Json | Yaml | Toml. ",
            "Example: hsDef \"filter\" [\"haskell/lib\"] returns the definition of filter with file/line. ",
            "Use grepGlob for structured text-level search with regex and filename globbing.",
        ),
        type_defs: &[
            "data Lang = Rust | Python | TypeScript | JavaScript | Go | Java | C | Cpp | Haskell | Nix | Html | Css | Json | Yaml | Toml",
            "data Match = Match { matchText :: Text, matchFile :: Text, matchLine :: Int, matchVars :: [(Text, Text)], matchReplacement :: Text }",
            "instance ToJSON Match where\n  toJSON (Match t f l vs r) = object ([\"text\" .= t, \"file\" .= f, \"line\" .= l] ++ (if null vs then [] else [\"vars\" .= toJSON (Map.fromList vs)]) ++ (if T.null r then [] else [\"replacement\" .= r]))",
            "var :: Match -> Text -> Text",
            "var m k = case [v | (k', v) <- matchVars m, k' == k] of { (x:_) -> x; _ -> \"\" }",
        ],
        constructors: &[
            "SgFind    :: Lang -> Text -> [Text] -> SG [Match]",
            "SgRuleFind    :: Lang -> Value -> [Text] -> SG [Match]",
            "SgPlan    :: Lang -> Text -> Text -> [Text] -> SG [Match]",
            "SgApply    :: Lang -> Text -> Text -> [Text] -> SG Int",
        ],
        helpers: &[
            "sgFind :: Lang -> Text -> [Text] -> M [Match]\nsgFind l p fs = send (SgFind l p fs)",
            "-- | Dry-run structural rewrite: matches with replacements, NO writes.\nplanRw :: Lang -> Text -> Text -> [Text] -> M [Match]\nplanRw l p r fs = send (SgPlan l p r fs)",
            "-- | Apply a structural rewrite in place. Prefer the gated `rewrite` verb.\napplyRw :: Lang -> Text -> Text -> [Text] -> M Int\napplyRw l p r fs = send (SgApply l p r fs)",
            "sgRuleFind :: Lang -> Value -> [Text] -> M [Match]\nsgRuleFind l r fs = send (SgRuleFind l r fs)",
            "rPat :: Text -> Value\nrPat p = object [\"pattern\" .= p]",
            "rKind :: Text -> Value\nrKind k = object [\"kind\" .= k]",
            "rRegex :: Text -> Value\nrRegex r = object [\"regex\" .= r]",
            "rHas :: Value -> Value\nrHas r = object [\"has\" .= (r .+. object [\"stopBy\" .= (\"end\" :: Text)])]",
            "rHasChild :: Value -> Value\nrHasChild r = object [\"has\" .= r]",
            "rInside :: Value -> Value\nrInside r = object [\"inside\" .= (r .+. object [\"stopBy\" .= (\"end\" :: Text)])]",
            "rInsideParent :: Value -> Value\nrInsideParent r = object [\"inside\" .= r]",
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
            // Recipes
            "-- | Find a Haskell function definition by name.\nhsDef :: Text -> [Text] -> M [Match]\nhsDef name paths = sgRuleFind Haskell (rAll [rKind \"function\", rHas (rField \"name\" .+. rRegex (\"^\" <> name <> \"$\"))]) paths",
            "-- | Find a Haskell function signature by name.\nhsSig :: Text -> [Text] -> M [Match]\nhsSig name paths = sgRuleFind Haskell (rAll [rKind \"signature\", rHas (rField \"name\" .+. rRegex (\"^\" <> name <> \"$\"))]) paths",
            "-- | Find a Rust function definition by name.\nrsFn :: Text -> [Text] -> M [Match]\nrsFn name paths = sgRuleFind Rust (rAll [rKind \"function_item\", rHas (rField \"name\" .+. rRegex (\"^\" <> name <> \"$\"))]) paths",
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
        description: "Suspend execution and ask the calling LLM a question. The LLM calls the resume tool with an answer, and execution continues. `askQ` attaches a schema: the suspension carries it as JSON Schema and the resume reply is validated against it server-side before re-entering the computation (invalid replies do NOT consume the continuation).",
        constructors: &[
            "Ask :: Text -> Ask Value",
            "AskWith :: Text -> Value -> Ask Value",
        ],
        type_defs: &[
            // Schema vocabulary lives on the Ask effect (always present in
            // every stack) so .tidepool/lib modules and Llm-less stacks can
            // use Q/askQ. llmJson (llm_decl) references schemaToValue from
            // here — same generated module.
            "data Schema = SObj [(Text, Schema)] | SArr Schema | SStr | SNum | SBool | SEnum [Text] | SOpt Schema",
            "data Q a = Q Schema (Value -> a) Double",
            "data Judged a = Sure a | Unsure Double a",
            "instance Functor Q where\n  fmap f (Q s p t) = Q s (f . p) t",
            "instance Applicative Q where\n  pure a = Q (SObj []) (const a) 0.6\n  Q (SObj fs1) p1 t1 <*> Q (SObj fs2) p2 t2 = Q (SObj (fs1 ++ fs2)) (\\v -> p1 v (p2 v)) (if t1 >= t2 then t1 else t2)\n  Q s1 p1 t1 <*> Q s2 p2 t2 = Q s1 (\\v -> p1 v (p2 v)) (if t1 >= t2 then t1 else t2)",
        ],
        helpers: &[
            "ask :: Text -> M Value\nask = send . Ask",
            "askWith :: Value -> Text -> M Value\naskWith meta prompt = send (AskWith prompt meta)",
            "askQ :: Q a -> Text -> M a\naskQ (Q schema parse _) prompt = parse <$> askWith (object [\"schema\" .= schemaToValue schema]) prompt",
            "getLine :: Text -> M Text\ngetLine prompt = do { v <- ask prompt; case v of { String s -> pure s; _ -> pure (show v) } }",
            "isOpt :: Schema -> Bool\nisOpt (SOpt _) = True\nisOpt _ = False",
            "innerSchema :: Schema -> Schema\ninnerSchema (SOpt s) = s\ninnerSchema s = s",
            "schemaToValue :: Schema -> Value\nschemaToValue SStr = object [\"type\" .= (\"string\" :: Text)]\nschemaToValue SNum = object [\"type\" .= (\"number\" :: Text)]\nschemaToValue SBool = object [\"type\" .= (\"boolean\" :: Text)]\nschemaToValue (SEnum vs) = object [\"type\" .= (\"string\" :: Text), \"enum\" .= vs]\nschemaToValue (SArr item) = object [\"type\" .= (\"array\" :: Text), \"items\" .= schemaToValue item]\nschemaToValue (SOpt s) = schemaToValue s\nschemaToValue (SObj fields) = object [\"type\" .= (\"object\" :: Text), \"properties\" .= object (map (\\(k,s) -> k .= schemaToValue (innerSchema s)) fields), \"required\" .= map fst (filter (not . isOpt . snd) fields)]",
            "pick :: [Text] -> Q Text\npick cats = Q (SObj [(\"pick\", SEnum cats)]) (\\v -> case v ^? key \"pick\" . _String of { Just s -> s; _ -> error \"Q: missing 'pick' in response\" }) 0.6",
            "yn :: Q Bool\nyn = Q (SObj [(\"answer\", SBool)]) (\\v -> case v ^? key \"answer\" . _Bool of { Just b -> b; _ -> error \"Q: missing 'answer' in response\" }) 0.6",
            "obj :: Schema -> Q Value\nobj s = Q s id 0.6",
            "txt :: Text -> Q Text\ntxt k = Q (SObj [(k, SStr)]) (\\v -> case v ^? key k . _String of { Just s -> s; _ -> error (\"Q: missing '\" <> k <> \"' in response\") }) 0.6",
            "num :: Text -> Q Double\nnum k = Q (SObj [(k, SNum)]) (\\v -> case v ^? key k . _Number of { Just n -> n; _ -> error (\"Q: missing '\" <> k <> \"' in response\") }) 0.6",
            "bar :: Double -> Q a -> Q a\nbar t (Q s p _) = Q s p t",
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
        type_defs: &[],
        helpers: &[
            "llm :: Text -> M Text\nllm = send . LlmChat",
            // schemaToValue lives in ask_decl (Ask is always present).
            "llmJson :: Text -> Schema -> M Value\nllmJson prompt schema = send (LlmStructured prompt (schemaToValue schema))",
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
/// Provide a single Haskell expression of type `M a`. The server wraps it in
/// a full module with the effect stack type, LANGUAGE pragmas, and imports.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EvalRequest {
    /// A single Haskell EXPRESSION of type `M a` — its value is the eval's
    /// result. Compose with `>>=`, `<&>`, `>=>`, point-free pipelines;
    /// attach a trailing `where` for local bindings. For step-by-step
    /// sequencing write an explicit `do` block (bare statement lines do
    /// NOT parse). `pure x` only to wrap a pure value — never
    /// `r <- f` followed by `pure r`.
    pub code: String,
    /// Additional Haskell imports, one per line (e.g. "Data.List (sort)").
    #[serde(default)]
    pub imports: String,
    /// Top-level definitions (functions, operators, type signatures) —
    /// where your program's real structure lives; `code` is often one
    /// call into these. Define `data` types in a `.tidepool/lib/<Mod>.hs`
    /// module instead (scaffold with `Explore.defMod`) and pull them in
    /// via `imports` — domain types belong in the library.
    #[serde(default)]
    pub helpers: String,
    /// Optional JSON input injected as `input :: Aeson.Value` binding.
    /// Also the PAYLOAD LANE: large or quote-heavy content (file bodies,
    /// generated source) rides here as a real JSON value — no Haskell
    /// string escaping — while `code` stays a short verb that consumes
    /// `input` (e.g. `writeFile path src where src = case input of { String s -> s; _ -> "" }`).
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
    /// The response to feed back to the suspended Haskell program. May be
    /// any JSON value; plain text is fine for schema-less asks. If the
    /// suspension carried a `schema`, the response is validated against it
    /// server-side BEFORE the continuation is consumed — pass the JSON
    /// directly (not stringified). A failed validation returns the
    /// violations and leaves the continuation alive for a corrected retry.
    pub response: serde_json::Value,
}

/// Request parameters for the `abort` tool.
///
/// Terminates a suspended evaluation without answering it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AbortRequest {
    /// The continuation ID returned by a suspended eval call.
    pub continuation_id: String,
    /// Optional reason, surfaced to the computation as the error message
    /// ("ask aborted by caller: <reason>").
    #[serde(default)]
    pub reason: Option<String>,
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
/// Source of the generated `Tidepool.Effects` module: effect type_defs,
/// GADTs, the `M` alias, the `error :: Text -> a` shadow, and the thin
/// send-wrapper helpers.
///
/// This exists as a REAL module (written to an include dir by
/// [`ensure_effects_module`]) rather than text spliced into the eval
/// preamble, because GHC identifies types by `(module, name)`: with one
/// importable module, the eval module and `.tidepool/lib` user modules
/// share ONE set of effect types — so user libraries can define effectful
/// verbs (`census :: Text -> M Value`) that unify with eval code.
pub fn effects_module_source(effects: &[EffectDecl]) -> String {
    let mut out = String::new();
    out.push_str("{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, ScopedTypeVariables, ExtendedDefaultRules #-}\n");
    out.push_str("-- GENERATED by the tidepool MCP server from its effect handler\n");
    out.push_str("-- declarations. Do not edit; regenerated (content-addressed) at startup.\n");
    out.push_str("module Tidepool.Effects where\n");
    out.push_str("import Tidepool.Prelude hiding (error)\n");
    out.push_str("import qualified Data.Text as T\n");
    out.push_str("import qualified Data.Map.Strict as Map\n");
    out.push_str("import qualified Tidepool.Aeson.KeyMap as KM\n");
    out.push_str("import Control.Monad.Freer hiding (run)\n");
    out.push_str("import qualified Prelude as P\n");
    out.push_str("default (Int, Double, Text)\n");
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

    // Thin effect helpers (send-wrappers and recipes)
    for eff in effects {
        for h in eff.helpers {
            out.push_str(h);
            out.push('\n');
        }
    }
    out
}

/// Write the generated `Tidepool/Effects.hs` into a content-addressed
/// directory and return that directory (an include root). Idempotent:
/// the path is keyed on the module source, so distinct effect stacks
/// coexist and repeat startups reuse the same dir.
pub fn ensure_effects_module(effects: &[EffectDecl]) -> std::io::Result<PathBuf> {
    use std::hash::{Hash, Hasher};
    let src = effects_module_source(effects);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut hasher);
    let root = std::env::temp_dir().join(format!("tidepool-effects-{:016x}", hasher.finish()));
    let module_dir = root.join("Tidepool");
    let module_path = module_dir.join("Effects.hs");
    if !module_path.exists() {
        std::fs::create_dir_all(&module_dir)?;
        std::fs::write(&module_path, src)?;
    }
    Ok(root)
}

pub fn build_preamble(effects: &[EffectDecl], user_library: bool) -> String {
    let mut out = String::new();
    out.push_str("{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables, ExtendedDefaultRules #-}\n");
    out.push_str("module Expr where\n");
    out.push_str("import Tidepool.Prelude hiding (error)\n");
    // Effect GADTs, `M`, the `error` shadow, and the send-wrapper helpers
    // live in the generated Tidepool.Effects module (see
    // `effects_module_source`) so .tidepool/lib modules can import the
    // SAME types and define effectful verbs.
    out.push_str("import Tidepool.Effects\n");
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
    out.push_str("default (Int, Double, Text)\n");
    out.push('\n');

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
            // Q itself (data Q, Schema, pick/yn/obj/txt/num/bar, askQ) lives
            // in the generated Tidepool.Effects module (ask_decl) so
            // .tidepool/lib modules can use it. Only the tier-1 cascade —
            // Haiku-first, escalate-to-caller — lives here, because it
            // needs llmJson.
            out.push_str(
                "-- Heuristic combinators: tier-1 cascade (Haiku-first, Ask-on-uncertainty)\n",
            );
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
            // Escalation rewrap: the validator enforces the BARE schema on
            // escalated replies, but tier-1 parsers for non-SObj schemas
            // expect h_aug's {"value": ...} wrapping — re-wrap so both
            // tiers hand `parse` the same shape.
            out.push_str(concat!(
                "h_wrap :: Schema -> Value -> Value\n",
                "h_wrap (SObj _) v = v\n",
                "h_wrap _ v = object [\"value\" .= v]\n",
            ));
            // ?? operator: ask Haiku, auto-escalate on low confidence.
            // Escalation carries the schema STRUCTURALLY (suspension JSON
            // "schema" field; resume validated server-side); the tier-1
            // draft rides in the prompt as a proposed default.
            out.push_str(concat!(
                "infixl 1 ??\n",
                "(??) :: Q a -> Text -> M a\n",
                "(Q schema parse threshold) ?? prompt = do\n",
                "  r <- llmJson prompt (h_aug schema)\n",
                "  let c = h_conf r\n",
                "  v <- if c >= threshold then pure (h_strip r)\n",
                "       else h_wrap schema <$> askWith (object [\"schema\" .= schemaToValue schema]) (prompt <> \"\\n[draft \" <> pack (showDouble c) <> \"]: \" <> show (h_strip r))\n",
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
                "      v <- askWith (object [\"schema\" .= schemaToValue schema]) (prompt <> \"\\n[draft \" <> pack (showDouble c) <> \"]: \" <> show (h_strip r))\n",
                "      pure (Unsure c (parse (h_wrap schema v)))\n",
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
        "`code` is a single Haskell EXPRESSION of type `M a`; its value is the ",
        "eval's result. The server wraps it in a module with the effect stack, ",
        "pragmas, and imports. Compose with `>>=`, `<&>`, `>=>`, point-free ",
        "pipelines; attach a trailing `where` for local bindings. For ",
        "step-by-step sequencing write an explicit `do` block — bare statement ",
        "lines do NOT parse. ",
        "Use `send (Constructor args)` to invoke effects. ",
        "First call is slow (~2s). Subsequent calls are cached.\n",
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

        let has_fs = effects.iter().any(|e| e.type_name == "Fs");
        if has_fs {
            desc.push_str(concat!(
                "\nExamples (idiomatic — expression-first):\n",
                "  glob \"**/*.rs\" >>= mapM (\\p -> (,) p <$> getFileSize p)\n",
                "  do { src <- readFile \"CLAUDE.md\"; pure (stake 5 (lines src)) }  -- explicit do when sequencing\n",
            ));
            if has_llm && has_ask_desc {
                desc.push_str(
                    "  glob \"**/*.hs\" >>= filterM (readFile >=> \\s -> yn ?? (\"test-only?\\n\" <> stake 2000 s))\n",
                );
            }
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
fn extract_sigs(src: &str) -> Vec<String> {
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
fn library_vocab(dir: &std::path::Path) -> String {
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

/// Unwrap double-encoded JSON strings if they contain an object or array.
fn normalize_input(v: &serde_json::Value) -> serde_json::Value {
    if let serde_json::Value::String(s) = v {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(s) {
            // MCP clients stringify the input param. Unwrap one level for
            // composite values AND strings (#315: a stringified bare-string
            // payload otherwise reaches Haskell with its quotes/escapes as
            // literal characters). Numbers/bools stay as-is: "42" is more
            // plausibly the literal text than a stringified number.
            if parsed.is_object() || parsed.is_array() || parsed.is_string() {
                return parsed;
            }
        }
    }
    v.clone()
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

    // User code is a real top-level binding: a single EXPRESSION (explicit
    // `do` required for sequencing), so trailing `where`-clauses are legal
    // and the inferred type rides into the wrapper below.
    out.push_str("__user =\n");
    for line in code.lines() {
        out.push_str(&format!("  {}\n", line));
    }
    out.push('\n');

    out.push_str(&format!("result :: Eff {} Value\n", effect_stack));
    out.push_str("result = do\n");
    if budget.is_some() {
        out.push_str("  kvSet \"__sayChars\" (toJSON (0 :: Int))\n");
    }
    out.push_str("  _r <- __user\n");
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
    // Extract user-written code: between the "-- [user]" marker and the
    // generated `result ::` wrapper (helpers + input + the __user binding).
    // Echoing the budget plumbing below teaches callers the wrong dialect.
    let user_section = source
        .find("-- [user]\n")
        .map(|pos| &source[pos + "-- [user]\n".len()..])
        .unwrap_or(source);
    let user_section = user_section
        .find("\nresult ::")
        .map(|pos| &user_section[..pos])
        .unwrap_or(user_section)
        .trim_end();
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
    /// `meta` carries AskWith metadata (e.g. a "schema" key) as JSON.
    Suspended {
        prompt: String,
        meta: Option<serde_json::Value>,
    },
    /// The program completed successfully.
    Completed { result: String },
    /// The program encountered an error.
    Error { error: String },
}

/// Messages from the MCP server to the blocked eval thread.
///
/// `Answer` carries the CANONICAL validated JSON value (the validator's
/// parse, not the raw resume text — single source of truth). `Abort`
/// terminates the ask as a handler error.
enum ResumeMsg {
    Answer(serde_json::Value),
    Abort(String),
}

/// A suspended evaluation session, waiting for a resume call.
struct EvalSession {
    /// Send a response to unblock the eval thread's Ask handler.
    response_tx: std::sync::mpsc::Sender<ResumeMsg>,
    /// Receive the next message (Completed, Suspended, or Error) from the eval thread.
    session_rx: tokio::sync::mpsc::UnboundedReceiver<SessionMessage>,
    /// The Haskell source code, for error formatting on resume.
    source: Arc<str>,
    /// When this session was created, for eviction ordering. Refreshed on
    /// failed validation so a retrying continuation isn't the eviction
    /// victim while its caller fixes the reply.
    created_at: std::time::Instant,
    /// Output capture for this session.
    captured_output: CapturedOutput,
    /// JSON Schema from the suspension's AskWith metadata ("schema" key);
    /// resume replies are validated against it BEFORE the continuation is
    /// consumed.
    expected_schema: Option<serde_json::Value>,
}

/// Wraps an existing effect dispatcher and intercepts the Ask effect tag.
///
/// When the Ask tag is hit, sends a `Suspended` message via the session channel
/// and blocks the current thread until a response arrives.
struct AskDispatcher {
    inner: Box<dyn McpEffectHandler>,
    ask_tag: u64,
    session_tx: tokio::sync::mpsc::UnboundedSender<SessionMessage>,
    response_rx: std::sync::mpsc::Receiver<ResumeMsg>,
}

impl DispatchEffect<CapturedOutput> for AskDispatcher {
    fn dispatch(
        &mut self,
        tag: u64,
        request: &tidepool_eval::value::Value,
        cx: &tidepool_effect::dispatch::EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        if tag == self.ask_tag {
            // Extract prompt (+ AskWith metadata) from the Ask constructor
            let (prompt, meta) = extract_ask_request(request, cx.table())
                .map_err(tidepool_effect::error::EffectError::Handler)?;

            // Signal suspension to the MCP server
            let _ = self
                .session_tx
                .send(SessionMessage::Suspended { prompt, meta });

            // Block until the MCP server sends a response via the resume
            // (or abort) tool. The server side has already JSON-parsed and
            // schema-validated the response — what arrives is canonical.
            let msg = self.response_rx.recv().map_err(|_| {
                tidepool_effect::error::EffectError::Handler(
                    "Ask session closed (timeout or client disconnected)".into(),
                )
            })?;

            match msg {
                ResumeMsg::Answer(json_val) => {
                    let core_val = json_val
                        .to_value(cx.table())
                        .map_err(tidepool_effect::error::EffectError::Bridge)?;
                    Ok(core_val.into())
                }
                ResumeMsg::Abort(reason) => Err(tidepool_effect::error::EffectError::Handler(
                    format!("ask aborted by caller: {reason}"),
                )),
            }
        } else {
            self.inner.dispatch(tag, request, cx)
        }
    }
}

/// Extract the prompt (and optional AskWith metadata) from an Ask request.
///
/// The request is `Con(Ask, [prompt_val])` or `Con(AskWith, [prompt_val,
/// meta_val])`, dispatched by constructor name. Returns an error if the
/// prompt cannot be extracted (e.g., unevaluated closure due to a crash in
/// the string-building expression).
fn extract_ask_request(
    request: &tidepool_eval::value::Value,
    table: &tidepool_repr::DataConTable,
) -> Result<(String, Option<serde_json::Value>), String> {
    use tidepool_eval::value::Value;

    let Value::Con(con_id, fields) = request else {
        return Err(format!(
            "ask received unexpected request shape (expected Con(Ask|AskWith, ..)): {:?}",
            request
        ));
    };

    let con_name = table.name_of(*con_id).unwrap_or("<unknown>");
    let has_meta = match con_name {
        "Ask" => false,
        "AskWith" => true,
        other => {
            return Err(format!(
                "ask received unexpected constructor {other:?} (expected Ask or AskWith)"
            ))
        }
    };

    let Some(prompt_val) = fields.first() else {
        return Err(format!(
            "ask received unexpected request shape (expected Con({con_name}, ..)): {:?}",
            request
        ));
    };

    // Try using FromCore (handles Text, LitString, [Char])
    let prompt = match String::from_value(prompt_val, table) {
        Ok(s) => s,
        Err(e) => {
            // Provide diagnostic: the prompt text couldn't be extracted,
            // likely because the string-building expression crashed
            // (e.g., unresolved external, partial evaluation).
            return Err(format!(
                "ask prompt could not be evaluated to Text: {e}. \
                 The expression passed to `ask` likely crashed during evaluation \
                 (check for unresolved externals or runtime errors in the prompt string)."
            ));
        }
    };

    let meta = if has_meta {
        // Requests arrive fully forced from the JIT bridge
        // (heap_to_value_forcing), so the aeson Value sub-tree is already
        // materialized — value_to_json renders it directly.
        fields
            .get(1)
            .map(|m| tidepool_runtime::value_to_json(m, table, 0))
    } else {
        None
    };

    Ok((prompt, meta))
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
        response_tx: std::sync::mpsc::Sender<ResumeMsg>,
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
                    SessionMessage::Suspended { prompt, meta } => {
                        tracing::info!(prompt = %prompt, "{} suspended on Ask", op);
                        let cont_id = self.next_continuation_id();
                        let mut json_obj = serde_json::json!({
                            "suspended": true,
                            "continuation_id": cont_id,
                            "prompt": prompt,
                        });
                        // AskWith metadata: hoist "schema" top-level (it
                        // arms resume validation); everything else rides
                        // under "meta" verbatim — no reserved-key
                        // collisions, no silent drops.
                        let mut expected_schema = None;
                        match meta {
                            Some(serde_json::Value::Object(mut meta_map)) => {
                                if let Some(obj) = json_obj.as_object_mut() {
                                    if let Some(schema) = meta_map.remove("schema") {
                                        obj.insert("schema".into(), schema.clone());
                                        expected_schema = Some(schema);
                                    }
                                    if !meta_map.is_empty() {
                                        obj.insert(
                                            "meta".into(),
                                            serde_json::Value::Object(meta_map),
                                        );
                                    }
                                }
                            }
                            Some(other) => {
                                // Non-object metadata: pass through verbatim.
                                if let Some(obj) = json_obj.as_object_mut() {
                                    obj.insert("meta".into(), other);
                                }
                            }
                            None => {}
                        }
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
                                expected_schema,
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

                // The program's last words are the cheapest forensics there
                // are — surface anything it printed before the signal.
                let output = captured_output.snapshot();
                if !output.is_empty() {
                    crash_info.push_str("\n\n## Output Before Crash\n");
                    for line in &output {
                        crash_info.push_str(line);
                        crash_info.push('\n');
                    }
                }

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

                // Output printed before the deadline answers "which step was
                // it on?" — the question a timeout always raises. snapshot()
                // (not drain) because the orphaned thread may still write.
                let output = captured_output.snapshot();
                let mut detail = format!(
                    "{} timed out after {}s. This usually means an infinite loop or unbounded recursion.",
                    op, EVAL_TIMEOUT_SECS
                );
                if !output.is_empty() {
                    detail.push_str("\n\n## Output Before Timeout\n");
                    for line in &output {
                        detail.push_str(line);
                        detail.push('\n');
                    }
                }
                let error_msg = format_error_with_source("Timeout", &detail, &source);
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
        let normalized_input = req.input.as_ref().map(normalize_input);
        let source: Arc<str> = template_haskell(
            &self.haskell_preamble,
            &self.effect_stack_type,
            &req.code,
            &all_imports,
            &req.helpers,
            normalized_input.as_ref(),
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
        let (response_tx, response_rx) = std::sync::mpsc::channel::<ResumeMsg>();

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

        // Validate-then-consume, all inside ONE lock scope: a reply that
        // fails schema validation must NOT consume the one-shot
        // continuation (the caller fixes and retries), and two concurrent
        // resumes must not both pass validation and both send.
        let session = {
            let mut conts = self.continuations.lock();
            let expected_schema = conts
                .get(&req.continuation_id)
                .ok_or_else(|| {
                    McpError::invalid_params(
                        format!(
                            "Unknown or expired continuation_id: {}",
                            req.continuation_id
                        ),
                        None,
                    )
                })?
                .expected_schema
                .clone();

            match validate::validate_response(expected_schema.as_ref(), &req.response) {
                validate::Outcome::Invalid(violations) => {
                    // Anti-starvation: a retrying continuation must not be
                    // the oldest-first eviction victim while its caller
                    // fixes the reply.
                    if let Some(session) = conts.get_mut(&req.continuation_id) {
                        session.created_at = std::time::Instant::now();
                    }
                    let body = serde_json::json!({
                        "validation_failed": true,
                        "violations": violations.iter().map(|v| v.to_json()).collect::<Vec<_>>(),
                        "schema": expected_schema,
                        "continuation_id": req.continuation_id,
                        "continuation_not_consumed": true,
                    });
                    return Ok(CallToolResult::error(vec![Content::text(format!(
                        "Response does not match the suspension's schema. Call resume again \
                         with the same continuation_id and a corrected response (or abort).\n{}",
                        body
                    ))]));
                }
                validate::Outcome::Valid(canonical) => {
                    let session = conts
                        .remove(&req.continuation_id)
                        .expect("session present: checked under the same lock");
                    // Send the canonical validated value to the blocked
                    // eval thread.
                    session
                        .response_tx
                        .send(ResumeMsg::Answer(canonical))
                        .map_err(|_| {
                            McpError::internal_error("eval thread is no longer running", None)
                        })?;
                    session
                }
            }
        };

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

    async fn abort(&self, req: AbortRequest) -> Result<CallToolResult, McpError> {
        tracing::info!(continuation_id = %req.continuation_id, "abort request");

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

        let reason = req
            .reason
            .unwrap_or_else(|| "aborted by caller".to_string());
        session
            .response_tx
            .send(ResumeMsg::Abort(reason))
            .map_err(|_| McpError::internal_error("eval thread is no longer running", None))?;

        let source = session.source.clone();
        let response_tx = session.response_tx.clone();
        let captured = session.captured_output.clone();

        // The eval terminates as a normal error result ("ask aborted by
        // caller: ...") carrying output-so-far — and its thread + semaphore
        // permit are freed instead of waiting for pressure eviction.
        self.handle_session_result(
            "abort",
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
            "abort" => {
                let req: AbortRequest = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| {
                        McpError::invalid_params(format!("invalid params: {}", e), None)
                    })?;
                self.abort(req).await
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
                     call this tool with the continuation_id and your response to the prompt. \
                     If the suspension carried a \"schema\" field, the response must be JSON \
                     matching it — pass the JSON value directly (string/enum schemas also \
                     accept raw text). A response that fails validation does NOT consume the \
                     continuation: the violations are returned and you can call resume again \
                     with the same continuation_id. If you cannot answer, call abort instead."
                        .into(),
                ),
                input_schema: schema_to_map(schemars::schema_for!(ResumeRequest))?,
                output_schema: None,
                annotations: None,
                icons: None,
                meta: None,
                execution: None,
            },
            Tool {
                name: "abort".into(),
                title: None,
                description: Some(
                    "Abort a suspended Haskell evaluation without answering it. Use when you \
                     cannot answer a suspension's question, or to clean up a suspended loop \
                     you are abandoning (a suspended eval pins a thread until evicted). The \
                     computation terminates with an error result (\"ask aborted by caller: \
                     <reason>\") carrying any output produced so far."
                        .into(),
                ),
                input_schema: schema_to_map(schemars::schema_for!(AbortRequest))?,
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
        // The generated Tidepool.Effects module must be on the include path
        // for every eval (the preamble imports it). Failure is survivable
        // here — evals will fail with a clear missing-module error.
        let mut include = Vec::new();
        match ensure_effects_module(&decls) {
            Ok(dir) => include.push(dir),
            Err(e) => eprintln!("[tidepool] failed to write Tidepool.Effects module: {e}"),
        }
        Self {
            inner: TidepoolMcpServerImpl {
                handler_factory: Arc::new(handler),
                include,
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

    /// Add include paths for Haskell module resolution. Extends the
    /// existing set (which already contains the generated
    /// `Tidepool.Effects` dir).
    pub fn with_include(mut self, paths: Vec<PathBuf>) -> Self {
        self.inner.include.extend(paths);
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
            self.inner.include.push(user_lib.clone());
            if self.inner.has_user_library {
                // Rebuild preamble with user library import
                let mut decls = H::collect_decls();
                decls.push(ask_decl());
                self.inner.haskell_preamble = build_preamble(&decls, true);
                // Append note + vocabulary digest to tool description
                self.inner.eval_tool_description.push_str(
                    "\n\nUser library: `Library` is auto-imported from `.tidepool/lib/Library.hs` \
                     and re-exports every module below — all names are in scope bare. \
                     Check this vocabulary for an existing combinator with the right shape \
                     (fold/unfold/loop/batch) BEFORE hand-rolling a recursive helper. \
                     New `data` types go in a `.tidepool/lib/<Mod>.hs` module \
                     (scaffold with `Explore.defMod`):\n",
                );
                self.inner
                    .eval_tool_description
                    .push_str(&library_vocab(&user_lib));
                self.inner.eval_tool_description.push_str(concat!(
                    "\nWith the library:\n",
                    "  glob \"**/*.rs\" >>= mapM (\\p -> (,) p <$> getFileSize p) <&> sizeRank 9\n",
                    "  seek \"where are case traps emitted?\" 5  -- steered search: suspends to you each round\n",
                ));
                // PATTERNS.md lives beside lib/, at .tidepool/PATTERNS.md.
                let patterns = user_lib.parent().map(|p| p.join("PATTERNS.md"));
                if patterns.as_deref().is_some_and(|p| p.exists()) {
                    self.inner.eval_tool_description.push_str(
                        "\nPattern catalog: read `.tidepool/PATTERNS.md` for composition idioms.\n",
                    );
                }
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

    /// Effects module + preamble concatenated: content assertions that
    /// predate the importable-effects split check against the union.
    fn generated_sources(effects: &[EffectDecl], user_library: bool) -> String {
        let mut s = effects_module_source(effects);
        s.push_str(&build_preamble(effects, user_library));
        s
    }

    #[test]
    fn test_preamble_structural_search_updates() {
        let effects = vec![sg_decl(), fs_decl()];
        let preamble = generated_sources(&effects, false);

        // Verify rHas includes stopBy: end (note: Haskell string escape)
        assert!(preamble.contains(
            "rHas r = object [\"has\" .= (r .+. object [\"stopBy\" .= (\"end\" :: Text)])]"
        ));

        // Verify rHasChild exists and lacks stopBy
        assert!(
            preamble.contains("rHasChild :: Value -> Value\nrHasChild r = object [\"has\" .= r]")
        );

        // Verify hsDef and rsFn recipes exist
        assert!(preamble.contains("hsDef :: Text -> [Text] -> M [Match]"));
        assert!(preamble.contains("rsFn :: Text -> [Text] -> M [Match]"));

        // Verify grepGlob exists in Fs section
        assert!(preamble.contains("grepGlob :: Text -> Text -> M [(Text, Int, Text)]"));

        // Verify Match record syntax
        assert!(preamble.contains("data Match = Match { matchText :: Text, matchFile :: Text, matchLine :: Int, matchVars :: [(Text, Text)], matchReplacement :: Text }"));
        assert!(preamble.contains(
            "var m k = case [v | (k', v) <- matchVars m, k' == k] of { (x:_) -> x; _ -> \"\" }"
        ));
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
        let preamble = generated_sources(&effects, false);
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
        let source = "do\n  let x = 42\n  pure x";

        let result = template_haskell(&preamble, &stack, source, "", "", None, None);

        assert!(result.contains("module Expr where"));
        assert!(result.contains("import Control.Monad.Freer hiding (run)"));
        // GADTs live in the generated Tidepool.Effects module now.
        assert!(result.contains("import Tidepool.Effects"));
        assert!(effects_module_source(&effects).contains("data Console a where"));
        // User code is a real top-level binding (expression-first contract).
        assert!(result.contains("__user =\n  do\n    let x = 42\n    pure x"));
        assert!(result.contains("result :: Eff '[Console] Value"));
        assert!(result.contains("result = do"));
        assert!(result.contains("  _r <- __user"));
    }

    #[test]
    fn test_template_haskell_expression_forms() {
        let effects = vec![EffectDecl {
            type_name: "Console",
            description: "",
            constructors: &["Print :: Text -> Console ()"],
            type_defs: &[],
            helpers: &[],
        }];
        let preamble = build_preamble(&effects, false);
        let stack = build_effect_stack_type(&effects);

        // Multi-line composition expression: continuation indentation rides
        // through verbatim under the 2-space binding indent.
        let pipeline = "glob \"**/*.rs\"\n  >>= mapM getFileSize\n  <&> sizeRank 9";
        let r = template_haskell(&preamble, &stack, pipeline, "", "", None, None);
        assert!(r.contains(
            "__user =\n  glob \"**/*.rs\"\n    >>= mapM getFileSize\n    <&> sizeRank 9"
        ));

        // Trailing where-clause is legal: __user is a genuine declaration.
        let with_where = "sizeRank 9 <$> sized\n  where\n    sized = mapM go =<< glob \"**/*.rs\"";
        let r = template_haskell(&preamble, &stack, with_where, "", "", None, None);
        assert!(r.contains("__user =\n  sizeRank 9 <$> sized\n    where\n      sized ="));
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
    fn test_extract_sigs() {
        let src = "\
{-# LANGUAGE NoImplicitPrelude #-}
-- | A comment with a fake sig :: not real
module Lib where

import Tidepool.Prelude

-- | Single-line.
oracle :: Text -> M Text
oracle q = do
  a <- ask q
  pure (vshow a)

-- | Multi-line: continuations join.
steerM :: Monad m
       => (Int -> Int -> a -> m r)
       -> b -> [a] -> m b
steerM suspend step = go 0
  where
    go _ acc [] = pure acc

type Vocab s = [(Text, Text -> s -> M s)]
data Rose a = Rose a [Rose a]
data Console a where
  Print :: Text -> Console ()

(??) :: Q a -> Text -> M a
(Q s p t) ?? prompt = undefined
";
        let sigs = extract_sigs(src);
        assert!(sigs.contains(&"oracle :: Text -> M Text".to_string()));
        assert!(sigs.contains(
            &"steerM :: Monad m => (Int -> Int -> a -> m r) -> b -> [a] -> m b".to_string()
        ));
        assert!(sigs.contains(&"type Vocab s = [(Text, Text -> s -> M s)]".to_string()));
        assert!(sigs.contains(&"data Rose a = Rose a [Rose a]".to_string()));
        assert!(sigs.contains(&"(??) :: Q a -> Text -> M a".to_string()));
        // GADT `where` heads and indented constructor sigs are excluded;
        // comment-embedded `::` never matches.
        assert!(!sigs.iter().any(|s| s.contains("Console")));
        assert!(!sigs.iter().any(|s| s.contains("fake sig")));
        // Function bodies never leak into signatures.
        assert!(!sigs.iter().any(|s| s.contains("go 0")));
    }

    #[test]
    fn test_preamble_includes_helpers() {
        let decls = standard_decls();
        let preamble = generated_sources(&decls, false);
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
        let source = "preamble stuff\n-- [user]\nhelper :: Int\nhelper = 7\n\n__user =\n  pure helper\n\nresult :: Eff '[] Value\nresult = do\n  _r <- __user\n  paginateResult 4096 (toJSON _r)\n";
        let formatted = format_error_with_source(title, error, source);

        assert!(formatted.contains("## Error"));
        assert!(formatted.contains("Type mismatch"));
        assert!(formatted.contains("## User Code"));
        // Helpers + the __user binding are echoed…
        assert!(formatted.contains("helper :: Int"));
        assert!(formatted.contains("__user =\n  pure helper"));
        // …but the preamble and the budget plumbing are trimmed.
        assert!(!formatted.contains("preamble stuff"));
        assert!(!formatted.contains("result ="));
        assert!(!formatted.contains("paginateResult"));
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
        assert_eq!(decl.constructors.len(), 2);
        assert!(decl.constructors[0].contains("Ask :: Text -> Ask Value"));
        assert!(decl.constructors[1].contains("AskWith :: Text -> Value -> Ask Value"));
        // The Q layer lives on the Ask effect (always present in every
        // stack) so .tidepool/lib modules and Llm-less stacks can use it.
        let type_defs = decl.type_defs.join("\n");
        assert!(type_defs.contains("data Schema"));
        assert!(type_defs.contains("data Q a = Q Schema (Value -> a) Double"));
        let helpers = decl.helpers.join("\n");
        assert!(helpers.contains("askQ :: Q a -> Text -> M a"));
        assert!(helpers.contains("askWith :: Value -> Text -> M Value"));
        assert!(helpers.contains("schemaToValue :: Schema -> Value"));
        assert!(helpers.contains("pick :: [Text] -> Q Text"));
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
        let preamble = generated_sources(&decls, false);
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
        let preamble = generated_sources(&decls, false);
        assert!(preamble.contains("import Control.Monad.Freer hiding (run)"));
        // Our run helper should still be present
        assert!(preamble.contains("run :: Text -> M (Int, Text, Text)\nrun = send . Run"));
    }

    #[test]
    fn test_preamble_text_error_shadow() {
        let decls = standard_decls();
        let preamble = generated_sources(&decls, false);
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
        // Tier-1 cascade operators (preamble, gated on Llm + Ask)
        assert!(preamble.contains("(??) :: Q a -> Text -> M a"));
        assert!(preamble.contains("(?!) :: Q a -> Text -> M (Judged a)"));
        assert!(preamble.contains("triage :: Q b -> (a -> Text) -> [a] -> M [(a, b)]"));
        assert!(preamble.contains("survey :: Eq b => Q b -> (a -> Text) -> [a] -> M [(b, Int)]"));
        assert!(preamble.contains("sift :: Q Bool -> (a -> Text) -> [a] -> M ([a], [a])"));
        // Escalation carries the schema structurally via askWith
        assert!(preamble.contains("askWith (object [\"schema\" .= schemaToValue schema])"));
        assert!(!preamble.contains("[haiku"));
        // Q itself lives in the generated Tidepool.Effects module so
        // .tidepool/lib modules can import it
        let effects_mod = effects_module_source(&decls);
        assert!(effects_mod.contains("data Q a = Q Schema (Value -> a) Double"));
        assert!(effects_mod.contains("data Judged a = Sure a | Unsure Double a"));
        assert!(effects_mod.contains("pick :: [Text] -> Q Text"));
        assert!(effects_mod.contains("yn :: Q Bool"));
        assert!(effects_mod.contains("obj :: Schema -> Q Value"));
        assert!(effects_mod.contains("txt :: Text -> Q Text"));
        assert!(effects_mod.contains("num :: Text -> Q Double"));
        assert!(effects_mod.contains("bar :: Double -> Q a -> Q a"));
        assert!(effects_mod.contains("askQ :: Q a -> Text -> M a"));
        // and NOT duplicated in the preamble (one definition site)
        assert!(!preamble.contains("data Q a"));
        assert!(!preamble.contains("pick :: [Text] -> Q Text"));
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
        let preamble = generated_sources(&decls, false);
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
    fn test_json_to_haskell_escapes_control_chars() {
        let v = serde_json::json!({"multi\nline\tkey": "line1\nline2\twith\rcontrols"});
        let rendered = json_to_haskell(&v);
        assert!(!rendered.contains('\n'), "raw newline leaked: {rendered}");
        assert!(rendered.contains("\\n"), "{rendered}");
        assert!(rendered.contains("\\t"), "{rendered}");
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
            meta: None,
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
    async fn test_suspended_meta_schema_hoisted() {
        let server = create_mock_server();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (resp_tx, _resp_rx) = std::sync::mpsc::channel();
        let source: Arc<str> = "test source".into();
        let captured = CapturedOutput::new();

        tx.send(SessionMessage::Suspended {
            prompt: "classify".into(),
            meta: Some(serde_json::json!({
                "schema": {"type": "string", "enum": ["a", "b"]},
                "moves": ["grep", "view"],
            })),
        })
        .unwrap();

        let res = server
            .handle_session_result("eval", rx, source, resp_tx, captured, None)
            .await
            .unwrap();
        let text = match &res.content[0].raw {
            RawContent::Text(t) => &t.text,
            _ => panic!("Expected text content"),
        };
        let json: serde_json::Value = serde_json::from_str(text).unwrap();
        // "schema" hoisted top-level; remaining metadata under "meta"
        assert_eq!(json["schema"]["enum"], serde_json::json!(["a", "b"]));
        assert_eq!(json["meta"]["moves"], serde_json::json!(["grep", "view"]));
        assert!(json.get("moves").is_none());

        // ...and stored as expected_schema for resume validation
        let cont_id = json["continuation_id"].as_str().unwrap();
        let conts = server.continuations.lock();
        assert!(conts[cont_id].expected_schema.is_some());
    }

    /// Hand-insert a suspended session carrying a schema; resume with an
    /// invalid reply (continuation must survive), then a valid one (the
    /// CANONICAL value must cross the channel and the continuation must be
    /// consumed).
    #[tokio::test]
    async fn test_resume_validation_fail_then_retry() {
        let server = create_mock_server();
        let (resp_tx, resp_rx) = std::sync::mpsc::channel::<ResumeMsg>();
        let (sess_tx, sess_rx) = tokio::sync::mpsc::unbounded_channel();

        server.continuations.lock().insert(
            "cont_t1".into(),
            EvalSession {
                response_tx: resp_tx,
                session_rx: sess_rx,
                source: "src".into(),
                created_at: std::time::Instant::now(),
                captured_output: CapturedOutput::new(),
                expected_schema: Some(serde_json::json!({
                    "type": "object",
                    "properties": {"pick": {"type": "string", "enum": ["bug", "refactor"]}},
                    "required": ["pick"],
                })),
            },
        );

        // 1: invalid reply — error result, continuation NOT consumed
        let res = server
            .resume(ResumeRequest {
                continuation_id: "cont_t1".into(),
                response: serde_json::json!("just some prose"),
            })
            .await
            .unwrap();
        assert_eq!(res.is_error, Some(true));
        let text = match &res.content[0].raw {
            RawContent::Text(t) => &t.text,
            _ => panic!("Expected text content"),
        };
        assert!(text.contains("validation_failed"));
        assert!(text.contains("cont_t1"));
        assert!(server.continuations.lock().contains_key("cont_t1"));

        // 2: valid retry on the SAME continuation_id. Pre-load the session
        // channel so handle_session_result returns immediately.
        sess_tx
            .send(SessionMessage::Completed {
                result: "\"ok\"".into(),
            })
            .unwrap();
        let res = server
            .resume(ResumeRequest {
                continuation_id: "cont_t1".into(),
                response: serde_json::json!({"pick": "bug", "rationale": "extra keys fine"}),
            })
            .await
            .unwrap();
        assert_eq!(res.is_error, Some(false));
        // canonical value crossed the channel
        match resp_rx.try_recv().unwrap() {
            ResumeMsg::Answer(v) => assert_eq!(v["pick"], serde_json::json!("bug")),
            ResumeMsg::Abort(_) => panic!("expected Answer"),
        }
        // consumed: a third resume is invalid_params
        assert!(!server.continuations.lock().contains_key("cont_t1"));
        let err = server
            .resume(ResumeRequest {
                continuation_id: "cont_t1".into(),
                response: serde_json::json!({"pick": "bug"}),
            })
            .await;
        assert!(err.is_err());
    }

    /// Stringified-JSON replies to object schemas unwrap one level (the
    /// #315 failure mode) and deliver the parsed object.
    #[tokio::test]
    async fn test_resume_stringified_object_unwraps() {
        let server = create_mock_server();
        let (resp_tx, resp_rx) = std::sync::mpsc::channel::<ResumeMsg>();
        let (sess_tx, sess_rx) = tokio::sync::mpsc::unbounded_channel();

        server.continuations.lock().insert(
            "cont_t2".into(),
            EvalSession {
                response_tx: resp_tx,
                session_rx: sess_rx,
                source: "src".into(),
                created_at: std::time::Instant::now(),
                captured_output: CapturedOutput::new(),
                expected_schema: Some(serde_json::json!({
                    "type": "object",
                    "properties": {"answer": {"type": "boolean"}},
                    "required": ["answer"],
                })),
            },
        );
        sess_tx
            .send(SessionMessage::Completed {
                result: "true".into(),
            })
            .unwrap();

        let res = server
            .resume(ResumeRequest {
                continuation_id: "cont_t2".into(),
                response: serde_json::json!("{\"answer\": true}"),
            })
            .await
            .unwrap();
        assert_eq!(res.is_error, Some(false));
        match resp_rx.try_recv().unwrap() {
            ResumeMsg::Answer(v) => assert_eq!(v, serde_json::json!({"answer": true})),
            ResumeMsg::Abort(_) => panic!("expected Answer"),
        }
    }

    /// abort consumes the continuation and the eval terminates as an error.
    #[tokio::test]
    async fn test_abort_consumes_continuation() {
        let server = create_mock_server();
        let (resp_tx, resp_rx) = std::sync::mpsc::channel::<ResumeMsg>();
        let (sess_tx, sess_rx) = tokio::sync::mpsc::unbounded_channel();

        server.continuations.lock().insert(
            "cont_t3".into(),
            EvalSession {
                response_tx: resp_tx,
                session_rx: sess_rx,
                source: "src".into(),
                created_at: std::time::Instant::now(),
                captured_output: CapturedOutput::new(),
                expected_schema: None,
            },
        );
        // In a real run the eval thread receives Abort and sends Error;
        // emulate it.
        sess_tx
            .send(SessionMessage::Error {
                error: "ask aborted by caller: cannot answer".into(),
            })
            .unwrap();

        let res = server
            .abort(AbortRequest {
                continuation_id: "cont_t3".into(),
                reason: Some("cannot answer".into()),
            })
            .await
            .unwrap();
        assert_eq!(res.is_error, Some(true));
        let text = match &res.content[0].raw {
            RawContent::Text(t) => &t.text,
            _ => panic!("Expected text content"),
        };
        assert!(text.contains("aborted by caller"));
        match resp_rx.try_recv().unwrap() {
            ResumeMsg::Abort(r) => assert_eq!(r, "cannot answer"),
            ResumeMsg::Answer(_) => panic!("expected Abort"),
        }
        assert!(!server.continuations.lock().contains_key("cont_t3"));
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
            ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError>
            {
                Ok(tidepool_eval::value::Value::Lit(tidepool_repr::Literal::LitInt(0)).into())
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

#[cfg(test)]
mod ergonomics_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_preamble_ergonomics() {
        let decls = standard_decls();
        let preamble = build_preamble(&decls, false);
        assert!(preamble.contains("ExtendedDefaultRules"));
        assert!(preamble.contains("default (Int, Double, Text)"));
        assert!(preamble.contains("renderJson :: Value -> Text"));
        assert!(preamble.contains("| Reply with a stub id (e.g. stub_0) to fetch that chunk"));
    }

    #[test]
    fn test_normalize_input_string_unwrapping() {
        // #315: stringified bare-string payloads unwrap one level.
        let stringified = serde_json::Value::String("\"line1\\nline2\"".to_string());
        assert_eq!(
            normalize_input(&stringified),
            serde_json::Value::String("line1\nline2".to_string())
        );
        // A plain non-JSON string stays untouched.
        let plain = serde_json::Value::String("not json".to_string());
        assert_eq!(normalize_input(&plain), plain);
        // Numbers-as-strings stay strings.
        let num = serde_json::Value::String("42".to_string());
        assert_eq!(normalize_input(&num), num);
    }

    #[test]
    fn test_normalize_input_unwrapping() {
        // Stringified object (unwrapped)
        let v1 = json!("{\"a\": 1}");
        assert_eq!(normalize_input(&v1), json!({"a": 1}));

        // Stringified array (unwrapped)
        let v2 = json!("[1, 2, 3]");
        assert_eq!(normalize_input(&v2), json!([1, 2, 3]));

        // Plain string "hello" (unchanged)
        let v3 = json!("hello");
        assert_eq!(normalize_input(&v3), v3);

        // Plain string "123" (unchanged — only Object/Array unwrap)
        let v4 = json!("123");
        assert_eq!(normalize_input(&v4), v4);

        // Real object (unchanged)
        let v5 = json!({"a": 1});
        assert_eq!(normalize_input(&v5), v5);
    }
}
