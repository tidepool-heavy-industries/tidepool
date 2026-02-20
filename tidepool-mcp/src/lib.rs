//! MCP (Model Context Protocol) server library for Tidepool.
//!
//! Wraps `tidepool-runtime` in an MCP server exposing a `run_haskell` tool.
//! Generic over effect handler stacks via `TidepoolMcpServer<H>`.

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

/// Request parameters for the `run_haskell` tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunHaskellRequest {
    /// Haskell source code to compile and execute.
    pub source: String,
    /// Name of the top-level binding to evaluate.
    pub target: String,
}

/// Request parameters for the `compile_haskell` tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CompileHaskellRequest {
    /// Haskell source code to compile (without executing).
    pub source: String,
    /// Name of the top-level binding to target.
    pub target: String,
}

/// Trait combining effect dispatch with cloning for the MCP server.
pub trait McpEffectHandler: DispatchEffect<()> + DynClone + Send + Sync + 'static {}
clone_trait_object!(McpEffectHandler);

impl<T> McpEffectHandler for T where T: DispatchEffect<()> + Clone + Send + Sync + 'static {}

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
}

struct HandlerWrapper<'a>(&'a mut dyn McpEffectHandler);

impl<'a> DispatchEffect<()> for HandlerWrapper<'a> {
    fn dispatch(
        &mut self,
        tag: u64,
        request: &tidepool_eval::value::Value,
        cx: &tidepool_effect::dispatch::EffectContext<'_, ()>,
    ) -> Result<tidepool_eval::value::Value, tidepool_effect::error::EffectError> {
        self.0.dispatch(tag, request, cx)
    }
}

impl TidepoolMcpServerImpl {
    async fn run_haskell(&self, req: RunHaskellRequest) -> Result<CallToolResult, McpError> {
        let mut handlers = dyn_clone::clone_box(&*self.handler_factory);
        let include_refs: Vec<std::path::PathBuf> = self.include.clone();

        let result = tokio::task::spawn_blocking(move || {
            let include_paths: Vec<&Path> = include_refs.iter().map(|p| p.as_path()).collect();
            let mut wrapper = HandlerWrapper(handlers.as_mut());
            tidepool_runtime::compile_and_run(
                &req.source,
                &req.target,
                &include_paths,
                &mut wrapper,
                &(),
            )
        })
        .await
        .map_err(|e| McpError::internal_error(format!("task join error: {}", e), None))?;

        match result {
            Ok(eval_result) => Ok(CallToolResult::success(vec![Content::text(
                eval_result.to_string_pretty(),
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!("{}", e))])),
        }
    }

    async fn compile_haskell_tool(
        &self,
        req: CompileHaskellRequest,
    ) -> Result<CallToolResult, McpError> {
        let include_refs: Vec<PathBuf> = self.include.clone();
        let source = req.source.clone();
        let target = req.target.clone();

        let result = tokio::task::spawn_blocking(move || {
            let include_paths: Vec<&Path> = include_refs.iter().map(|p| p.as_path()).collect();
            tidepool_runtime::compile_haskell(&source, &target, &include_paths)
        })
        .await
        .map_err(|e| McpError::internal_error(format!("task join error: {}", e), None))?;

        match result {
            Ok((expr, table)) => {
                let constructors: Vec<String> = table.iter().map(|dc| dc.name.clone()).collect();
                let info = serde_json::json!({
                    "target": req.target,
                    "expr_nodes": expr.nodes.len(),
                    "constructors": constructors,
                });
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&info).unwrap_or_else(|_| info.to_string()),
                )]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!("{}", e))])),
        }
    }
}

