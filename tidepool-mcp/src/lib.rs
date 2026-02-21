//! MCP (Model Context Protocol) server library for Tidepool.
//!
//! Wraps `tidepool-runtime` in an MCP server exposing `run_haskell`,
//! `compile_haskell`, and `eval` tools. Generic over effect handler stacks
//! via `TidepoolMcpServer<H>`.

use dyn_clone::{clone_trait_object, DynClone};
use rmcp::{
    model::*,
    service::RequestContext,
    ErrorData as McpError, RoleServer, ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tidepool_runtime::DispatchEffect;
use tokio::io::{stdin, stdout};

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
// Request types
// ---------------------------------------------------------------------------

/// Request parameters for the structured `eval` tool.
///
/// Provide do-notation lines; the server wraps them in a full Haskell module
/// with the correct effect stack type, LANGUAGE pragmas, and imports.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EvalRequest {
    /// Lines of do-notation Haskell code. Each line becomes one line in a
    /// do-block. Use `pure x` as the last line to return a value.
    /// Use `send (Constructor args)` to invoke effects.
    pub source: Vec<String>,
    /// Additional Haskell module imports (e.g. `"Data.List"`). Optional.
    #[serde(default)]
    pub imports: Vec<String>,
    /// Top-level helper definitions placed before the main binding. Optional.
    /// Each entry is a complete Haskell definition (may be multi-line).
    #[serde(default)]
    pub helpers: Vec<String>,
}

// ---------------------------------------------------------------------------
// Templating
// ---------------------------------------------------------------------------

fn build_preamble(effects: &[EffectDecl]) -> String {
    let mut out = String::new();
    out.push_str("{-# LANGUAGE DataKinds, TypeOperators, FlexibleContexts, GADTs, PartialTypeSignatures, ScopedTypeVariables #-}\n");
    out.push_str("module Expr where\n");
    out.push_str("import Prelude hiding (reverse, splitAt, span, break, init, words, lines, unlines, unwords, concatMap, dropWhile)\n");
    out.push_str("import Control.Monad.Freer\n");
    out.push_str("import Tidepool.Prelude\n");
    out.push('\n');

    for eff in effects {
        out.push_str(&format!("data {} a where\n", eff.type_name));
        for ctor in eff.constructors {
            out.push_str(&format!("  {}\n", ctor));
        }
        out.push('\n');
    }

    out
}

fn build_effect_stack_type(effects: &[EffectDecl]) -> String {
    if effects.is_empty() {
        "'[]".to_string()
    } else {
        let names: Vec<&str> = effects.iter().map(|e| e.type_name).collect();
        format!("'[{}]", names.join(", "))
    }
}

fn build_eval_tool_description(effects: &[EffectDecl]) -> String {
    let mut desc = String::from(concat!(
        "Provide do-notation lines in `source`; the server wraps them in a Haskell ",
        "module with the effect stack, pragmas, and imports. ",
        "Use `pure x` as the last line to return a value. ",
        "Use `send (Constructor args)` to invoke effects. ",
        "First call is slow (~2s). Subsequent calls are cached.\n",
        "Return values are automatically rendered to JSON by the Rust runtime — ",
        "Int becomes a number, [Char] becomes a string, Bool becomes true/false, ",
        "lists become arrays, etc. Prefer `pure x` over `send (Print (show x))` ",
        "for returning results.",
    ));

    if !effects.is_empty() {
        desc.push_str("\nAvailable effects (use `send` to invoke):\n");
        for eff in effects {
            desc.push_str(&format!("\n{}: {}\n", eff.type_name, eff.description));
            for ctor in eff.constructors {
                desc.push_str(&format!("  {}\n", ctor));
            }
        }
    }

    desc
}

