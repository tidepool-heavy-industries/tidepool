//! MCP (Model Context Protocol) server library for Tidepool.
//!
//! Wraps `tidepool-runtime` in an MCP server exposing `run_haskell`,
//! `compile_haskell`, and `eval` tools. Generic over effect handler stacks
//! via `TidepoolMcpServer<H>`.

#![warn(clippy::unwrap_used, clippy::expect_used)]
pub mod validate;

mod eval_prep;
pub use eval_prep::*;
// The single failure taxonomy lives in tidepool-runtime; re-export it from the
// server facade so callers keep reaching it as `tidepool_mcp::FailureClass`.
pub use tidepool_runtime::{classify, FailureClass, FailureEnvelope, Phase};

mod effect_decls;
pub use effect_decls::*;

mod effect_defs;

mod fs_stable;
pub use fs_stable::*;

// Effect declarations generated from the `tidepool-protocol` schema. An
// effect appears here once its whole vertical has migrated; the rest are
// still expanded from `effect_defs`'s macros (PRD 22: effect at a time).
mod generated;
pub use generated::*;

mod preamble;
pub use preamble::*;

mod describe;
pub use describe::*;

mod lib_isolate;
pub use lib_isolate::*;

pub mod resources;

mod server;
pub use server::*;

pub mod server_common;

use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) const EVAL_TIMEOUT_SECS: u64 = 600;

/// Hard ceiling for the per-eval `timeout_secs` knob (seconds). The default is
/// `EVAL_TIMEOUT_SECS`; a caller may raise the window up to this cap for
/// deliberately heavy dev evals. Beyond it a runaway is likelier than an
/// intentional compute, so the request is clamped here.
const MAX_EVAL_TIMEOUT_SECS: u64 = 1800;

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
    /// {{TIMEOUT_SECS_DOC}} Raise it for deliberately heavy
    /// evals — e.g. a `cargo check`/`cargo build` driven through the `run`
    /// effect — so they aren't cut off mid-compile. The runaway backstop is
    /// unchanged: at the window an eval at an effect boundary parks as a
    /// continuation, and a pure infinite loop is still detached.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Sentinel spliced into the generated `EvalRequest` JSON schema's
/// `timeout_secs` description (see the doc comment above) and replaced by
/// [`eval_request_input_schema`] with the live `EVAL_TIMEOUT_SECS`/
/// `MAX_EVAL_TIMEOUT_SECS` constants — a doc comment is a compile-time
/// literal, so this is the one place the numbers can be interpolated instead
/// of hand-copied (the drift that made the doc claim "600" while the real
/// cap is 1800).
const TIMEOUT_SECS_DOC_SENTINEL: &str = "{{TIMEOUT_SECS_DOC}}";

/// The `eval` tool's JSON input schema, with the `timeout_secs` sentinel
/// resolved to the actual timeout constants (formatted exactly once, here).
pub fn eval_request_input_schema() -> Result<Arc<serde_json::Map<String, serde_json::Value>>, String>
{
    let schema = schemars::schema_for!(EvalRequest);
    let json = serde_json::to_string(&schema)
        .map_err(|e| format!("failed to serialize EvalRequest schema: {e}"))?;
    let doc = format!("Default {EVAL_TIMEOUT_SECS}; clamped to [1, {MAX_EVAL_TIMEOUT_SECS}].");
    let json = json.replace(TIMEOUT_SECS_DOC_SENTINEL, &doc);
    match serde_json::from_str(&json)
        .map_err(|e| format!("failed to reparse EvalRequest schema: {e}"))?
    {
        serde_json::Value::Object(o) => Ok(Arc::new(o)),
        _ => Ok(Arc::new(serde_json::Map::new())),
    }
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

/// The two include roots a compile needs to see the whole effect surface —
/// returned together because they are cache-addressed SEPARATELY and both
/// must be on the include path (GHC resolves `Tidepool.Effects`'s `import
/// Tidepool.Effects.Core` against the `core` root).
///
/// **`core` is the stable half**: content-addressed on the effect VOCABULARY
/// alone, so every window/turn that shares a vocabulary (the general Agent
/// stack, the self-iterating harness's answerer stack, …) resolves to the
/// SAME dir — no recompiling every effect GADT + helper per window, and no
/// tycon churn for a value that mentions one (see
/// `haskell/src/Tidepool/Translate.hs`'s narrowed `typeMentionsEffectMonad`).
///
/// **`shim` is the per-window half**: content-addressed on the ROW (which
/// effects are actually in `type M`) and any [`RowArgs`] type application —
/// small, and it is the only dir that changes when e.g. a `Finalize` hole's
/// answer type changes between windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectsModuleDirs {
    /// Include root holding `Tidepool/Effects/Core.hs` — vocabulary-keyed.
    pub core: PathBuf,
    /// Include root holding `Tidepool/Effects.hs` (the shim) and
    /// `Tidepool/Orchestrate.hs` — row-keyed.
    pub shim: PathBuf,
}

impl EffectsModuleDirs {
    /// Both roots, in the order a GHC include-path search would want them
    /// (core first: the shim's `import Tidepool.Effects.Core` resolves
    /// against it, though GHC's own search order does not actually require
    /// this — listed core-first purely for readability at call sites).
    #[must_use]
    pub fn include_paths(&self) -> [PathBuf; 2] {
        [self.core.clone(), self.shim.clone()]
    }
}

