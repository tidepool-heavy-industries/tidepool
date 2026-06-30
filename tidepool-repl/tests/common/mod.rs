//! Shared harness for the tidepool-repl session hardening suites.
//!
//! Each dimension test file does `mod common;` + `use common::*;` and drives the
//! REAL `tidepool-repl` MCP entry point (`dispatch_tool`) over multiple turns,
//! per the standing rule (no bespoke internal wiring). A small 2 MiB nursery
//! forces organic GC under heavy-allocation turns.

#![allow(dead_code)]

use std::path::PathBuf;

use rmcp::model::{CallToolResult, RawContent};
use tidepool_handlers::{base_decls_with_ask, build_minimal_stack};
use tidepool_repl::{ReplServerConfig, TidepoolReplServer};
use tidepool_runtime::session::ModuleEnv;

/// True if the session-aware `tidepool-extract` is reachable (else the suite
/// skips cleanly — CI without the nix shell / `TIDEPOOL_EXTRACT` set).
pub fn extract_available() -> bool {
    let bin = std::env::var("TIDEPOOL_EXTRACT").unwrap_or_else(|_| "tidepool-extract".into());
    std::process::Command::new(bin)
        .arg("--numeric-version")
        .output()
        .is_ok()
}

/// The first text content block of a tool result.
pub fn text_of(res: &CallToolResult) -> String {
    match &res.content[0].raw {
        RawContent::Text(t) => t.text.clone(),
        other => panic!("expected text content, got {other:?}"),
    }
}

/// A `serde_json::Map` from string pairs (tool arguments).
pub fn obj(pairs: &[(&str, &str)]) -> serde_json::Map<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
        .collect()
}

/// Build a repl server with a deliberately SMALL nursery (2 MiB) so heavy-alloc
/// turns trigger a real minor GC between bind and read. No continuation reaper.
pub fn build_server() -> TidepoolReplServer {
    build_server_with_ttl(None)
}

/// As [`build_server`], but with an explicit continuation/wedge reaper TTL
/// (`Some(tiny)` to exercise reaping fast in a test; `None` to disable it).
pub fn build_server_with_ttl(continuation_ttl: Option<std::time::Duration>) -> TidepoolReplServer {
    build_server_full(continuation_ttl, None)
}