impl ServerHandler for TidepoolMcpServerImpl {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(concat!(
                "Tidepool compiles and runs Haskell via GHC + Cranelift JIT. ",
                "Write standard Haskell (module header required). ",
                "Supported: algebraic data types, pattern matching, let/where, ",
                "Prelude functions (map, filter, null, take, length, ++, ==, ||, &&), ",
                "arithmetic, lists, tuples, Maybe, Either, Bool. ",
                "Returns structured JSON: 42, [1,2,3], true, \"hello\", ",
                "{\"constructor\":\"Person\",\"fields\":[\"Alice\",30]}. ",
                "Effects available at tags 0-2: Console(Print), KV(Get/Set/Delete/Keys), Fs(Read/Write).",
            ).into()),
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
            "run_haskell" => {
                let req: RunHaskellRequest = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| {
                        McpError::invalid_params(format!("invalid params: {}", e), None)
                    })?;
                self.run_haskell(req).await
            }
            "compile_haskell" => {
                let req: CompileHaskellRequest =
                    serde_json::from_value(serde_json::Value::Object(args)).map_err(|e| {
                        McpError::invalid_params(format!("invalid params: {}", e), None)
                    })?;
                self.compile_haskell_tool(req).await
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
        let run_schema = schemars::schema_for!(RunHaskellRequest);
        let run_schema_json = serde_json::to_value(&run_schema).map_err(|e| {
            McpError::internal_error(format!("Failed to serialize schema: {}", e), None)
        })?;
        let run_input_schema = match run_schema_json {
            serde_json::Value::Object(o) => Arc::new(o),
            _ => Arc::new(serde_json::Map::new()),
        };

        let compile_schema = schemars::schema_for!(CompileHaskellRequest);
        let compile_schema_json = serde_json::to_value(&compile_schema).map_err(|e| {
            McpError::internal_error(format!("Failed to serialize schema: {}", e), None)
        })?;
        let compile_input_schema = match compile_schema_json {
            serde_json::Value::Object(o) => Arc::new(o),
            _ => Arc::new(serde_json::Map::new()),
        };

        let tools = vec![
            Tool {
                name: "run_haskell".into(),
                title: None,
                description: Some(
                    concat!(
                        "Compile and run Haskell source code via GHC + Cranelift JIT. ",
                        "Input: Haskell source with 'module X where' header and a target binding name. ",
                        "Output: the evaluated result as structured JSON. ",
                        "Examples: Int->number, Bool->boolean, String->string, [a]->array, Maybe->null/value, ",
                        "custom ADTs->{constructor,fields}. ",
                        "First compilation is slow (~2s, GHC). Subsequent calls are cached.",
                    ).into(),
                ),
                input_schema: run_input_schema,
                output_schema: None,
                annotations: None,
                icons: None,
                meta: None,
                execution: None,
            },
            Tool {
                name: "compile_haskell".into(),
                title: None,
                description: Some(
                    "Compile Haskell source code without executing. Returns metadata: expression node count and available constructors."
                        .into(),
                ),
                input_schema: compile_input_schema,
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

impl<H> TidepoolMcpServer<H>
where
    H: DispatchEffect<()> + Clone + Send + Sync + 'static,
{
    /// Create a new server with the given effect handler stack.
    pub fn new(handler: H) -> Self {
        Self {
            inner: TidepoolMcpServerImpl {
                handler_factory: Arc::new(handler),
                include: Vec::new(),
            },
            _phantom: PhantomData,
        }
    }

    /// Add include paths for Haskell module resolution.
    pub fn with_include(mut self, paths: Vec<PathBuf>) -> Self {
        self.inner.include = paths;
        self
    }

    /// Start the MCP server on stdio transport.
    ///
    /// This starts an MCP server that communicates over the process's standard
    /// input and output streams. It will run until the underlying server shuts
    /// down or an error occurs.
    ///
    /// # Errors
    ///
    /// This function returns an error if:
    ///
    /// - Reading from `stdin` or writing to `stdout` fails (for example, due to
    ///   I/O errors on the standard streams).
    /// - The underlying MCP server fails to start or encounters an error while
    ///   serving requests over stdio.
    /// - There are protocol- or serialization-level errors reported by the
    ///   `rmcp` server implementation while handling MCP messages.
    ///
    /// All such errors are returned as a boxed [`std::error::Error`], and may
    /// originate from `std::io` or from the underlying MCP/transport layer.
    pub async fn serve_stdio(self) -> Result<(), Box<dyn std::error::Error>> {
        self.inner
            .serve((stdin(), stdout()))
            .await?
            .waiting()
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frunk::HNil;

    #[test]
    fn test_run_haskell_request_serialization() {
        let req = RunHaskellRequest {
            source: "main = 42".to_string(),
            target: "main".to_string(),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["source"], "main = 42");
        assert_eq!(json["target"], "main");

        let de: RunHaskellRequest = serde_json::from_value(json).unwrap();
        assert_eq!(de.source, "main = 42");
        assert_eq!(de.target, "main");
    }

    #[test]
    fn test_compile_haskell_request_serialization() {
        let req = CompileHaskellRequest {
            source: "module Test where\nfoo = 42".to_string(),
            target: "foo".to_string(),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["source"], "module Test where\nfoo = 42");
        assert_eq!(json["target"], "foo");

        let de: CompileHaskellRequest = serde_json::from_value(json).unwrap();
        assert_eq!(de.source, "module Test where\nfoo = 42");
        assert_eq!(de.target, "foo");
    }

    #[test]
    fn test_with_include() {
        let server = TidepoolMcpServer::new(HNil);
        let path = PathBuf::from("/tmp/haskell");
        let server = server.with_include(vec![path.clone()]);
        assert_eq!(server.inner.include, vec![path]);
    }
}
