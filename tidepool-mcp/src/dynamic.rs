//! Generic MCP service over one typed tool declaration set and live dispatcher.
//!
//! This module owns MCP projection only. Actor admission, execution principals,
//! and Haskell closure invocation belong in the adapter that supplies the
//! dispatcher.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rmcp::{model::*, service::RequestContext, ErrorData as McpError, RoleServer, ServerHandler};
use tidepool_tool::{ToolDeclaration, ToolKind};

pub type ToolDispatchFuture =
    Pin<Box<dyn Future<Output = Result<serde_json::Value, ToolDispatchError>> + Send + 'static>>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolDispatchError {
    #[error("tool call refused: {0}")]
    Refused(String),
    #[error("tool execution failed: {0}")]
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DynamicMcpError {
    #[error("duplicate MCP tool name {0:?}")]
    DuplicateName(String),
    #[error("invalid MCP tool name {0:?}")]
    InvalidName(String),
    #[error("MCP tool {name:?} has a non-object input schema")]
    InvalidInputSchema { name: String },
    #[error("MCP tool {name:?} has a non-object output schema")]
    InvalidOutputSchema { name: String },
}

type Dispatcher = dyn Fn(String, serde_json::Value) -> ToolDispatchFuture + Send + Sync + 'static;

/// Cloneable MCP server for one immutable actor-policy installation.
#[derive(Clone)]
pub struct DynamicMcpServer {
    declarations: Arc<[ToolDeclaration]>,
    dispatcher: Arc<Dispatcher>,
    instructions: Option<String>,
}

impl DynamicMcpServer {
    pub fn new<F>(
        declarations: Vec<ToolDeclaration>,
        instructions: Option<String>,
        dispatcher: F,
    ) -> Result<Self, DynamicMcpError>
    where
        F: Fn(String, serde_json::Value) -> ToolDispatchFuture + Send + Sync + 'static,
    {
        validate_declarations(&declarations)?;
        Ok(Self {
            declarations: declarations.into(),
            dispatcher: Arc::new(dispatcher),
            instructions,
        })
    }

    /// Project one installed resident Haskell policy into MCP. Actor
    /// admission and continuation ownership remain inside the policy; this
    /// adapter owns only MCP declaration and result shapes.
    pub fn from_resident_policy(
        policy: Arc<tidepool_actor::ResidentMcpPolicy>,
    ) -> Result<Self, DynamicMcpError> {
        let declarations = policy.declarations().to_vec();
        let instructions = policy.instructions().map(str::to_owned);
        Self::new(declarations, instructions, move |name, arguments| {
            let policy = Arc::clone(&policy);
            Box::pin(async move {
                policy
                    .dispatch(name, arguments)
                    .await
                    .map_err(|error| ToolDispatchError::Failed(error.to_string()))
            })
        })
    }

    pub fn declarations(&self) -> &[ToolDeclaration] {
        &self.declarations
    }

    pub async fn dispatch_tool(
        &self,
        name: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<CallToolResult, McpError> {
        if !self
            .declarations
            .iter()
            .any(|declaration| declaration.name == name)
        {
            return Err(McpError {
                code: ErrorCode::METHOD_NOT_FOUND,
                message: format!("Tool not found: {name}").into(),
                data: None,
            });
        }
        match (self.dispatcher)(name.to_string(), serde_json::Value::Object(arguments)).await {
            Ok(value) => Ok(CallToolResult::structured(value)),
            Err(error) => Ok(CallToolResult::error(vec![Content::text(
                error.to_string(),
            )])),
        }
    }

    fn projected_tools(&self) -> Vec<Tool> {
        self.declarations
            .iter()
            .map(|declaration| {
                let schema = match &declaration.input_schema {
                    serde_json::Value::Object(schema) => Arc::new(schema.clone()),
                    _ => unreachable!("validated by DynamicMcpServer::new"),
                };
                let description = match declaration.kind {
                    ToolKind::Call => declaration.description.clone(),
                    ToolKind::Notify => format!(
                        "{} This notification does not return a domain result.",
                        declaration.description
                    ),
                    ToolKind::Update => format!(
                        "{} A successful call replaces this actor's resident policy state.",
                        declaration.description
                    ),
                    ToolKind::Finish => format!(
                        "{} A successful call replies and then completes this actor.",
                        declaration.description
                    ),
                };
                let mut tool =
                    crate::server_common::make_tool(&declaration.name, &description, schema);
                let title = match declaration.kind {
                    ToolKind::Call => "Call",
                    ToolKind::Notify => "Notify",
                    ToolKind::Update => "Update actor state",
                    ToolKind::Finish => "Finish actor",
                };
                tool.title = Some(title.into());
                tool.annotations = match declaration.kind {
                    // A Call handler may still perform effects, so its
                    // environmental mutability is deliberately unknown.
                    ToolKind::Call => None,
                    ToolKind::Notify | ToolKind::Update | ToolKind::Finish => {
                        Some(ToolAnnotations::with_title(title).read_only(false))
                    }
                };
                tool.output_schema = declaration
                    .output_schema
                    .as_ref()
                    .and_then(|schema| schema.as_object().map(|object| Arc::new(object.clone())));
                tool
            })
            .collect()
    }
}

impl ServerHandler for DynamicMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: self.instructions.clone(),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch_tool(&request.name, request.arguments.unwrap_or_default())
            .await
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult {
            tools: self.projected_tools(),
            next_cursor: None,
            meta: None,
        })
    }
}