/// Write the generated `Tidepool/Effects/Core.hs` (vocabulary-keyed) and
/// `Tidepool/Effects.hs` + `Tidepool/Orchestrate.hs` (row-keyed) into their two
/// content-addressed directories and return both (see [`EffectsModuleDirs`]).
/// Idempotent: each path is keyed on its own module source(s), so distinct
/// vocabularies/rows coexist and repeat startups reuse the same dirs.
/// Re-callable per eval (see [`write_core_module`]/[`write_shim_module`]) to
/// self-heal if a dir is reaped.
pub fn ensure_effects_module(effects: &[EffectDecl]) -> std::io::Result<EffectsModuleDirs> {
    ensure_effects_module_at(effects, &RowArgs::default())
}

/// [`ensure_effects_module`] with the row's parameterized effects applied to
/// explicit type arguments (the harness's per-hole `Finalize <answer type>`).
/// The shim dir stays content-addressed on ITS generated source (which
/// includes the applied types + their imports) — so two answer types get two
/// shim dirs, and neither can be served the other's module — while the core
/// dir stays whatever this same `effects` vocabulary always resolves to.
///
/// Both dirs hold SOURCE ONLY (no `.hi`/`.o`), and a harness turn compile is
/// uncached, so an edit to an author module named in `row` is picked up on the
/// next compile: there is no compiled artifact for its hash to have to cover.
pub fn ensure_effects_module_at(
    effects: &[EffectDecl],
    row: &RowArgs,
) -> std::io::Result<EffectsModuleDirs> {
    ensure_effects_module_with_vocab(effects, effects, row)
}

/// [`ensure_effects_module_at`] with the effect VOCABULARY (what's nameable —
/// gets a GADT + `type_defs` emitted into the stable Core module) split from
/// the effect ROW (`row_effects` — what's IN `type M`, i.e. actually
/// executable via `Member`). This is the mechanism behind "effect vocabulary
/// available in scope ≠ effects present in M's row" (extract-wave item 0b,
/// generalized by stable-effects-core to every vocabulary effect, not just
/// `RunLLMTurn`): a name can be IN SCOPE (compiles, resolves, has a real GADT
/// constructor) without being IN THE ROW (a `Member` constraint at its call
/// site is then unsolved — a comprehensible type error, not "not in scope").
///
/// **`vocab_effects` must be a SUPERSET of `row_effects`** (matched by
/// `type_name`) — a loud panic, not a silently-widened row, if it isn't: a
/// row effect with no vocabulary entry would have no GADT in Core to compile
/// `type M` against at all.
///
/// **A [`ROW_DEPENDENT_EFFECTS`] vocab effect (e.g. `Green`) must ALSO be in
/// `row_effects`** — a loud panic, not a silent drop, if it isn't:
/// [`effects_core_module_source`] excludes such an effect from Core entirely
/// (it cannot typecheck there, having no `M`), and only
/// [`effects_shim_module_source`] picks it up again, keyed on `row_effects`
/// alone — a vocab-only row-dependent effect would simply vanish from the
/// generated surface with no error at all.
pub fn ensure_effects_module_with_vocab(
    row_effects: &[EffectDecl],
    vocab_effects: &[EffectDecl],
    row: &RowArgs,
) -> std::io::Result<EffectsModuleDirs> {
    for v in vocab_effects {
        assert!(
            !is_row_dependent_effect(v.type_name)
                || row_effects.iter().any(|r| r.type_name == v.type_name),
            "`{}` is row-dependent (ROW_DEPENDENT_EFFECTS) but is only in the \
             vocabulary, not the row — it would be silently excluded from both \
             Core and the shim; add it to `row_effects` too",
            v.type_name
        );
    }
    for r in row_effects {
        assert!(
            vocab_effects.iter().any(|v| v.type_name == r.type_name),
            "effect vocabulary must be a superset of the row: `{}` is in the \
             row but not in the vocabulary",
            r.type_name
        );
    }
    let core = ensure_effects_core_module(vocab_effects)?;
    let shim = ensure_effects_shim_module(row_effects, row)?;
    Ok(EffectsModuleDirs { core, shim })
}

/// Write the stable `Tidepool/Effects/Core.hs` module (a pure function of
/// `vocab_effects` alone) into its own content-addressed dir and return it.
/// Separate from [`ensure_effects_module_with_vocab`]'s shim/orchestrate
/// write so the decl plane (which needs Core on its include path but NEVER
/// the per-window shim — see `tidepool-mcp/CLAUDE.md`) can materialize it
/// without also minting a throwaway row-keyed dir.
pub fn ensure_effects_core_module(vocab_effects: &[EffectDecl]) -> std::io::Result<PathBuf> {
    write_core_module(&effects_core_module_source(vocab_effects))
}

/// Like [`ensure_effects_core_module`] but takes the already-rendered source
/// text directly — the self-heal path (a server holding onto its own
/// generated sources to re-materialize them if the staging dir is reaped)
/// calls this instead of re-deriving the text from `vocab_effects` each time.
pub fn write_core_module(core_src: &str) -> std::io::Result<PathBuf> {
    write_module_dir("tidepool-effects-core", &[("Effects/Core.hs", core_src)])
}

/// Write just the per-window shim (`Tidepool/Effects.hs` +
/// `Tidepool/Orchestrate.hs`) into its own content-addressed dir and return
/// it, WITHOUT touching Core. For a caller that already holds a stable Core
/// dir (e.g. re-pinning a `Finalize <T>` row every round of the same
/// answerer hole) and only needs to re-materialize the small per-row half —
/// [`ensure_effects_module_with_vocab`] would also recompute (a cheap,
/// content-addressed cache hit, but still a hash + lock) Core's dir every
/// call, which this skips entirely.
pub fn ensure_effects_shim_module(
    row_effects: &[EffectDecl],
    row: &RowArgs,
) -> std::io::Result<PathBuf> {
    write_shim_module(
        &effects_shim_module_source(row_effects, row),
        &orchestrate_module_source(row_effects),
    )
}