fn template_haskell(
    preamble: &str,
    effect_stack: &str,
    source: &[String],
    imports: &[String],
    helpers: &[String],
) -> String {
    let mut out = String::new();

    out.push_str(preamble);

    for imp in imports {
        out.push_str(&format!("import {}\n", imp));
    }
    if !imports.is_empty() {
        out.push('\n');
    }

    for helper in helpers {
        out.push_str(helper);
        out.push('\n');
    }
    if !helpers.is_empty() {
        out.push('\n');
    }

    out.push_str(&format!("result :: Eff {} _\n", effect_stack));
    out.push_str("result = do\n");
    for line in source {
        out.push_str(&format!("  {}\n", line));
    }

    out
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
    format!(
        "## {}\n{}\n\n## Compiled Source\n```haskell\n{}\n```",
        title, error, source
    )
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
        self.lines.lock().unwrap().push(line);
    }

    /// Drain all captured lines, returning them and clearing the buffer.
    pub fn drain(&self) -> Vec<String> {
        let mut lines = self.lines.lock().unwrap();
        std::mem::take(&mut *lines)
    }
}

// ---------------------------------------------------------------------------
// Server internals
// ---------------------------------------------------------------------------

/// Trait combining effect dispatch with cloning for the MCP server.
pub trait McpEffectHandler: DispatchEffect<CapturedOutput> + DynClone + Send + Sync + 'static {}
clone_trait_object!(McpEffectHandler);

impl<T> McpEffectHandler for T where T: DispatchEffect<CapturedOutput> + Clone + Send + Sync + 'static {}

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
}

struct HandlerWrapper<'a>(&'a mut dyn McpEffectHandler);