fn validate_declarations(declarations: &[ToolDeclaration]) -> Result<(), DynamicMcpError> {
    let mut names = HashSet::new();
    for declaration in declarations {
        if !valid_tool_name(&declaration.name) {
            return Err(DynamicMcpError::InvalidName(declaration.name.clone()));
        }
        if !names.insert(&declaration.name) {
            return Err(DynamicMcpError::DuplicateName(declaration.name.clone()));
        }
        if !declaration.input_schema.is_object() {
            return Err(DynamicMcpError::InvalidInputSchema {
                name: declaration.name.clone(),
            });
        }
        if declaration
            .output_schema
            .as_ref()
            .is_some_and(|schema| !schema.is_object())
        {
            return Err(DynamicMcpError::InvalidOutputSchema {
                name: declaration.name.clone(),
            });
        }
    }
    Ok(())
}

fn valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn declaration(name: &str) -> ToolDeclaration {
        ToolDeclaration {
            name: name.to_string(),
            description: format!("Run {name}"),
            input_schema: serde_json::json!({"type": "object"}),
            output_schema: Some(serde_json::json!({"type": "object"})),
            kind: ToolKind::Call,
        }
    }

    #[tokio::test]
    async fn declared_call_reaches_the_one_dispatcher_and_returns_structured_data() {
        let calls = Arc::new(AtomicUsize::new(0));
        let server = DynamicMcpServer::new(
            vec![declaration("actor_status")],
            Some("Actor control".to_string()),
            {
                let calls = Arc::clone(&calls);
                move |name, arguments| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async move {
                        Ok(serde_json::json!({"tool": name, "arguments": arguments}))
                    })
                }
            },
        )
        .unwrap();
        let result = server
            .dispatch_tool("actor_status", serde_json::Map::new())
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            result.structured_content,
            Some(serde_json::json!({"tool": "actor_status", "arguments": {}}))
        );
        assert_eq!(server.projected_tools()[0].name, "actor_status");
        assert!(
            server.projected_tools()[0].annotations.is_none(),
            "a request/response endpoint is not necessarily read-only"
        );
        assert_eq!(
            server.projected_tools()[0].output_schema.as_deref(),
            Some(&serde_json::Map::from_iter([(
                "type".into(),
                serde_json::json!("object")
            )]))
        );
    }

    #[tokio::test]
    async fn unknown_tool_never_reaches_dispatch() {
        let server = DynamicMcpServer::new(vec![declaration("known")], None, |_, _| {
            panic!("unknown tools must be rejected before dispatch")
        })
        .unwrap();
        let error = server
            .dispatch_tool("unknown", serde_json::Map::new())
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::METHOD_NOT_FOUND);
    }

    #[test]
    fn stateful_endpoint_kind_projects_a_truthful_mutability_hint() {
        let mut finish = declaration("finish_work");
        finish.kind = ToolKind::Finish;
        let server = DynamicMcpServer::new(vec![finish], None, |_, _| unreachable!()).unwrap();
        let tool = &server.projected_tools()[0];

        assert_eq!(tool.title.as_deref(), Some("Finish actor"));
        assert_eq!(
            tool.annotations
                .as_ref()
                .and_then(|annotations| annotations.read_only_hint),
            Some(false)
        );
        assert!(tool
            .description
            .as_deref()
            .is_some_and(|description| description.contains("completes this actor")));
    }

    #[test]
    fn malformed_declaration_sets_are_rejected_at_installation() {
        assert!(matches!(
            DynamicMcpServer::new(vec![declaration("bad.name")], None, |_, _| unreachable!()),
            Err(DynamicMcpError::InvalidName(_))
        ));
        assert!(matches!(
            DynamicMcpServer::new(
                vec![declaration("same"), declaration("same")],
                None,
                |_, _| unreachable!()
            ),
            Err(DynamicMcpError::DuplicateName(_))
        ));
    }
}