/// As [`build_server`], with explicit reaper TTL AND per-turn timeout. A short
/// `turn_timeout` lets a test exercise the timeout/self-heal path without the
/// 120 s production budget.
pub fn build_server_full(
    continuation_ttl: Option<std::time::Duration>,
    turn_timeout: Option<std::time::Duration>,
) -> TidepoolReplServer {
    let stack = build_minimal_stack();
    let (decls, ask_tag) = base_decls_with_ask(&stack);
    let effects_dir =
        tidepool_mcp::ensure_effects_module(&decls).expect("write Tidepool.Effects module");
    let prelude_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root")
        .join("haskell")
        .join("lib");
    let session_root_base = std::env::temp_dir().join(format!(
        "tidepool-repl-harden-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let cfg = ReplServerConfig {
        decls,
        ask_tag,
        base_include: vec![effects_dir, prelude_dir],
        module_env: ModuleEnv::standalone_default(),
        session_root_base,
        nursery_size: Some(1 << 21), // 2 MiB
        continuation_ttl,
        turn_timeout,
    };
    TidepoolReplServer::new(stack, cfg)
}

/// One turn's result: the rendered text + whether it surfaced as an MCP error.
#[derive(Debug, Clone)]
pub struct Turn {
    pub text: String,
    pub is_error: bool,
}

impl Turn {
    pub fn ok(&self) -> bool {
        !self.is_error
    }
    /// Assert success, returning the text (panics with the error text otherwise).
    pub fn expect_ok(&self, ctx: &str) -> &str {
        assert!(!self.is_error, "{ctx}: unexpected error: {}", self.text);
        &self.text
    }
    /// Assert this turn errored (graceful failure), returning the text.
    pub fn expect_err(&self, ctx: &str) -> &str {
        assert!(
            self.is_error,
            "{ctx}: expected an error, got ok: {}",
            self.text
        );
        &self.text
    }
    pub fn contains(&self, needle: &str) -> bool {
        self.text.contains(needle)
    }
}

/// An ergonomic wrapper over the server: terse async verbs for each tool.
pub struct Repl {
    pub server: TidepoolReplServer,
}

impl Repl {
    /// Build a server (small nursery). Caller should guard with `extract_available`.
    pub fn new() -> Repl {
        Repl {
            server: build_server(),
        }
    }

    /// Build a server with an explicit reaper TTL (for lifecycle/reaper tests).
    pub fn with_ttl(ttl: std::time::Duration) -> Repl {
        Repl {
            server: build_server_with_ttl(Some(ttl)),
        }
    }

    /// Build a server with a short per-turn timeout AND reaper TTL (for the
    /// timeout / self-healing-Wedged tests).
    pub fn with_timeout(turn_timeout: std::time::Duration, ttl: std::time::Duration) -> Repl {
        Repl {
            server: build_server_full(Some(ttl), Some(turn_timeout)),
        }
    }

    async fn dispatch(&self, tool: &str, args: serde_json::Map<String, serde_json::Value>) -> Turn {
        let r = self
            .server
            .dispatch_tool(tool, args)
            .await
            .unwrap_or_else(|e| panic!("dispatch `{tool}` transport error: {e:?}"));
        Turn {
            text: text_of(&r),
            is_error: r.is_error == Some(true),
        }
    }

    /// Route a single item through `session_run` and unwrap `items[0]` back to
    /// the legacy `Turn` shape (`text = items[0].result`, `is_error = !items[0].ok`).
    /// Falls back to the raw `Turn` when the block envelope can't be parsed
    /// (e.g. "no session open" — the error text is not a JSON block envelope).
    async fn run_block_single(&self, item: &str) -> Turn {
        let mut args = serde_json::Map::new();
        args.insert(
            "items".to_string(),
            serde_json::Value::Array(vec![serde_json::Value::String(item.to_string())]),
        );
        let raw = self.dispatch("session_run", args).await;

        // Strip the "## Output\n...\n## Result\n" prefix if captured output is present.
        let json_part = if let Some(pos) = raw.text.rfind("\n## Result\n") {
            &raw.text[pos + "\n## Result\n".len()..]
        } else {
            &raw.text
        };

        // Extract items[0] from the block envelope JSON.
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_part) {
            if let Some(item0) = v.get("items").and_then(|arr| arr.get(0)) {
                let text = item0
                    .get("result")
                    .and_then(|r| r.as_str())
                    .unwrap_or(&raw.text)
                    .to_string();
                let is_error = item0
                    .get("ok")
                    .and_then(|o| o.as_bool())
                    .map(|ok| !ok)
                    .unwrap_or(raw.is_error);
                return Turn { text, is_error };
            }
        }

        // Not a block envelope (e.g. "no session open" plain error) — return as-is.
        raw
    }

    pub async fn open(&self) -> Turn {
        self.dispatch("session_open", serde_json::Map::new()).await
    }
    pub async fn def(&self, decl: &str) -> Turn {
        self.run_block_single(decl).await
    }
    pub async fn eval(&self, code: &str) -> Turn {
        self.run_block_single(code).await
    }
    pub async fn cmd(&self, command: &str) -> Turn {
        self.run_block_single(command).await
    }
    pub async fn close(&self) -> Turn {
        self.dispatch("session_close", serde_json::Map::new()).await
    }

    /// open + assert ok (the common preamble for every suite).
    pub async fn open_ok(&self) {
        self.open().await.expect_ok("open");
    }
    /// eval + assert ok, returning the text.
    pub async fn eval_ok(&self, code: &str) -> String {
        self.eval(code).await.expect_ok(code).to_string()
    }
}

impl Default for Repl {
    fn default() -> Repl {
        Repl::new()
    }
}