/// Like [`ensure_effects_shim_module`] but takes the already-rendered source
/// texts directly — the self-heal path calls this instead of re-deriving them
/// from `row_effects`/`row` each time.
pub fn write_shim_module(shim_src: &str, orchestrate_src: &str) -> std::io::Result<PathBuf> {
    write_module_dir(
        "tidepool-effects",
        &[
            ("Effects.hs", shim_src),
            ("Orchestrate.hs", orchestrate_src),
        ],
    )
}

/// Process-level write-through cache for the content-addressed generated-module
/// directories, keyed on `(dir prefix, content hash)` — the prefix keeps the
/// core dir's cache entries from colliding with the shim dir's (or any future
/// caller's) even on a coincidental hash match across independent content.
///
/// Serializes concurrent writes within one process: the first call for a given
/// key acquires the lock and writes every file; concurrent callers block until
/// ALL of them are on disk, then get the cached path. This closes the TOCTOU
/// window where a caller could see one generated file but not a sibling and
/// compute a fingerprint / call GHC against an incomplete staging dir.
/// Inter-process safety (multiple `cargo test` binaries) is handled by the
/// atomic-rename primitive inside [`write_module_file`].
fn generated_module_write_cache(
) -> &'static Mutex<std::collections::HashMap<(&'static str, String), PathBuf>> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Mutex<std::collections::HashMap<(&'static str, String), PathBuf>>> =
        OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// Materialize a set of `(relative path under Tidepool/, source)` pairs into a
/// SINGLE content-addressed staging dir if absent, and return the dir (an
/// include root whose `Tidepool/` subtree holds every file). The dir hash
/// covers every source TOGETHER, so a change to any one busts the whole dir —
/// co-location means a caller that needs several files generated from the same
/// inputs (e.g. the shim + orchestrate module, both keyed on the same row)
/// gets them for free, no extra include path to thread through.
///
/// `dir_prefix` names the staging-dir family (`"tidepool-effects-core"`,
/// `"tidepool-effects"`) — distinct callers get distinct dirs even if their
/// content hashes happened to collide, and it is what a human sees first when
/// listing the cache dir.
///
/// Concurrent calls within the same process are serialized: only one caller
/// writes the files at a time; others wait and reuse the cached result. This
/// prevents parallel tests from racing on a partially-written staging dir.
/// Inter-process races (parallel test binaries) are handled by the
/// atomic-rename primitive inside [`write_module_file`].
///
/// Self-heals if the staging dir is externally removed
/// (`rm -rf ~/.cache/tidepool`): the cache entry is evicted and the files are
/// re-materialized on the next call.
pub(crate) fn write_module_dir(
    dir_prefix: &'static str,
    files: &[(&str, &str)],
) -> std::io::Result<PathBuf> {
    // blake3, content-addressed and deterministic across processes (no
    // per-process SipHash seed like DefaultHasher, which would hash identical
    // source to different paths in each process → "Could not find module
    // Tidepool.Effects" when a second process picks a different cache dir
    // than the one that wrote it). Each source is its own length-framed
    // field (via `content_hash_hex`), so hashing them separately can never
    // collide with hashing their concatenation.
    let field_bytes: Vec<&[u8]> = files.iter().map(|(_, src)| src.as_bytes()).collect();
    let hash = content_hash_hex(&field_bytes);
    let root = tidepool_runtime::paths::effects_dir().join(format!("{dir_prefix}-{hash}"));
    let module_dir = root.join("Tidepool");

    // Acquire the process-level serialization lock. Concurrent callers
    // (parallel test threads, concurrent eval requests) block here; the first
    // to proceed writes every file and stores the result; the rest take the
    // fast path below.
    let mut cache = generated_module_write_cache().lock();
    let key = (dir_prefix, hash.clone());

    // Fast path: previously written this key AND files still present.
    if cache.contains_key(&key) {
        if files.iter().all(|(rel, _)| module_dir.join(rel).exists()) {
            return Ok(root);
        }
        // Files were externally removed. Evict the stale entry and fall
        // through to re-materialize while still holding the lock.
        cache.remove(&key);
    }

    // Slow path: write every file (still under the lock so concurrent callers
    // wait rather than racing into the same write sequence).
    for (rel, src) in files {
        write_module_file(&module_dir, rel, src)?;
    }
    cache.insert(key, root.clone());
    Ok(root)
}

/// Atomically write `<module_dir>/<rel>` with `src` if it does not already
/// exist (write to a temp file then rename, so a concurrent GHC process never
/// sees a partial module — the rename is atomic on POSIX). `rel` may itself
/// contain a directory separator (e.g. `"Effects/Core.hs"`), whose parent is
/// created alongside `module_dir`.
///
/// The temp filename is UNIQUE per writer (pid + monotonic counter): two
/// processes/threads racing on the same fresh content-addressed dir must not
/// share one `*.tmp` path, or the slower writer's `rename` source can vanish
/// (the faster writer already renamed it) → spurious `NotFound`. The rename
/// target is the same for all, and POSIX rename-onto-existing is atomic, so a
/// double write just no-ops the loser.
pub(crate) fn write_module_file(module_dir: &Path, rel: &str, src: &str) -> std::io::Result<()> {
    let module_path = module_dir.join(rel);
    if !module_path.exists() {
        let Some(parent) = module_path.parent() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("generated module path `{rel}` has no parent directory"),
            ));
        };
        std::fs::create_dir_all(parent)?;
        static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let uniq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let file_name = module_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "generated".to_string());
        let tmp_path = parent.join(format!("{file_name}.{}.{uniq}.tmp", std::process::id()));
        std::fs::write(&tmp_path, src)?;
        // rename overwrites an existing destination atomically; a concurrent
        // writer that already created `module_path` is harmless.
        std::fs::rename(&tmp_path, &module_path)?;
    }
    Ok(())
}

