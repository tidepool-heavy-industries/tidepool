//! MCP (Model Context Protocol) server library for Tidepool.
//!
//! Wraps `tidepool-runtime` in an MCP server exposing `run_haskell`,
//! `compile_haskell`, and `eval` tools. Generic over effect handler stacks
//! via `TidepoolMcpServer<H>`.

pub mod validate;

mod eval_prep;
pub use eval_prep::*;

mod effect_decls;
pub use effect_decls::*;

mod preamble;
pub use preamble::*;

mod resources;

mod ask;
pub(crate) use ask::*;

mod server;
pub use server::*;

use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) const EVAL_TIMEOUT_SECS: u64 = 120;

/// Hard ceiling for the per-eval `timeout_secs` knob (seconds). The default is
/// `EVAL_TIMEOUT_SECS`; a caller may raise the window up to this cap for
/// deliberately heavy dev evals. Beyond it a runaway is likelier than an
/// intentional compute, so the request is clamped here.
const MAX_EVAL_TIMEOUT_SECS: u64 = 600;

/// Resolve the effective eval window (seconds) from an optional per-request
/// override: `None` → the server default (`EVAL_TIMEOUT_SECS`); `Some(t)` → `t`
/// clamped to `[1, MAX_EVAL_TIMEOUT_SECS]`.
pub(crate) fn resolve_eval_timeout_secs(requested: Option<u64>) -> u64 {
    if let Some(t) = requested {
        return t.clamp(1, MAX_EVAL_TIMEOUT_SECS);
    }
    // Server default: `TIDEPOOL_EVAL_TIMEOUT_SECS` (set directly or bridged from
    // config.toml) else the built-in `EVAL_TIMEOUT_SECS`.
    std::env::var("TIDEPOOL_EVAL_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|t| t.clamp(1, MAX_EVAL_TIMEOUT_SECS))
        .unwrap_or(EVAL_TIMEOUT_SECS)
}
pub(crate) const MAX_CONCURRENT_EVALS: usize = 4;
pub(crate) const MAX_ORPHANED_EVALS: usize = 10;

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
    /// call into these. Inline data declarations in `helpers` are fully
    /// supported and right for eval-local types; promote types to a
    /// `.tidepool/lib/<Mod>.hs` module (scaffold with `Explore.defMod`)
    /// when they need to be REUSED across evals.
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
    /// Optional eval window in SECONDS before the timeout-yield fires.
    /// Default 120; clamped to [1, 600]. Raise it for deliberately heavy
    /// evals — e.g. a `cargo check`/`cargo build` driven through the `run`
    /// effect — so they aren't cut off mid-compile. The runaway backstop is
    /// unchanged: at the window an eval at an effect boundary parks as a
    /// continuation, and a pure infinite loop is still detached.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
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
    /// For PAUSED continuations (`"paused": true` suspensions) the
    /// response is ignored and may be omitted — resuming just runs
    /// another window.
    #[serde(default)]
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

/// Request parameters for the `help` tool.
///
/// Returns reference content (the same text behind the `tidepool://…` resources)
/// via a plain tool call, so any MCP client can reach it — not just ones that
/// implement `resources/read`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HelpRequest {
    /// The topic to fetch. One of `guide`, `schema`, `edits`, `vocab`,
    /// `patterns`, `effect <Name>` (e.g. `effect Fs`), or `stdlib <Module>`
    /// (e.g. `stdlib Tidepool.Prelude`). Omit (or pass empty) to list topics.
    #[serde(default)]
    pub topic: Option<String>,
}

// ---------------------------------------------------------------------------
// Templating
// ---------------------------------------------------------------------------

/// Write the generated `Tidepool/Effects.hs` + `Tidepool/Orchestrate.hs` into a
/// content-addressed directory and return that directory (an include root).
/// Idempotent: the path is keyed on both module sources, so distinct effect
/// stacks coexist and repeat startups reuse the same dir. Re-callable per eval
/// (see [`write_generated_modules`]) to self-heal if the dir is reaped.
pub fn ensure_effects_module(effects: &[EffectDecl]) -> std::io::Result<PathBuf> {
    write_generated_modules(
        &effects_module_source(effects),
        &orchestrate_module_source(effects),
    )
}

