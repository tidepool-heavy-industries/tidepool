//! Concrete effect handlers for the Tidepool eval server.
//!
//! Provides the base handlers (Console, KV, Fs, Http, Exec, Lsp, Llm, Git, Time),
//! the debug-only MetaHandler, and the [`build_base_stack`] / [`base_decls_with_ask`]
//! convenience functions for assembling a fully-wired eval server.
//!
//! ## Stack assembly
//!
//! ```no_run
//! # use std::path::PathBuf;
//! # use tidepool_handlers::{HandlerConfig, build_base_stack, base_decls_with_ask};
//! # use tidepool_mcp::TidepoolMcpServer;
//! // Must be called inside a tokio runtime (LlmHandler captures Handle::current()).
//! let cfg = HandlerConfig {
//!     cwd: PathBuf::from("."),
//!     kv_path: PathBuf::from(".tidepool/kv.json"),
//!     llm_model: "gpt-4o-mini".into(),
//! };
//! let stack = build_base_stack(&cfg);
//! let server = TidepoolMcpServer::new(stack);
//! ```

#![warn(clippy::unwrap_used, clippy::expect_used)]
use std::path::PathBuf;

use tidepool_mcp::{CapturedOutput, CollectEffectDecls, EffectDecl};

pub(crate) mod effect_glue;

// Request enums, error ADTs and dispatch glue generated from the
// `tidepool-protocol` schema, for the effects that have migrated. Each
// effect's own module re-exports its types, so the public paths are
// unchanged (`tidepool_handlers::ExecReq`).
mod generated;

pub mod handlers;
pub use handlers::*;

// The six bridged wire records now live in the low `tidepool-bridge-effects`
// crate (single source of truth shared with test mocks) — re-exported here so
// the external surface (`tidepool_handlers::Proc`, etc.) is unchanged.
pub use tidepool_bridge_effects::{
    bridged_records_module, FileMeta, GitCommit, GitCommitDeltas, GitFileDelta, GitStatusEntry,
    Hit, Proc,
};

// ============================================================================
// Stack assembly
// ============================================================================

/// Configuration for building the base effect handler stack.
pub struct HandlerConfig {
    /// Working directory (sandbox root for Fs/Lsp/Git; Exec's initial cwd
    /// only — Exec itself is not filesystem-sandboxed, see
    /// `tidepool-handlers/CLAUDE.md`'s Sandboxing section).
    pub cwd: PathBuf,
    /// Path for the KV store's JSON backing file.
    pub kv_path: PathBuf,
    /// LLM model name (routed by genai: gpt-* → OpenAI, claude-* → Anthropic, etc.).
    pub llm_model: String,
}

/// Map an effect name (as listed in `tidepool_mcp::base_effects!`) to its
/// handler-construction expression. This is a NAME-keyed lookup (arm order is
/// irrelevant); the load-bearing ORDER lives solely in `base_effects!`, so this
/// map stays in lockstep with the decl list by construction.
macro_rules! handler_for {
    (Console, $cfg:ident) => {
        ConsoleHandler
    };
    (KV,      $cfg:ident) => {
        KvHandler::new($cfg.kv_path.clone())
    };
    (Fs,      $cfg:ident) => {
        FsHandler::new($cfg.cwd.clone())
    };
    (Http,    $cfg:ident) => {
        HttpHandler
    };
    (Exec,    $cfg:ident) => {
        ExecHandler::new($cfg.cwd.clone())
    };
    (Lsp,     $cfg:ident) => {
        LspHandler::new($cfg.cwd.clone())
    };
    (Llm,     $cfg:ident) => {
        LlmHandler::new($cfg.llm_model.clone())
    };
    (Git,     $cfg:ident) => {
        GitHandler::new($cfg.cwd.clone())
    };
    (Time,    $cfg:ident) => {
        TimeHandler
    };
}

/// Build the base effect stack (tags 0–8: Console, KV, Fs, Http, Exec, Lsp, Llm, Git, Time).
///
/// **Must be called inside a tokio runtime** — `LlmHandler` captures
/// `tokio::runtime::Handle::current()` at construction time.
///
/// Ask (tag 9 on this 9-handler stack — see [`base_decls_with_ask`]) is
/// **not** included here; it is interposed by `tidepool_runtime::session`'s
/// `SessionEngine`/`GateDispatcher` (see `TidepoolMcpServer::new`), not by a
/// handler in this HList.
pub fn build_base_stack(
    cfg: &HandlerConfig,
) -> impl tidepool_effect::dispatch::DispatchEffect<CapturedOutput>
       + CollectEffectDecls
       + Clone
       + Send
       + Sync
       + 'static {
    // The handler HList is generated from the single-source `base_effects!`
    // list in `tidepool-mcp` (the SAME sequence that drives `standard_decls`,
    // the `type M = Eff '[…]` string, and the union-tag positions). Reordering
    // or cutting an effect is a single edit THERE — this fn just maps each
    // effect name to its handler constructor (`handler_for!`), so the two
    // orders cannot desync.
    macro_rules! build_stack_rows {
        ($(($name:ident, $decl:ident)),* $(,)?) => {
            frunk::hlist![ $( handler_for!($name, cfg) ),* ]
        };
    }
    tidepool_mcp::base_effects!(build_stack_rows)
}