impl<'a> DispatchEffect<CapturedOutput> for HandlerWrapper<'a> {
    fn dispatch(
        &mut self,
        tag: u64,
        request: &tidepool_eval::value::Value,
        cx: &tidepool_effect::dispatch::EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_eval::value::Value, tidepool_effect::error::EffectError> {
        self.0.dispatch(tag, request, cx)
    }
}

impl TidepoolMcpServerImpl {
    async fn eval(&self, req: EvalRequest) -> Result<CallToolResult, McpError> {
        tracing::info!(lines = req.source.len(), "eval request");
        let source: Arc<str> = template_haskell(
            &self.haskell_preamble,
            &self.effect_stack_type,
            &req.source,
            &req.imports,
            &req.helpers,
        )
        .into();

        let mut handlers = dyn_clone::clone_box(&*self.handler_factory);
        let include_refs: Vec<PathBuf> = self.include.clone();
        let source_for_blocking = Arc::clone(&source);
        let captured = CapturedOutput::new();
        let captured_for_blocking = captured.clone();

        let result = std::thread::Builder::new()
            .name("tidepool-eval".into())
            .stack_size(8 * 1024 * 1024) // 8 MiB — JIT compilation + execution needs headroom
            .spawn(move || {
                let include_paths: Vec<&Path> = include_refs.iter().map(|p| p.as_path()).collect();
                let mut wrapper = HandlerWrapper(handlers.as_mut());
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    tidepool_runtime::compile_and_run(
                        &source_for_blocking,
                        "result",
                        &include_paths,
                        &mut wrapper,
                        &captured_for_blocking,
                    )
                }))
            })
            .map_err(|e| McpError::internal_error(format!("thread spawn error: {}", e), None))?
            .join()
            .map_err(|_| McpError::internal_error("eval thread panicked", None))?;

        let output_lines = captured.drain();

        match result {
            Ok(Ok(eval_result)) => {
                tracing::info!("eval succeeded");
                let mut response = String::new();
                if !output_lines.is_empty() {
                    response.push_str("## Output\n");
                    for line in &output_lines {
                        response.push_str(line);
                        response.push('\n');
                    }
                    response.push_str("\n## Result\n");
                }
                response.push_str(&eval_result.to_string_pretty());
                Ok(CallToolResult::success(vec![Content::text(response)]))
            }
            Ok(Err(e)) => {
                let error_msg = format_error_with_source("Error", &e.to_string(), &source);
                tracing::error!("eval failed: {}", e);
                Ok(CallToolResult::error(vec![Content::text(error_msg)]))
            }
            Err(panic_payload) => {
                let panic_msg = format_panic_payload(panic_payload);
                let error_msg = format_error_with_source(
                    "Error",
                    &format!("Internal panic: {}", panic_msg),
                    &source,
                );
                tracing::error!("eval panicked: {}", panic_msg);
                Ok(CallToolResult::error(vec![Content::text(error_msg)]))
            }
        }
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
                let req: EvalRequest =
                    serde_json::from_value(serde_json::Value::Object(args)).map_err(|e| {
                        McpError::invalid_params(format!("invalid params: {}", e), None)
                    })?;
                self.eval(req).await
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
        let decls = H::collect_decls();
        Self {
            inner: TidepoolMcpServerImpl {
                handler_factory: Arc::new(handler),
                include: Vec::new(),
                haskell_preamble: build_preamble(&decls),
                effect_stack_type: build_effect_stack_type(&decls),
                eval_tool_description: build_eval_tool_description(&decls),
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
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eval_request_defaults() {
        let json = serde_json::json!({"source": ["pure 42"]});
        let req: EvalRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.source, vec!["pure 42"]);
        assert!(req.imports.is_empty());
        assert!(req.helpers.is_empty());
    }

    #[test]
    fn test_build_preamble() {
        let effects = vec![
            EffectDecl {
                type_name: "Console",
                description: "Print output",
                constructors: &["Print :: String -> Console ()"],
            },
            EffectDecl {
                type_name: "KV",
                description: "Key-value store",
                constructors: &[
                    "KvGet :: String -> KV (Maybe String)",
                    "KvSet :: String -> String -> KV ()",
                ],
            },
        ];
        let preamble = build_preamble(&effects);
        assert!(preamble.contains("data Console a where"));
        assert!(preamble.contains("  Print :: String -> Console ()"));
        assert!(preamble.contains("data KV a where"));
    }

    #[test]
    fn test_build_effect_stack_type() {
        let effects = vec![
            EffectDecl { type_name: "Console", description: "", constructors: &[] },
            EffectDecl { type_name: "KV", description: "", constructors: &[] },
            EffectDecl { type_name: "Fs", description: "", constructors: &[] },
        ];
        assert_eq!(build_effect_stack_type(&effects), "'[Console, KV, Fs]");
        assert_eq!(build_effect_stack_type(&[]), "'[]");
    }

    #[test]
    fn test_template_haskell() {
        let effects = vec![EffectDecl {
            type_name: "Console",
            description: "",
            constructors: &["Print :: String -> Console ()"],
        }];
        let preamble = build_preamble(&effects);
        let stack = build_effect_stack_type(&effects);
        let source = vec!["let x = 42".into(), "pure x".into()];

        let result = template_haskell(&preamble, &stack, &source, &[], &[]);

        assert!(result.contains("module Expr where"));
        assert!(result.contains("import Control.Monad.Freer"));
        assert!(result.contains("data Console a where"));
        assert!(result.contains("result :: Eff '[Console] _"));
        assert!(result.contains("result = do"));
        assert!(result.contains("  let x = 42"));
        assert!(result.contains("  pure x"));
    }

    #[test]
    fn test_eval_tool_description_includes_effects() {
        let effects = vec![EffectDecl {
            type_name: "Console",
            description: "Print to console",
            constructors: &["Print :: String -> Console ()"],
        }];
        let desc = build_eval_tool_description(&effects);
        assert!(desc.contains("Console: Print to console"));
        assert!(desc.contains("Print :: String -> Console ()"));
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
        let source = "main = pure ()";
        let formatted = format_error_with_source(title, error, source);

        assert!(formatted.contains("## Error"));
        assert!(formatted.contains("Type mismatch"));
        assert!(formatted.contains("## Compiled Source"));
        assert!(formatted.contains("```haskell\nmain = pure ()\n```"));
    }
}