/// Materialize the generated `Tidepool.Effects` AND `Tidepool.Orchestrate`
/// modules from pre-generated source strings, writing each into the SAME
/// content-addressed staging dir if absent, and returning the dir (an include
/// root). The dir hash covers BOTH sources, so a change to either busts the
/// cache dir — co-location means every `ensure_effects_module` caller picks up
/// the orchestrate module for free, no extra include path to thread through.
///
/// Lets the server self-heal each eval without re-deriving the source from
/// decls — the cheap path is two `exists()` stats. Staged under the stable
/// cache root (`tidepool_runtime::paths::effects_dir`), NOT `$TMPDIR`, which
/// macOS reaps out from under a long-running server.
pub(crate) fn write_generated_modules(
    effects_src: &str,
    orchestrate_src: &str,
) -> std::io::Result<PathBuf> {
    // FNV-1a: deterministic across processes (no per-process SipHash seed).
    // DefaultHasher is randomly seeded, causing identical source to hash to
    // different paths in each process → "Could not find module Tidepool.Effects"
    // when a second process picks a different cache dir than the one that wrote it.
    // Hash both sources together so either changing busts the dir.
    let combined = format!("{effects_src}\n--ORCH--\n{orchestrate_src}");
    let hash = fnv1a_hash(combined.as_bytes());
    let root =
        tidepool_runtime::paths::effects_dir().join(format!("tidepool-effects-{:016x}", hash));
    let module_dir = root.join("Tidepool");
    write_module_file(&module_dir, "Effects.hs", effects_src)?;
    write_module_file(&module_dir, "Orchestrate.hs", orchestrate_src)?;
    Ok(root)
}

/// Atomically write `<module_dir>/<name>` with `src` if it does not already
/// exist (write to a temp file then rename, so a concurrent GHC process never
/// sees a partial module — the rename is atomic on POSIX).
///
/// The temp filename is UNIQUE per writer (pid + monotonic counter): two
/// processes/threads racing on the same fresh content-addressed dir must not
/// share one `*.tmp` path, or the slower writer's `rename` source can vanish
/// (the faster writer already renamed it) → spurious `NotFound`. The rename
/// target is the same for all, and POSIX rename-onto-existing is atomic, so a
/// double write just no-ops the loser.
fn write_module_file(module_dir: &Path, name: &str, src: &str) -> std::io::Result<()> {
    let module_path = module_dir.join(name);
    if !module_path.exists() {
        std::fs::create_dir_all(module_dir)?;
        static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let uniq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp_path = module_dir.join(format!("{name}.{}.{uniq}.tmp", std::process::id()));
        std::fs::write(&tmp_path, src)?;
        // rename overwrites an existing destination atomically; a concurrent
        // writer that already created `module_path` is harmless.
        std::fs::rename(&tmp_path, &module_path)?;
    }
    Ok(())
}

/// FNV-1a 64-bit hash — deterministic, no external dependency.
fn fnv1a_hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 14_695_981_039_346_656_037;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(1_099_511_628_211);
    }
    h
}