/// Build the debug effect stack: the same base effects as [`build_base_stack`]
/// (tags 0–8) plus `MetaHandler` appended last (tag 9) — the `--debug`-only
/// self-mirror. Mirrors `build_base_stack`'s callback exactly, so the two
/// stacks can never desync on order; the ONLY difference is the trailing
/// `MetaHandler::new(effect_names, helper_sigs)` row. Callers derive
/// `effect_names`/`helper_sigs` from the SAME `base_effects!`-ordered decl
/// list (plus `meta_decl()` appended, matching this stack's tag order) so
/// `metaEffects`/`metaHelp` report the actual running stack, not a
/// hand-maintained guess.
///
/// **Must be called inside a tokio runtime** (see [`build_base_stack`]).
pub fn build_debug_stack(
    cfg: &HandlerConfig,
    effect_names: Vec<String>,
    helper_sigs: Vec<String>,
) -> impl tidepool_effect::dispatch::DispatchEffect<CapturedOutput>
       + CollectEffectDecls
       + Clone
       + Send
       + Sync
       + 'static {
    macro_rules! build_debug_stack_rows {
        ($(($name:ident, $decl:ident)),* $(,)?) => {
            frunk::hlist![
                $( handler_for!($name, cfg) ),*,
                MetaHandler::new(effect_names, helper_sigs)
            ]
        };
    }
    tidepool_mcp::base_effects!(build_debug_stack_rows)
}

/// Build the MINIMAL effect stack (tag 0: Console only).
///
/// For cheap-startup sessions and tests that exercise the session mechanism
/// rather than the effects — it avoids constructing the heavier handlers (Llm's
/// genai client, the cwd-bound Fs/Exec/Lsp). Ask (the next tag) is interposed
/// the same way as with [`build_base_stack`], not by a handler here. Pair
/// with [`base_decls_with_ask`] (which is generic over any `CollectEffectDecls`
/// stack) to derive `(decls, ask_tag)`.
pub fn build_minimal_stack() -> impl tidepool_effect::dispatch::DispatchEffect<CapturedOutput>
       + CollectEffectDecls
       + Clone
       + Send
       + Sync
       + 'static {
    frunk::hlist![ConsoleHandler]
}

/// Collect effect declarations from a base stack and append the interposed
/// effects (`Ask`, then `RunLLMTurn` — WS-B split `runLLMTurn` out of `Ask`).
/// No `Fork`: the ordinary session engine servicing this stack never accepts
/// `ForkWith`/`ForkAllWith` (vestigial-subsystems review §4) — see
/// `tidepool_mcp::EffectRoster::from_handlers`'s doc.
///
/// Returns `(decls, ask_tag)` where `ask_tag` is the index of the FIRST
/// interposed effect (`Ask`) in `decls` — the suspend threshold every tag at
/// or beyond it shares (see `jit_machine::drive_effect_loop`'s `suspend_tag`
/// doc). A thin tuple-shaped adapter over `tidepool_mcp::EffectRoster` — the
/// single place that actually appends the interposed suffix and derives the
/// tag — kept so existing `(Vec<EffectDecl>, u64)` call sites don't need to
/// migrate to the roster type themselves.
pub fn base_decls_with_ask<H: CollectEffectDecls>(stack: &H) -> (Vec<EffectDecl>, u64) {
    let roster = tidepool_mcp::EffectRoster::from_handlers(stack);
    (roster.decls().to_vec(), roster.suspend_tag())
}

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests {
    use crate::test_support::*;

    #[test]
    fn all_effect_constructors_in_table() {
        let table = full_effect_test_table();
        for decl in &tidepool_mcp::standard_decls() {
            for con_str in decl.constructors {
                let parsed = tidepool_mcp::parse_constructor(con_str).unwrap();
                let id = table.get_by_name(&parsed.name).unwrap_or_else(|| {
                    panic!(
                        "Constructor '{}' from effect '{}' missing from test DataConTable",
                        parsed.name, decl.type_name
                    )
                });
                let dc = table.get(id).unwrap();
                assert_eq!(
                    dc.rep_arity, parsed.arity,
                    "Arity mismatch for '{}': decl says {} but table has {}",
                    parsed.name, parsed.arity, dc.rep_arity
                );
            }
        }
    }

    // === JIT-level roundtrip tests ===
    //
    // Bundled per effect family into one tidepool-extract compile each. Each
    // bundle returns the list of FAILED check names (empty on success) so a
    // regression still names which behavior broke — see
    // tidepool-runtime/tests/generic_form_roundtrip.rs's `check` helper for
    // the idiom. Fs's own roundtrips live in `handlers::fs::tests`.

    /// Checks Console (`putStrLn` dispatches without aborting the eval) and
    /// KV (`kvSet`/`kvGet` round-trips a JSON value) in one compile.
    #[tokio::test]
    async fn test_jit_console_kv_family() {
        let result = jit_eval(&[
            "let check nm ok = if ok then [] else [nm]",
            "putStrLn \"hello from JIT\"",
            "kvSet \"jit_test\" (toJSON (42 :: Int))",
            "v <- kvGet \"jit_test\"",
            "let c1 = check \"console-print-dispatches\" True",
            "let c2 = check \"kv-set-get-roundtrip\" (toJSON v == toJSON (42 :: Int))",
            "pure (concat [c1, c2])",
        ]);
        assert_eq!(result, serde_json::json!([]), "failed checks: {result}");
    }
}