/// Blake3 content-address hash of `fields`, each framed with its own byte
/// length before hashing so two different field splits can never collide
/// (e.g. `["ab", "c"]` hashing the same as `["a", "bc"]` would under bare
/// concatenation). Truncated to 32 hex chars (128 bits) — collision-safe for
/// a content-addressed staging dir name.
pub(crate) fn content_hash_hex(fields: &[&[u8]]) -> String {
    let mut h = blake3::Hasher::new();
    for f in fields {
        h.update(&(f.len() as u64).to_le_bytes());
        h.update(f);
    }
    h.finalize().to_hex()[..32].to_string()
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

impl tidepool_runtime::session::OutputSink for CapturedOutput {
    fn drain(&self) -> Vec<String> {
        CapturedOutput::drain(self)
    }

    fn snapshot(&self) -> Vec<String> {
        CapturedOutput::snapshot(self)
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

    /// Core module + shim module + orchestrate module + preamble concatenated:
    /// content assertions that predate the importable-module split (and the
    /// later Core/shim split) check against the union of all generated
    /// sources the eval sees. NOT compilable as one file (two `module`
    /// headers) — `.contains()` assertions only.
    fn generated_sources(effects: &[EffectDecl], user_library: bool) -> String {
        let mut s = effects_core_module_source(effects);
        s.push_str(&effects_shim_module_source(effects, &RowArgs::default()));
        s.push_str(&orchestrate_module_source(effects));
        s.push_str(&build_preamble(effects, user_library));
        s
    }

    #[test]
    fn test_preamble_grep_glob_present() {
        let effects = vec![fs_decl()];
        let preamble = generated_sources(&effects, false);
        // grepGlob is the Fs structured text-search verb (the SG structural
        // combinators it used to sit beside were cut with the SG effect).
        assert!(preamble.contains("grepGlob :: forall effs. Member Fs effs => Text -> FilePath -> Eff effs (Either FsError [Hit])"));
    }

    #[test]
    fn test_preamble_qq_pragmas_always_on() {
        // Root decision: one eval dialect everywhere. See the FIXME at the
        // pragma line in build_preamble for the latency cost this carries
        // (extension-keyed TH provisioning) and the unpoison-fixed-binary
        // requirement it implies.
        for (src, name) in [
            (build_preamble(&[], false), "preamble"),
            (build_preamble(&[fs_decl()], true), "preamble+lib"),
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
            imports.push_str("Tidepool.QQ (fmt, j, patch, uri, form)\n");
        }
        let src = template_haskell(&pre, "'[]", code, &imports, "", None, None);
        let qq = src
            .find("import Tidepool.QQ (fmt, j, patch, uri, form)\n")
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
            imports.push_str("Tidepool.QQ (fmt, j, patch, uri, form)\n");
        }
        let src = template_haskell(&pre, "'[]", code, &imports, "", None, None);
        assert!(
            !src.contains("Tidepool.QQ"),
            "no-splice eval must not import Tidepool.QQ"
        );
    }

    #[test]
    fn test_build_preamble() {
        let effects = vec![
            EffectDecl {
                type_name: "Console",
                description: "Print output",
                constructors: &["Print :: Text -> Console ()"],
                type_defs: &[],
                extra_imports: &[],
                helpers: &[],
                type_params: &[],
                default_row_args: &[],
                prompt_card: None,
                helpers_row_polymorphic: false,
            },
            EffectDecl {
                type_name: "KV",
                description: "Key-value store",
                constructors: &[
                    "KvGet :: Text -> KV (Maybe Text)",
                    "KvSet :: Text -> Text -> KV ()",
                ],
                type_defs: &[],
                extra_imports: &[],
                helpers: &[],
                type_params: &[],
                default_row_args: &[],
                prompt_card: None,
                helpers_row_polymorphic: false,
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
        // Exec+Http present so both the decl env and the eval preamble emit
        // the qualified Tidepool.Shell/Git/Cargo imports (gated identically on
        // the same effect pair in both).
        let env = session_decl_module_env(&[exec_decl(), http_decl()], false);
        let preamble = build_preamble(&[exec_decl(), http_decl()], false);
        // Every decl import line appears verbatim in the eval preamble.
        for imp in &env.imports {
            assert!(
                preamble.contains(&format!("{imp}\n")),
                "session decl import `{imp}` missing from eval preamble — drift"
            );
        }
        // Same pragma block, modulo the decl-plane's intentional
        // NoMonomorphismRestriction (decl_pragmas adds it so nullary
        // constrained binds generalize; the eval expr module must NOT carry it
        // — see decl_pragmas). Strip that one addition, then the blocks match.
        let decl_pragmas_sans_nmr = env.pragmas.replace("NoMonomorphismRestriction, ", "");
        assert!(
            preamble.contains(&decl_pragmas_sans_nmr),
            "session decl pragmas (minus decl-only NMR) diverged from eval preamble"
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
            extra_imports: &[],
            helpers: &[],
            type_params: &[],
            default_row_args: &[],
            prompt_card: None,
            helpers_row_polymorphic: false,
        }];
        let preamble = build_preamble(&effects, false);
        let stack = build_effect_stack_type(&effects);
        let source = "do\n  let x = 42\n  pure x";

        let result = template_haskell(&preamble, &stack, source, "", "", None, None);

        assert!(result.contains("module Expr where"));
        assert!(result.contains("import Control.Monad.Freer hiding (run)"));
        // GADTs live in the generated Tidepool.Effects module now.
        assert!(result.contains("import Tidepool.Effects"));
        assert!(effects_core_module_source(&effects).contains("data Console a where"));
        // User code is a real top-level binding (expression-first contract).
        assert!(result.contains("__user = let {\n __b =\ndo\n  let x = 42\n  pure x\n } in __b"));
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
            extra_imports: &[],
            helpers: &[],
            type_params: &[],
            default_row_args: &[],
            prompt_card: None,
            helpers_row_polymorphic: false,
        }];
        let preamble = build_preamble(&effects, false);
        let stack = build_effect_stack_type(&effects);

        // Multi-line composition expression rides through VERBATIM (explicit
        // let-brackets suspend layout; no indent transform).
        let pipeline = "glob \"**/*.rs\"\n  >>= mapM getFileSize\n  <&> sizeRank 9";
        let r = template_haskell(&preamble, &stack, pipeline, "", "", None, None);
        assert!(r.contains(
            "__user = let {\n __b =\nglob \"**/*.rs\"\n  >>= mapM getFileSize\n  <&> sizeRank 9\n } in __b"
        ));

        // Trailing where-clause is legal: __user is a genuine declaration.
        let with_where = "sizeRank 9 <$> sized\n  where\n    sized = mapM go =<< glob \"**/*.rs\"";
        let r = template_haskell(&preamble, &stack, with_where, "", "", None, None);
        assert!(r.contains("__user = let {\n __b =\nsizeRank 9 <$> sized\n  where\n    sized ="));
    }

    #[test]
    fn test_eval_tool_description_includes_effects() {
        let effects = vec![EffectDecl {
            type_name: "Console",
            description: "Print to console",
            constructors: &["Print :: Text -> Console ()"],
            type_defs: &[],
            extra_imports: &[],
            helpers: &["putStrLn :: Text -> M ()\nputStrLn = send . Print"],
            type_params: &[],
            default_row_args: &[],
            prompt_card: None,
            helpers_row_polymorphic: false,
        }];
        let desc = build_eval_tool_description(&effects);
        // The slim floor lists each effect name + one-liner …
        assert!(desc.contains("Console: Print to console"));
        // … and points at the resources that carry the depth (per-effect
        // constructors/helpers now live in `tidepool://effect/{name}`, not inline).
        assert!(desc.contains("tidepool://effect/{name}"));
        assert!(!desc.contains("Built-in helpers"));
    }

    /// The assembled eval description attests to the idealized surface: no
    /// severity-halo vocabulary, no "JIT-safe" unsafe-zone implication, no
    /// closed-world "prefer the unqualified" framing. A regression that
    /// reintroduces a caution reads here as a failed assertion, not a review nit.
    #[test]
    fn eval_description_carries_no_caution_vocabulary() {
        let desc = build_eval_tool_description(&standard_decls());
        let lower = desc.to_lowercase();
        for banned in [
            "jit-safe",
            "prefer the unqualified",
            "do not",
            "with care",
            "use with caution",
            "footgun",
            "unsafe",
        ] {
            assert!(
                !lower.contains(banned),
                "assembled eval description must not contain caution vocabulary {banned:?}:\n{desc}"
            );
        }
    }

    /// The examples ARE the style guide: the primary `input` example is a typed
    /// decode, and the effect-failure example binds the `Right`. If the modelled
    /// idiom moves, these break — that is the point.
    #[test]
    fn eval_description_models_the_idealized_idiom() {
        let desc = build_eval_tool_description(&standard_decls());
        assert!(
            desc.contains("deriving (Generic, FromJSON)"),
            "primary input example must be a typed decode:\n{desc}"
        );
        assert!(
            desc.contains("Right p <- run"),
            "must model Either-returning effects:\n{desc}"
        );
        assert!(
            desc.contains("Left (FsNotFound _)"),
            "must model matching a specific Left:\n{desc}"
        );
        assert!(
            desc.contains("recommended surface"),
            "Prelude shadows get a positive attestation, not a JIT-safety hedge:\n{desc}"
        );
        assert!(
            desc.contains("tidepool://capabilities"),
            "the qualified-namespace list points at the live capabilities index:\n{desc}"
        );
        // #335: every verb in a modelled snippet returns `Either <Err> a`, so
        // every snippet that USES a verb's result must unwrap it first. These
        // two examples applied `<&> stake limit` / `<&> (^? …)` straight to the
        // `Either` and could not typecheck; pin the unwrapped spellings.
        assert!(
            desc.contains("Right hits <- grepGlob target \"**/*.rs\""),
            "the input-lane example must bind grepGlob's Right, not map over the Either:\n{desc}"
        );
        assert!(
            desc.contains("Right v <- llm (SObj"),
            "the llm extraction example must bind the Right before applying optics:\n{desc}"
        );
        assert!(
            !desc.contains("<&> stake limit") && !desc.contains("p <&> (^? key"),
            "no snippet may apply a pure function to an unwrapped Either result:\n{desc}"
        );
    }

    /// The per-effect descriptions are served verbatim as
    /// `tidepool://effect/{name}`, so their snippets are style guide too. Two
    /// things they must model, because following them otherwise does not
    /// compile: `askUser @T` needs `FromJSON` (its constraint is literally
    /// `DerivedForm a = (FormRoot a, FromJSON a)` — `Generic` alone builds the
    /// form but cannot read the submission back), and the typed spawn is
    /// `spawnAgent @r`, whose result type additionally needs `JsonSchema`.
    #[test]
    fn effect_descriptions_model_the_typed_derive_sets() {
        let ask = askuser_decl().description;
        assert!(
            ask.contains("derive `Generic` and `FromJSON`"),
            "askUser's derive set must name FromJSON, not Generic alone:\n{ask}"
        );
        assert!(
            ask.contains("deriving (Generic, FromJSON)") && ask.contains("askUser @Deploy"),
            "askUser must carry a worked `askUser @T` example:\n{ask}"
        );

        let sub = subagent_decl().description;
        assert!(
            sub.contains("deriving (Generic, FromJSON, JsonSchema)"),
            "a typed spawn result type needs JsonSchema in its derive set:\n{sub}"
        );
        assert!(
            sub.contains("spawnAgent @WorkerResult (spawnSpec"),
            "the typed spawn surface must be shown, not only recommended:\n{sub}"
        );
        assert!(
            sub.contains("Tidepool.Agent.Spawn"),
            "the typed spawn surface is not auto-imported — name its module:\n{sub}"
        );
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
        // #335: the primitive Fs verbs expose typed failure; the composite
        // helpers below (appendFile/doesFileExist/…) absorb it and keep their
        // shape.
        assert!(preamble.contains(
            "readFile :: forall effs. Member Fs effs => FilePath -> Eff effs (Either FsError Text)"
        ));
        assert!(preamble.contains("writeFile :: forall effs. Member Fs effs => FilePath -> Text -> Eff effs (Either FsError ())"));
        assert!(preamble.contains("appendFile :: forall effs. Member Fs effs => FilePath -> Text -> Eff effs (Either FsError ())"));
        assert!(preamble.contains("listDirectory :: forall effs. Member Fs effs => FilePath -> Eff effs (Either FsError [FilePath])"));
        assert!(preamble
            .contains("doesFileExist :: forall effs. Member Fs effs => FilePath -> Eff effs Bool"));
        assert!(preamble.contains(
            "getFileSize :: forall effs. Member Fs effs => FilePath -> Eff effs (Maybe Int)"
        ));
        assert!(preamble.contains(
            "fsMeta :: forall effs. Member Fs effs => FilePath -> Eff effs (Maybe FileMeta)"
        ));
        assert!(preamble.contains("glob :: forall effs. Member Fs effs => FilePath -> Eff effs (Either FsError [FilePath])"));
        // Core editing verbs (the str-replace common case + dry-run).
        assert!(preamble.contains("update :: forall effs. Member Fs effs => FilePath -> Text -> Text -> Eff effs UpdateOneOutcome"));
        assert!(preamble.contains("updateAll :: forall effs. Member Fs effs => FilePath -> Text -> Text -> Eff effs UpdateAllOutcome"));
        assert!(preamble.contains("planUpdate :: forall effs. Member Fs effs => FilePath -> Text -> Text -> Eff effs UpdateOutcome"));
        assert!(
            preamble.contains("insertAfter :: forall effs. Member Fs effs => FilePath -> Text -> Text -> Eff effs InsertAfterOutcome")
        );
        assert!(preamble.contains(
            "run :: forall effs. Member Exec effs => Text -> Eff effs (Either ExecError Proc)"
        ));
        // No old aliases (verb-type sweep: records over tuples, no dup names)
        assert!(!preamble.contains("fsRead"));
        assert!(!preamble.contains("fsWrite"));
        assert!(!preamble.contains("callCommand"));
        assert!(!preamble.contains("readProcess"));
        assert!(!preamble.contains("fsGlob"));
        assert!(!preamble.contains("fsMetadata"));
        assert!(!preamble.contains("parseFileMeta"));
        // `say` is the Console wrapper (re-added 2026-06-22, friction #5).
        assert!(preamble.contains("say :: forall effs. Member Console effs => Text -> Eff effs ()"));
        // Other helpers unchanged
        assert!(preamble
            .contains("kvGet :: forall effs. Member KV effs => Text -> Eff effs (Maybe Value)"));
        // #335: httpGet is errors-tagged.
        assert!(preamble.contains(
            "httpGet :: forall effs. Member Http effs => Text -> Eff effs (Either HttpError Value)"
        ));
        assert!(preamble
            .contains("ask :: forall effs. Member Ask effs => Schema -> Text -> Eff effs Value"));
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
        assert!(helpers
            .contains("ask :: forall effs. Member Ask effs => Schema -> Text -> Eff effs Value"));
        assert!(helpers.contains("schemaToValue :: Schema -> Value"));
        assert!(!helpers.contains("askQ"));
    }

    #[test]
    fn test_standard_decls_includes_ask() {
        let decls = standard_decls();
        assert_eq!(decls.len(), 12);
        assert_eq!(decls[3].type_name, "Http");
        assert_eq!(decls[4].type_name, "Exec");
        assert_eq!(decls[5].type_name, "Lsp");
        assert_eq!(decls[6].type_name, "Llm");
        assert_eq!(decls[7].type_name, "Git");
        assert_eq!(decls[8].type_name, "Time");
        assert_eq!(decls[9].type_name, "Ask");
        // RunLLMTurn (self-iterating-harness WS-B) was split out of Ask into
        // its own interposed effect, appended right after it.
        assert_eq!(decls[10].type_name, "RunLLMTurn");
        // Fork (answerer parallel delegation) appended after RunLLMTurn.
        assert_eq!(decls[11].type_name, "Fork");
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
        assert!(preamble.contains(
            "type M = Eff '[Console, KV, Fs, Http, Exec, Lsp, Llm, Git, Time, Ask, RunLLMTurn, Fork]"
        ));
    }

    #[test]
    fn test_ask_in_effect_stack_type() {
        let decls = standard_decls();
        let stack = build_effect_stack_type(&decls);
        assert_eq!(
            stack,
            "'[Console, KV, Fs, Http, Exec, Lsp, Llm, Git, Time, Ask, RunLLMTurn, Fork]"
        );
    }

    #[test]
    fn test_preamble_hides_run_from_freer() {
        let decls = standard_decls();
        let preamble = generated_sources(&decls, false);
        assert!(preamble.contains("import Control.Monad.Freer hiding (run)"));
        // Our run helper should still be present (#335: errors-tagged).
        assert!(preamble.contains(
            "run :: forall effs. Member Exec effs => Text -> Eff effs (Either ExecError Proc)\nrun = send . Run"
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
        // #335: Run/RunIn are errors-tagged.
        assert!(decl
            .constructors
            .iter()
            .any(|c| c.contains("Run :: Text -> Exec (Either ExecError Proc)")));
        assert!(decl
            .constructors
            .iter()
            .any(|c| c.contains("RunIn :: Text -> Text -> Exec (Either ExecError Proc)")));
    }

    #[test]
    fn test_preamble_orchestration_helpers() {
        let decls = standard_decls();
        // The orchestration helpers moved OUT of the expr-module preamble into
        // the generated Tidepool.Orchestrate module (the namespace-poison fix);
        // assert their signatures there instead.
        let orch = orchestrate_module_source(&decls);
        // runChecked runs a command and returns stdout, erroring on nonzero
        // exit (assert the signature; the body is volatile).
        assert!(orch.contains("runChecked :: Text -> M Text"));
        // File manipulation helpers
        assert!(orch.contains("mapFile :: Text -> (Text -> Text) -> M ()"));
        assert!(orch.contains("mapFileM :: Text -> (Text -> M Text) -> M ()"));
        assert!(orch.contains("searchFiles :: Text -> Text -> M [Hit]"));
        assert!(orch.contains("lineCount :: Text -> M Int"));
        assert!(orch.contains("fileContains :: Text -> Text -> M Bool"));
        // KV batch helper. No orchestration `kvClear :: M ()` here — it
        // collided with the effect helper `kvClear :: Text -> Eff effs Int`
        // (dup-survey item 1); the effect helper strictly subsumes it.
        assert!(orch.contains("kvAll :: M [(Text, Value)]"));
        assert!(!orch.contains("kvClear :: M ()"));
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
        let effects_mod = effects_core_module_source(&decls);
        assert!(effects_mod.contains("data Schema = SObj"));
        assert!(effects_mod
            .contains("ask :: forall effs. Member Ask effs => Schema -> Text -> Eff effs Value"));
        // #335: llm is errors-tagged (fully total — budget exhaustion is DATA);
        // tryLlm is gone (llm supersedes it).
        assert!(effects_mod.contains("llm :: forall effs. Member Llm effs => Schema -> Text -> Eff effs (Either LlmError Value)"));
        assert!(!effects_mod.contains("tryLlm"));
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
        let no_llm_mod = effects_core_module_source(&no_llm);
        assert!(no_llm_mod
            .contains("ask :: forall effs. Member Ask effs => Schema -> Text -> Eff effs Value"));
        // llm needs the Llm effect — absent from an Llm-less stack.
        assert!(!no_llm_mod.contains("llm :: forall effs. Member Llm effs => Schema -> Text -> Eff effs (Either LlmError Value)"));
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
            extra_imports: &[],
            helpers: &[],
            type_params: &[],
            default_row_args: &[],
            prompt_card: None,
            helpers_row_polymorphic: false,
        }];
        let preamble = build_preamble(&effects, false);
        let stack = build_effect_stack_type(&effects);
        let source = "pure 42";

        // With budget
        let result = template_haskell(&preamble, &stack, source, "", "", None, Some(1024));
        assert!(result.contains("kvSet \"__sayChars\" (toJSON (0 :: Int))"));
        assert!(result.contains("paginateResult (max 100 (1024 - _sayC)) (toJSON _r)"));

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
            extra_imports: &[],
            helpers: &[],
            type_params: &[],
            default_row_args: &[],
            prompt_card: None,
            helpers_row_polymorphic: false,
        }];
        let preamble = build_preamble(&effects, false);
        let stack = build_effect_stack_type(&effects);
        let source = "pure 42";
        let input = serde_json::json!({"val": 123});

        let result = template_haskell(&preamble, &stack, source, "", "", Some(&input), None);

        assert!(result.contains("input :: Aeson.Value"));
        assert!(result
            .contains("input = object [\"val\" .= Aeson.Number (Aeson.scientific (123) (0))]"));
    }

    #[test]
    fn test_eval_timeout_value() {
        assert_eq!(EVAL_TIMEOUT_SECS, 600);
    }

    /// The `eval` tool schema must attest to the REAL clamp (600/1800), not a
    /// stale hand-copied "Default 120; clamped to [1, 600]" claim — and the
    /// sentinel must never leak into the client-visible schema.
    #[test]
    fn eval_request_schema_reports_real_timeout_constants() {
        let schema = eval_request_input_schema().unwrap();
        let json = serde_json::to_string(schema.as_ref()).unwrap();
        assert!(
            json.contains(&format!(
                "Default {EVAL_TIMEOUT_SECS}; clamped to [1, {MAX_EVAL_TIMEOUT_SECS}]."
            )),
            "schema must contain the live timeout constants: {json}"
        );
        assert!(
            !json.contains(TIMEOUT_SECS_DOC_SENTINEL),
            "sentinel must not leak into the client-visible schema: {json}"
        );
        assert!(
            !json.contains("Default 120"),
            "the old stale default (120) must not appear: {json}"
        );
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
    /// Snapshot test: blake3 of a fixed pair of strings must produce the same
    /// hash in every process. If DefaultHasher (randomly seeded) were
    /// accidentally reintroduced, this assertion fails because the computed
    /// hash won't match the stable blake3 value baked into the expected dir
    /// name.
    #[test]
    fn test_effects_hash_is_deterministic_across_calls() {
        let eff = "module Tidepool.Effects where\n-- sentinel\n";
        let orch = "module Tidepool.Orchestrate where\n-- sentinel\n";
        let dir1 = write_shim_module(eff, orch).unwrap();
        let dir2 = write_shim_module(eff, orch).unwrap();
        assert_eq!(
            dir1, dir2,
            "same source must yield same content-addressed dir"
        );
        // Verify the dir name encodes the known blake3 hash of BOTH sources
        // (changing either busts the dir).
        let expected_hash = content_hash_hex(&[eff.as_bytes(), orch.as_bytes()]);
        let expected_suffix = format!("tidepool-effects-{expected_hash}");
        let dir_name = dir1.file_name().unwrap().to_str().unwrap();
        assert_eq!(
            dir_name, expected_suffix,
            "dir name must be the stable blake3 hash of both sources; got {dir_name}"
        );
        // A change to the orchestrate source alone busts the dir.
        let dir3 = write_shim_module(eff, "module Tidepool.Orchestrate where\n-- other\n").unwrap();
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
        let dir = write_shim_module(&eff, &orch).unwrap();
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
        let dir2 = write_shim_module(&eff, &orch).unwrap();
        assert_eq!(dir, dir2, "content-addressed path is stable across calls");
        assert!(module.exists(), "effects self-healed after reap");
        assert!(orch_module.exists(), "orchestrate self-healed after reap");
        assert_eq!(std::fs::read_to_string(&module).unwrap(), eff);
        assert_eq!(std::fs::read_to_string(&orch_module).unwrap(), orch);
        let _ = std::fs::remove_dir_all(&dir);
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

    // ---------------------------------------------------------------------------
    // #315 regression: normalize_input → input_binding_source source-generation
    // round-trip for all five payload shapes.  Fast (no JIT/GHC).
    // ---------------------------------------------------------------------------

    /// Helper: apply normalize_input then render to the Haskell binding snippet.
    fn binding_for(v: &serde_json::Value) -> String {
        let normalized = normalize_input(v);
        input_binding_source(Some(&normalized))
    }

    /// Per-payload-shape cases: each row is a raw JSON input plus the
    /// substring(s) its generated Haskell binding must contain.
    #[test]
    fn test_input_source_gen_renders_expected_binding() {
        let cases: Vec<(&str, serde_json::Value, Vec<&str>)> = vec![
            // THE #315 CASE: a double-encoded string (MCP client
            // JSON.stringify'd the payload) must unwrap so the generated
            // binding contains the bare string, not the surrounding
            // quotes/escapes as literal characters.
            (
                "double_encoded_string",
                json!("\"hello\""),
                vec![r#"Aeson.String "hello""#],
            ),
            // Multi-line double-encoded string (the bug-report shape): after
            // unwrapping, the \n should be in the Haskell escape, not the
            // outer quotes.
            (
                "double_encoded_multiline",
                json!("\"line1\\nline2\""),
                vec![r#"Aeson.String "line1\nline2""#],
            ),
            // Plain string (not double-encoded) passes through.
            (
                "plain_string",
                json!("hello world"),
                vec![r#"Aeson.String "hello world""#],
            ),
            // Number: binding emits `Aeson.Number (Aeson.scientific (42) (0))`.
            (
                "number",
                json!(42),
                vec!["Aeson.Number (Aeson.scientific (42) (0))"],
            ),
            // Bool true: binding emits `Aeson.Bool True`.
            ("bool_true", json!(true), vec!["Aeson.Bool True"]),
            // Bool false: binding emits `Aeson.Bool False`.
            ("bool_false", json!(false), vec!["Aeson.Bool False"]),
            // Object: binding emits `object [...]`.
            (
                "object",
                json!({"key": "val"}),
                vec!["object [", r#""key" .= Aeson.String "val""#],
            ),
            // Array: binding emits `toJSON [...]`.
            (
                "array",
                json!(["x", "y"]),
                vec!["toJSON [", r#"Aeson.String "x""#, r#"Aeson.String "y""#],
            ),
        ];

        for (label, raw, expected_substrings) in cases {
            let src = binding_for(&raw);
            for expected in expected_substrings {
                assert!(
                    src.contains(expected),
                    "case {label}: expected {expected:?} in: {src}"
                );
            }
        }
    }

    /// THE #315 CASE, negative half: a double-encoded string must NOT leave
    /// the literal outer quotes/escapes in the generated binding.
    #[test]
    fn test_input_source_gen_double_encoded_string_does_not_double_encode() {
        let raw = json!("\"hello\"");
        let src = binding_for(&raw);
        assert!(
            !src.contains(r#"Aeson.String "\"hello\"""#),
            "double-encoding detected in: {src}"
        );
    }
}