/// Unwrap double-encoded JSON strings if they contain an object or array.
pub fn normalize_input(v: &serde_json::Value) -> serde_json::Value {
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

// ---------------------------------------------------------------------------
// Error formatting
// ---------------------------------------------------------------------------

pub(crate) fn format_panic_payload(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else {
        "unknown panic".to_string()
    }
}

// ---------------------------------------------------------------------------
// Import blocklist
// ---------------------------------------------------------------------------

/// Blocked module prefixes. Returns the module name if the import should be rejected.
pub(crate) fn rejected_import(import_str: &str) -> Option<&str> {
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
/// `parking_lot::Mutex` (the file-wide choice) — no poisoning, so `.lock()`
/// hands back the guard directly.
#[derive(Clone, Default)]
pub struct CapturedOutput {
    lines: Arc<Mutex<Vec<String>>>,
}

impl CapturedOutput {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a line of output.
    pub fn push(&self, line: String) {
        self.lines.lock().push(line);
    }

    /// Drain all captured lines, returning them and clearing the buffer.
    pub fn drain(&self) -> Vec<String> {
        std::mem::take(&mut *self.lines.lock())
    }

    /// Snapshot current captured lines without clearing the buffer.
    pub fn snapshot(&self) -> Vec<String> {
        self.lines.lock().clone()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::RawContent;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use tidepool_runtime::DispatchEffect;
    use tokio::time::Duration;

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

    /// Effects module + orchestrate module + preamble concatenated: content
    /// assertions that predate the importable-module split check against the
    /// union of all generated sources the eval sees.
    fn generated_sources(effects: &[EffectDecl], user_library: bool) -> String {
        let mut s = effects_module_source(effects);
        s.push_str(&orchestrate_module_source(effects));
        s.push_str(&build_preamble(effects, user_library));
        s
    }

    #[test]
    fn test_preamble_structural_search_updates() {
        let effects = vec![sg_decl(), fs_decl()];
        let preamble = generated_sources(&effects, false);

        // Verify the rHas / rHasChild combinators are both present (the
        // stopBy: end vs direct-child distinction is exercised by the
        // dedicated sg-operators test; here we only assert presence so a
        // body tweak doesn't break this test).
        assert!(preamble.contains("rHas :: Value -> Value"));
        assert!(preamble.contains("rHasChild :: Value -> Value"));

        // Verify hsDef and rsFn recipes exist
        assert!(preamble.contains("hsDef :: Text -> [Text] -> M [Match]"));
        assert!(preamble.contains("rsFn :: Text -> [Text] -> M [Match]"));

        // Verify grepGlob exists in Fs section
        assert!(preamble.contains("grepGlob :: Text -> FilePath -> M [Hit]"));

        // Verify Match record syntax + the Map-typed matchVars accessor
        assert!(preamble.contains("data Match = Match {"));
        assert!(preamble.contains("matchVars :: Match -> Map Text Text"));
        assert!(preamble.contains("var :: Match -> Text -> Text"));
    }

    #[test]
    fn test_preamble_qq_pragmas_always_on() {
        // Root decision: one eval dialect everywhere. See the FIXME at the
        // pragma line in build_preamble for the latency cost this carries
        // (extension-keyed TH provisioning) and the unpoison-fixed-binary
        // requirement it implies.
        for (src, name) in [
            (build_preamble(&[], false), "preamble"),
            (
                build_preamble(&[sg_decl(), fs_decl()], true),
                "preamble+lib",
            ),
        ] {
            let pragma_line = src.lines().next().unwrap();
            assert!(
                pragma_line.contains("QuasiQuotes"),
                "{name}: QuasiQuotes missing from pragma line"
            );
            assert!(
                pragma_line.contains("ViewPatterns"),
                "{name}: ViewPatterns missing from pragma line"
            );
        }
    }

    #[test]
    fn test_template_haskell_qq_import_placement() {
        let pre = build_preamble(&[], false);
        // mirror eval()'s assembly for a QQ-using request
        let code = "pure [fmt|hello {name}|]";
        let mut imports = aeson_imports();
        if uses_qq(code) {
            imports.push_str("Tidepool.QQ (fmt, j, patch, sg, uri)\n");
        }
        let src = template_haskell(&pre, "'[]", code, &imports, "", None, None);
        let qq = src
            .find("import Tidepool.QQ (fmt, j, patch, sg, uri)\n")
            .expect("QQ import missing from rendered module");
        let default_decl = src.find("default (Int").unwrap();
        assert!(qq < default_decl, "QQ import must precede default decl");
    }

    #[test]
    fn test_no_qq_import_without_token() {
        let pre = build_preamble(&[], false);
        let code = "pure [x | x <- xs]";
        let mut imports = aeson_imports();
        if uses_qq(code) {
            imports.push_str("Tidepool.QQ (fmt, j, patch, sg, uri)\n");
        }
        let src = template_haskell(&pre, "'[]", code, &imports, "", None, None);
        assert!(
            !src.contains("Tidepool.QQ"),
            "no-splice eval must not import Tidepool.QQ"
        );
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
        assert!(rejected_import("Tidepool.TextFormat").is_none());
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

    /// Drift guard: the session decl `ModuleEnv` MUST stay a subset of the eval
    /// preamble's pragmas+imports, so a `session_def` helper sees the same
    /// vocabulary an `eval`/`session_eval` expression does. If someone adds an
    /// import to the eval preamble but not `eval_import_lines`, this catches it.
    #[test]
    fn session_decl_env_matches_eval_preamble() {
        let env = session_decl_module_env();
        // Eval preamble with Exec+Http present so it emits the qualified
        // Tidepool.Shell/Git/Cargo imports the decl env also carries (the full
        // stack the repl always runs under).
        let preamble = build_preamble(&[exec_decl(), http_decl()], false);
        // Every decl import line appears verbatim in the eval preamble.
        for imp in &env.imports {
            assert!(
                preamble.contains(&format!("{imp}\n")),
                "session decl import `{imp}` missing from eval preamble — drift"
            );
        }
        // Same pragma block (one dialect everywhere).
        assert!(
            preamble.contains(&env.pragmas),
            "session decl pragmas diverged from eval preamble"
        );
        // The decl env is qualified-imports-only: no unqualified `import Library`
        // (it would clash with decl-defined names — see hide_library_names; the
        // shell modules are safe because they're imported qualified).
        assert!(!env.imports.iter().any(|i| i == "import Library"));
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
        // The slim floor lists each effect name + one-liner …
        assert!(desc.contains("Console: Print to console"));
        // … and points at the resources that carry the depth (per-effect
        // constructors/helpers now live in `tidepool://effect/{name}`, not inline).
        assert!(desc.contains("tidepool://effect/{name}"));
        assert!(!desc.contains("Built-in helpers"));
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

(<?>) :: Q a -> Text -> M a
(Q s p t) <?> prompt = undefined
";
        let sigs = extract_sigs(src);
        assert!(sigs.contains(&"oracle :: Text -> M Text".to_string()));
        assert!(sigs.contains(
            &"steerM :: Monad m => (Int -> Int -> a -> m r) -> b -> [a] -> m b".to_string()
        ));
        assert!(sigs.contains(&"type Vocab s = [(Text, Text -> s -> M s)]".to_string()));
        assert!(sigs.contains(&"data Rose a = Rose a [Rose a]".to_string()));
        assert!(sigs.contains(&"(<?>) :: Q a -> Text -> M a".to_string()));
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
        // Standard Haskell names as primary — assert the SIGNATURE lines,
        // not the `= send . …` bodies (body wording is volatile; the
        // signature is the stable contract eval authors depend on).
        assert!(preamble.contains("putStrLn :: Text -> M ()"));
        assert!(preamble.contains("readFile :: FilePath -> M Text"));
        assert!(preamble.contains("writeFile :: FilePath -> Text -> M ()"));
        assert!(preamble.contains("appendFile :: FilePath -> Text -> M ()"));
        assert!(preamble.contains("listDirectory :: FilePath -> M [FilePath]"));
        assert!(preamble.contains("doesFileExist :: FilePath -> M Bool"));
        assert!(preamble.contains("getFileSize :: FilePath -> M (Maybe Int)"));
        assert!(preamble.contains("fsMeta :: FilePath -> M (Maybe FileMeta)"));
        assert!(preamble.contains("glob :: FilePath -> M [FilePath]"));
        // Core editing verbs (the str-replace common case + dry-run).
        assert!(preamble.contains("update :: FilePath -> Text -> Text -> M ()"));
        assert!(preamble.contains("updateAll :: FilePath -> Text -> Text -> M Int"));
        assert!(preamble.contains("planUpdate :: FilePath -> Text -> Text -> M UpdateOutcome"));
        assert!(preamble.contains("insertAfter :: FilePath -> Text -> Text -> M ()"));
        assert!(preamble.contains("callCommand :: Text -> M ()"));
        assert!(preamble.contains("readProcess :: Text -> M Text"));
        // No old aliases
        assert!(!preamble.contains("fsRead"));
        assert!(!preamble.contains("fsWrite"));
        // `say` is the Console wrapper (re-added 2026-06-22, friction #5).
        assert!(preamble.contains("say :: Text -> M ()"));
        // Other helpers unchanged
        assert!(preamble.contains("kvGet :: Text -> M (Maybe Value)"));
        assert!(preamble.contains("httpGet :: Text -> M Value"));
        assert!(preamble.contains("ask :: Schema -> Text -> M Value"));
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
    fn test_ask_decl() {
        let decl = ask_decl();
        assert_eq!(decl.type_name, "Ask");
        // Bare `Ask` was reaped with the structured-Ask collapse; only AskWith
        // (schema-carrying) remains.
        assert_eq!(decl.constructors.len(), 1);
        assert!(decl.constructors[0].contains("AskWith :: Text -> Value -> Ask Value"));
        // The Schema vocabulary lives on the Ask effect (always present in every
        // stack) so .tidepool/lib modules and Llm-less stacks can build schemas.
        let type_defs = decl.type_defs.join("\n");
        assert!(type_defs.contains("data Schema"));
        assert!(!type_defs.contains("data Q a"));
        let helpers = decl.helpers.join("\n");
        assert!(helpers.contains("ask :: Schema -> Text -> M Value"));
        assert!(helpers.contains("schemaToValue :: Schema -> Value"));
        assert!(!helpers.contains("askQ"));
    }

    #[test]
    fn test_standard_decls_includes_ask() {
        let decls = standard_decls();
        assert_eq!(decls.len(), 9);
        assert_eq!(decls[4].type_name, "Http");
        assert_eq!(decls[5].type_name, "Exec");
        assert_eq!(decls[6].type_name, "Lsp");
        assert_eq!(decls[7].type_name, "Llm");
        assert_eq!(decls[8].type_name, "Ask");
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
        assert!(preamble.contains("  AskWith :: Text -> Value -> Ask Value"));
        assert!(preamble.contains("type M = Eff '[Console, KV, Fs, SG, Http, Exec, Lsp, Llm, Ask]"));
    }

    #[test]
    fn test_ask_in_effect_stack_type() {
        let decls = standard_decls();
        let stack = build_effect_stack_type(&decls);
        assert_eq!(stack, "'[Console, KV, Fs, SG, Http, Exec, Lsp, Llm, Ask]");
    }

    #[test]
    fn test_preamble_hides_run_from_freer() {
        let decls = standard_decls();
        let preamble = generated_sources(&decls, false);
        assert!(preamble.contains("import Control.Monad.Freer hiding (run)"));
        // Our run helper should still be present
        assert!(preamble.contains(
            "run :: Text -> M Proc\nrun cmd = (\\(ec, o, e) -> Proc ec o e) <$> send (Run cmd)"
        ));
    }

    #[test]
    fn test_preamble_text_error_shadow() {
        let decls = standard_decls();
        let preamble = generated_sources(&decls, false);
        // Prelude error (String-based) is hidden
        assert!(preamble.contains("import Tidepool.Prelude hiding (error)"));
        // Text-taking error is defined via qualified Prelude
        assert!(preamble.contains("import qualified Prelude as P"));
        // Assert the Text-taking `error` SIGNATURE (the shadow contract);
        // the `= P.error . T.unpack` body is an implementation detail.
        assert!(preamble.contains("error :: Text -> a"));
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
        // The orchestration helpers moved OUT of the expr-module preamble into
        // the generated Tidepool.Orchestrate module (the namespace-poison fix);
        // assert their signatures there instead.
        let orch = orchestrate_module_source(&decls);
        // runChecked is now an alias for readProcess (assert the signature;
        // the alias body is volatile).
        assert!(orch.contains("runChecked :: Text -> M Text"));
        // File manipulation helpers
        assert!(orch.contains("mapFile :: Text -> (Text -> Text) -> M ()"));
        assert!(orch.contains("mapFileM :: Text -> (Text -> M Text) -> M ()"));
        assert!(orch.contains("searchFiles :: Text -> Text -> M [Hit]"));
        assert!(orch.contains("lineCount :: Text -> M Int"));
        assert!(orch.contains("fileContains :: Text -> Text -> M Bool"));
        // KV batch helpers
        assert!(orch.contains("kvAll :: M [(Text, Value)]"));
        assert!(orch.contains("kvClear :: M ()"));
        assert!(orch.contains("runAll :: [Text] -> M [Proc]"));
        // The expr-module preamble no longer splices these bodies — it imports
        // the module and only emits the paginateResult alias.
        let preamble = build_preamble(&decls, true);
        assert!(preamble.contains("import Tidepool.Orchestrate"));
        assert!(!preamble.contains("runChecked :: Text -> M Text"));
        assert!(!preamble.contains("searchFiles :: Text -> Text -> M [(Text, Int, Text)]"));
        // The structured Ask/Llm surface lives in the generated Tidepool.Effects
        // module — one Schema vocabulary, extract with optics. The Q-builder DSL
        // and the `??`/`?!`/triage/survey/sift sugar are removed.
        let effects_mod = effects_module_source(&decls);
        assert!(effects_mod.contains("data Schema = SObj"));
        assert!(effects_mod.contains("ask :: Schema -> Text -> M Value"));
        assert!(effects_mod.contains("llm :: Schema -> Text -> M Value"));
        assert!(effects_mod.contains("tryLlm :: Schema -> Text -> M (Either Text Value)"));
        // ask suspends to the caller via AskWith (no autonomous LLM call)
        assert!(effects_mod
            .contains("send (AskWith prompt (object [\"schema\" .= schemaToValue schema]))"));
        // The removed Q layer + sugar are gone.
        assert!(!effects_mod.contains("data Q a"));
        assert!(!effects_mod.contains("askQ ::"));
        assert!(!effects_mod.contains("llmQ ::"));
        assert!(!effects_mod.contains("llmJson ::"));
        assert!(!effects_mod.contains("pick :: [Text] -> Q Text"));
        assert!(!effects_mod.contains("(??)"));
        assert!(!effects_mod.contains("(?!)"));
        assert!(!effects_mod.contains("triage ::"));
        assert!(!effects_mod.contains("survey ::"));
        assert!(!effects_mod.contains("sift ::"));
        // and NOT duplicated in the preamble (one definition site)
        assert!(!preamble.contains("data Schema = SObj"));
        // ask lives in ask_decl (always present), so it survives an Llm-less stack
        let no_llm: Vec<EffectDecl> = standard_decls()
            .into_iter()
            .filter(|d| d.type_name != "Llm")
            .collect();
        let no_llm_mod = effects_module_source(&no_llm);
        assert!(no_llm_mod.contains("ask :: Schema -> Text -> M Value"));
        // llm needs the Llm effect — absent from an Llm-less stack.
        assert!(!no_llm_mod.contains("llm :: Schema -> Text -> M Value"));
    }

    #[test]
    fn test_orchestration_is_pure_fn_of_effects() {
        // The orchestration helpers no longer depend on the user_library flag —
        // Tidepool.Orchestrate is a PURE function of the effect set (so it can be
        // co-located + hashed with Tidepool.Effects). The bodies never appear in
        // the expr-module preamble (imported, not spliced), regardless of library.
        let decls = standard_decls();
        assert!(!build_preamble(&decls, false).contains("runChecked"));
        assert!(!build_preamble(&decls, true).contains("runChecked"));
        // The module carries them based on effects alone (Exec present here).
        let orch = orchestrate_module_source(&decls);
        assert!(orch.contains("runChecked :: Text -> M Text"));
        // An Exec-less stack omits the Exec-gated helpers.
        let no_exec: Vec<EffectDecl> = standard_decls()
            .into_iter()
            .filter(|d| d.type_name != "Exec")
            .collect();
        assert!(!orchestrate_module_source(&no_exec).contains("runChecked"));
    }

    #[test]
    fn test_preamble_sg_rule_operators() {
        let decls = standard_decls();
        let preamble = generated_sources(&decls, false);
        // Object merge operator (fixity + signature; the KM.unionWith body
        // is an implementation detail).
        assert!(preamble.contains("infixr 6 .+."));
        assert!(preamble.contains("(.+.) :: Value -> Value -> Value"));
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
        assert!(preamble.contains("import qualified Tidepool.Data.Text as T"));
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
    fn test_resolve_eval_timeout_secs() {
        // None → server default.
        assert_eq!(resolve_eval_timeout_secs(None), EVAL_TIMEOUT_SECS);
        // In-range values pass through.
        assert_eq!(resolve_eval_timeout_secs(Some(1)), 1);
        assert_eq!(resolve_eval_timeout_secs(Some(300)), 300);
        assert_eq!(
            resolve_eval_timeout_secs(Some(MAX_EVAL_TIMEOUT_SECS)),
            MAX_EVAL_TIMEOUT_SECS
        );
        // Below floor clamps up to 1 (never 0 — a 0s window would insta-yield).
        assert_eq!(resolve_eval_timeout_secs(Some(0)), 1);
        // Above ceiling clamps down to the cap.
        assert_eq!(
            resolve_eval_timeout_secs(Some(100_000)),
            MAX_EVAL_TIMEOUT_SECS
        );
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
            .handle_session_result(
                "eval",
                rx,
                source,
                resp_tx,
                captured,
                None,
                PauseGate::new(),
            )
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
            .handle_session_result(
                "eval",
                rx,
                source,
                resp_tx,
                captured,
                None,
                PauseGate::new(),
            )
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
            .handle_session_result(
                "eval",
                rx,
                source,
                resp_tx,
                captured,
                None,
                PauseGate::new(),
            )
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
        assert!(matches!(
            conts[cont_id].kind,
            SessionKind::AwaitingAnswer {
                expected_schema: Some(_)
            }
        ));
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
                kind: SessionKind::AwaitingAnswer {
                    expected_schema: Some(serde_json::json!({
                        "type": "object",
                        "properties": {"pick": {"type": "string", "enum": ["bug", "refactor"]}},
                        "required": ["pick"],
                    })),
                },
                thread: None,
                gate: PauseGate::new(),
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
                kind: SessionKind::AwaitingAnswer {
                    expected_schema: Some(serde_json::json!({
                        "type": "object",
                        "properties": {"answer": {"type": "boolean"}},
                        "required": ["answer"],
                    })),
                },
                thread: None,
                gate: PauseGate::new(),
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
                kind: SessionKind::AwaitingAnswer {
                    expected_schema: None,
                },
                thread: None,
                gate: PauseGate::new(),
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
        // The eval printed before failing — that output must survive.
        captured.push("printed before failure".into());

        tx.send(SessionMessage::Error {
            error: "oops".into(),
        })
        .unwrap();

        let res = server
            .handle_session_result(
                "eval",
                rx,
                source,
                resp_tx,
                captured,
                None,
                PauseGate::new(),
            )
            .await
            .unwrap();
        assert_eq!(res.is_error, Some(true));
        let text = match &res.content[0].raw {
            RawContent::Text(t) => &t.text,
            _ => panic!("Expected text content"),
        };
        assert!(text.contains("## Error"));
        assert!(text.contains("oops"));
        // A plain error message is a clean Haskell error.
        assert!(text.contains("**failure-class:** `haskell-error`"));
        // Partial output is surfaced on the failure path.
        assert!(text.contains("printed before failure"));
    }

    /// A `SessionMessage::Error` carrying a stack-overflow yield must tag
    /// `runtime-yield`, not `haskell-error`.
    #[tokio::test]
    async fn test_handle_session_result_runtime_yield() {
        let server = create_mock_server();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (resp_tx, _resp_rx) = std::sync::mpsc::channel();
        let source: Arc<str> = "test source".into();
        let captured = CapturedOutput::new();
        captured.push("loop iter 1".into());

        tx.send(SessionMessage::Error {
            error: "stack overflow (likely infinite list or unbounded recursion)".into(),
        })
        .unwrap();

        let res = server
            .handle_session_result(
                "eval",
                rx,
                source,
                resp_tx,
                captured,
                None,
                PauseGate::new(),
            )
            .await
            .unwrap();
        assert_eq!(res.is_error, Some(true));
        let text = match &res.content[0].raw {
            RawContent::Text(t) => &t.text,
            _ => panic!("Expected text content"),
        };
        assert!(text.contains("**failure-class:** `runtime-yield`"));
        assert!(text.contains("loop iter 1"));
    }

    /// A caught JIT signal arrives on the in-band error channel; it must still
    /// tag `signal-crash` (compiler bug), not `haskell-error`.
    #[tokio::test]
    async fn test_handle_session_result_caught_signal() {
        let server = create_mock_server();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (resp_tx, _resp_rx) = std::sync::mpsc::channel();
        let source: Arc<str> = "test source".into();
        let captured = CapturedOutput::new();

        tx.send(SessionMessage::Error {
            error: "JIT signal: SIGILL (illegal instruction — likely exhausted case branch)".into(),
        })
        .unwrap();

        let res = server
            .handle_session_result(
                "eval",
                rx,
                source,
                resp_tx,
                captured,
                None,
                PauseGate::new(),
            )
            .await
            .unwrap();
        let text = match &res.content[0].raw {
            RawContent::Text(t) => &t.text,
            _ => panic!("Expected text content"),
        };
        assert!(text.contains("**failure-class:** `signal-crash`"));
    }

    #[tokio::test]
    async fn test_handle_session_result_crash() {
        let server = create_mock_server();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (resp_tx, _resp_rx) = std::sync::mpsc::channel();
        let source: Arc<str> = "test source".into();
        let captured = CapturedOutput::new();
        // Output printed before the thread died must still be surfaced.
        captured.push("printed before crash".into());

        // Close the channel without sending anything
        drop(tx);

        let res = server
            .handle_session_result(
                "eval",
                rx,
                source,
                resp_tx,
                captured,
                None,
                PauseGate::new(),
            )
            .await
            .unwrap();
        assert_eq!(res.is_error, Some(true));
        let text = match &res.content[0].raw {
            RawContent::Text(t) => &t.text,
            _ => panic!("Expected text content"),
        };
        assert!(text.contains("## Crash"));
        assert!(text.contains("eval thread crashed"));
        // A dead eval thread is a signal-crash (compiler bug), and its last
        // words must survive.
        assert!(text.contains("**failure-class:** `signal-crash`"));
        assert!(text.contains("printed before crash"));
    }

    #[tokio::test]
    async fn test_handle_session_result_timeout() {
        tokio::time::pause();

        let server = create_mock_server();
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (resp_tx, _resp_rx) = std::sync::mpsc::channel();
        let source: Arc<str> = "test source".into();
        let captured = CapturedOutput::new();
        captured.push("printed before timeout".into());

        let handle = tokio::spawn(async move {
            server
                .handle_session_result(
                    "eval",
                    rx,
                    source,
                    resp_tx,
                    captured,
                    None,
                    PauseGate::new(),
                )
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
        assert!(text.contains("**failure-class:** `timeout`"));
        // Output before a pure-compute timeout is surfaced.
        assert!(text.contains("printed before timeout"));
    }

    /// The gate state machine: pause parks a checkpointing thread, resume
    /// releases it, abort errors it out; in_effect threads are not
    /// runaways.
    #[test]
    fn test_pause_gate_park_resume_abort() {
        // pause → thread parks at checkpoint → resume releases it
        let gate = PauseGate::new();
        gate.request_pause();
        let g2 = Arc::clone(&gate);
        let t = std::thread::spawn(move || g2.checkpoint());
        assert!(gate.parked_or_in_effect(Duration::from_secs(2)));
        gate.resume_run();
        assert!(t.join().unwrap().is_ok());
        gate.exit_effect();

        // pause → park → abort errors the checkpoint
        gate.request_pause();
        let g3 = Arc::clone(&gate);
        let t = std::thread::spawn(move || g3.checkpoint());
        assert!(gate.parked_or_in_effect(Duration::from_secs(2)));
        gate.request_abort("killed".into());
        let err = t.join().unwrap().unwrap_err();
        assert!(err.contains("killed"));

        // a running gate with no checkpointing thread = runaway
        let lone = PauseGate::new();
        lone.request_pause();
        assert!(!lone.parked_or_in_effect(Duration::from_millis(50)));

        // ...unless the thread is inside an effect (e.g. a long LLM
        // call): it will park at the NEXT boundary — not a runaway.
        let busy = PauseGate::new();
        busy.checkpoint().unwrap(); // enter effect (in_effect = true)
        busy.request_pause();
        assert!(busy.parked_or_in_effect(Duration::from_millis(50)));
    }

    /// Timeout with a thread parked at the gate → paused continuation
    /// (not an error), and resume wakes it and collects the result.
    #[tokio::test]
    async fn test_timeout_parks_paused_continuation_and_resume_collects() {
        let server = create_mock_server();
        let (sess_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (resp_tx, _resp_rx) = std::sync::mpsc::channel();
        let source: Arc<str> = "test source".into();
        let captured = CapturedOutput::new();
        captured.push("step 1 done".into());

        // A real "eval thread": parks at its checkpoint (pause is already
        // requested), and once resumed reports completion.
        let gate = PauseGate::new();
        gate.request_pause();
        let g2 = Arc::clone(&gate);
        let thread_tx = sess_tx.clone();
        let t = std::thread::spawn(move || {
            g2.checkpoint().unwrap(); // parks here until resume
            g2.exit_effect();
            let _ = thread_tx.send(SessionMessage::Completed {
                result: "\"finished\"".into(),
            });
        });

        // Wait for the park, then drive the timeout branch.
        assert!(gate.parked_or_in_effect(Duration::from_secs(2)));
        tokio::time::pause();
        let server2 = server.clone();
        let h = tokio::spawn(async move {
            server2
                .handle_session_result("eval", rx, source, resp_tx, captured, None, gate)
                .await
        });
        tokio::time::advance(Duration::from_secs(EVAL_TIMEOUT_SECS + 1)).await;
        let res = h.await.unwrap().unwrap();
        tokio::time::resume();

        assert_eq!(res.is_error, Some(false));
        let text = match &res.content[0].raw {
            RawContent::Text(t) => &t.text,
            _ => panic!("Expected text content"),
        };
        let json: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(json["paused"], true);
        assert_eq!(json["output"][0], "step 1 done");
        let cont_id = json["continuation_id"].as_str().unwrap().to_string();
        assert!(matches!(
            server.continuations.lock()[&cont_id].kind,
            SessionKind::Paused
        ));

        // resume: wakes the gate; the thread completes and we collect.
        let res = server
            .resume(ResumeRequest {
                continuation_id: cont_id,
                response: serde_json::Value::Null,
            })
            .await
            .unwrap();
        assert_eq!(res.is_error, Some(false));
        let text = match &res.content[0].raw {
            RawContent::Text(t) => &t.text,
            _ => panic!("Expected text content"),
        };
        assert!(text.contains("finished"));
        t.join().unwrap();
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
            timeout_secs: None,
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
            effect_decls: Vec::new(),
            lib_dirs: Vec::new(),
            patterns_path: None,
            stdlib_dir: None,
            help_tool: false,
            continuations: Arc::new(Mutex::new(HashMap::new())),
            next_cont_id: Arc::new(AtomicU64::new(1)),
            eval_semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_EVALS)),
            orphaned_threads: Arc::new(AtomicUsize::new(0)),
            effects_source: String::new(),
            orchestrate_source: String::new(),
        }
    }

    /// Snapshot test: FNV-1a of a fixed string must produce the same hash
    /// in every process. If DefaultHasher (randomly seeded) is accidentally
    /// reintroduced, this assertion fails because the computed hash won't
    /// match the stable FNV-1a value baked into the expected dir name.
    #[test]
    fn test_effects_hash_is_deterministic_across_calls() {
        let eff = "module Tidepool.Effects where\n-- sentinel\n";
        let orch = "module Tidepool.Orchestrate where\n-- sentinel\n";
        let dir1 = write_generated_modules(eff, orch).unwrap();
        let dir2 = write_generated_modules(eff, orch).unwrap();
        assert_eq!(
            dir1, dir2,
            "same source must yield same content-addressed dir"
        );
        // Verify the dir name encodes the known FNV-1a hash of BOTH sources
        // combined (changing either busts the dir).
        let combined = format!("{eff}\n--ORCH--\n{orch}");
        let expected_hash = fnv1a_hash(combined.as_bytes());
        let expected_suffix = format!("tidepool-effects-{:016x}", expected_hash);
        let dir_name = dir1.file_name().unwrap().to_str().unwrap();
        assert_eq!(
            dir_name, expected_suffix,
            "dir name must be the stable FNV-1a hash of both sources; got {dir_name}"
        );
        // A change to the orchestrate source alone busts the dir.
        let dir3 =
            write_generated_modules(eff, "module Tidepool.Orchestrate where\n-- other\n").unwrap();
        assert_ne!(dir1, dir3, "orchestrate change must bust the dir");
        let _ = std::fs::remove_dir_all(&dir1);
        let _ = std::fs::remove_dir_all(&dir3);
    }

    #[test]
    fn test_effects_module_self_heals_after_reap() {
        // Unique source → unique content-addressed dir, so this can't collide
        // with a real effect stack or a parallel test. Cleans up after.
        let eff = format!(
            "module Tidepool.Effects where\n-- probe {}\n",
            std::process::id()
        );
        let orch = format!(
            "module Tidepool.Orchestrate where\n-- probe {}\n",
            std::process::id()
        );
        let dir = write_generated_modules(&eff, &orch).unwrap();
        let module = dir.join("Tidepool").join("Effects.hs");
        let orch_module = dir.join("Tidepool").join("Orchestrate.hs");
        assert!(module.exists(), "effects module written on first call");
        assert!(
            orch_module.exists(),
            "orchestrate module written on first call"
        );
        // Staged off $TMPDIR — the macOS-reaped location we moved away from
        // (unless neither XDG_CACHE_HOME nor HOME is set, the last-resort case).
        if std::env::var_os("HOME").is_some() || std::env::var_os("XDG_CACHE_HOME").is_some() {
            assert!(
                !dir.starts_with(std::env::temp_dir()),
                "effects module should not stage under $TMPDIR"
            );
        }
        // Simulate the OS reaping the staging dir mid-session.
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(!module.exists());
        // The per-eval self-heal recreates both at the same content-addressed path.
        let dir2 = write_generated_modules(&eff, &orch).unwrap();
        assert_eq!(dir, dir2, "content-addressed path is stable across calls");
        assert!(module.exists(), "effects self-healed after reap");
        assert!(orch_module.exists(), "orchestrate self-healed after reap");
        assert_eq!(std::fs::read_to_string(&module).unwrap(), eff);
        assert_eq!(std::fs::read_to_string(&orch_module).unwrap(), orch);
        let _ = std::fs::remove_dir_all(&dir);
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
        // renderJson + the interactive-pagination prompt moved into the generated
        // Tidepool.Orchestrate module (imported by the preamble, not spliced).
        let orch = orchestrate_module_source(&decls);
        assert!(orch.contains("renderJson :: Value -> Text"));
        assert!(orch.contains("| Reply with a stub id (e.g. stub_0) to fetch that chunk"));
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
