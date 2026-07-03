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

use std::path::PathBuf;

use tidepool_mcp::{CapturedOutput, CollectEffectDecls, EffectDecl};

pub mod handlers;
pub use handlers::*;

// ============================================================================
// Stack assembly
// ============================================================================

/// Configuration for building the base effect handler stack.
pub struct HandlerConfig {
    /// Working directory (sandbox root for Fs, Sg, Exec, Lsp, Git).
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
/// Ask (tag 10) is **not** included here; it is interposed by each server's
/// `AskDispatcher` wrapper (see `TidepoolMcpServer::new`).
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

/// Build the MINIMAL effect stack (tag 0: Console only).
///
/// For cheap-startup sessions and tests that exercise the session mechanism
/// rather than the effects — it avoids constructing the heavier handlers (Llm's
/// genai client, the cwd-bound Fs/Exec/Lsp). Ask (the next tag) is interposed
/// by each server's `AskDispatcher` wrapper, as with [`build_base_stack`]. Pair
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

/// Collect effect declarations from a base stack and append the Ask effect.
///
/// Returns `(decls, ask_tag)` where `ask_tag` is the index of the Ask effect
/// in `decls`. Mirrors `TidepoolMcpServer::new`'s internal logic so Wave B
/// servers can build the same declaration list without constructing a full
/// `TidepoolMcpServer`.
pub fn base_decls_with_ask<H: CollectEffectDecls>(_stack: &H) -> (Vec<EffectDecl>, u64) {
    let mut decls = H::collect_decls();
    let ask_tag = decls.len() as u64;
    decls.push(tidepool_mcp::ask_decl());
    (decls, ask_tag)
}

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests {
    use crate::test_support::*;
    use tidepool_bridge::ToCore;
    use tidepool_eval::value::Value;

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

    const EFFECTS_WITH_ROUNDTRIP_TESTS: &[&str] = &[
        "Console", "KV", "Fs", "Http", "Exec", "Lsp", "Llm", "Git", "Time", "Ask",
    ];

    #[test]
    fn all_effects_have_roundtrip_coverage() {
        let declared: Vec<&str> = tidepool_mcp::standard_decls()
            .iter()
            .map(|d| d.type_name)
            .collect();
        let missing: Vec<&&str> = declared
            .iter()
            .filter(|name| !EFFECTS_WITH_ROUNDTRIP_TESTS.contains(name))
            .collect();
        assert!(
            missing.is_empty(),
            "Effects in standard_decls() without roundtrip tests: {:?}\n\
             Add roundtrip tests and update EFFECTS_WITH_ROUNDTRIP_TESTS.",
            missing
        );
    }

    // === Ask construction test ===

    #[test]
    fn test_ask_constructor_in_table() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("AskWith").unwrap();
        let dc = table.get(con_id).unwrap();
        assert_eq!(dc.rep_arity, 2, "AskWith should have arity 2");
        let prompt = "What is your name?".to_string().to_value(&table).unwrap();
        let meta = "{}".to_string().to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![prompt, meta]);
        match &val {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(*id).unwrap(), "AskWith");
                assert_eq!(fields.len(), 2);
            }
            _ => panic!("Expected Con"),
        }
    }

    // === JIT-level roundtrip tests ===

    #[tokio::test]
    async fn test_jit_console_roundtrip() {
        let result = jit_eval(&["putStrLn \"hello from JIT\"", "pure (toJSON True)"]);
        assert_eq!(result, serde_json::json!(true));
    }

    #[tokio::test]
    async fn test_jit_kv_roundtrip() {
        let result = jit_eval(&[
            "kvSet \"jit_test\" (toJSON (42 :: Int))",
            "v <- kvGet \"jit_test\"",
            "pure (toJSON v)",
        ]);
        assert_eq!(result, serde_json::json!(42));
    }

    #[tokio::test]
    async fn test_jit_fs_exists_roundtrip() {
        let result = jit_eval(&["b <- doesFileExist \"Cargo.toml\"", "pure (toJSON b)"]);
        assert_eq!(result, serde_json::json!(true));
    }

    #[tokio::test]
    async fn test_jit_fs_listdir_roundtrip() {
        let result = jit_eval(&[
            "entries <- listDirectory \".\"",
            "pure (toJSON (length entries > 0))",
        ]);
        assert_eq!(result, serde_json::json!(true));
    }
}
