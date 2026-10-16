//! Actor-owned notebook requests and authenticated checkpoint coordinates.

use tidepool_runtime::session::WorkbenchExecutionId;

/// Exact invocation identity used to settle captured context checkpoints.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContextCheckpointBoundary {
    Hosted(exomonad_tool::OriginalOperation),
    Route {
        actor_id: u64,
        incarnation: u64,
        watch_id: u64,
    },
    Execution {
        actor_id: u64,
        incarnation: u64,
        execution_id: WorkbenchExecutionId,
    },
}

impl ContextCheckpointBoundary {
    #[must_use]
    pub fn external(thread_id: String, request_id: String, call_id: String) -> Self {
        Self::Hosted(exomonad_tool::OriginalOperation {
            origin: exomonad_tool::ConversationOrigin::External { thread_id },
            request_id,
            call_id,
        })
    }

    #[must_use]
    pub fn hosted(&self) -> Option<&exomonad_tool::OriginalOperation> {
        match self {
            Self::Hosted(operation) => Some(operation),
            Self::Route { .. } | Self::Execution { .. } => None,
        }
    }

    #[must_use]
    pub fn is_complete(&self) -> bool {
        match self {
            Self::Hosted(operation) => operation.is_complete(),
            Self::Route { .. } | Self::Execution { .. } => true,
        }
    }
}

/// One ordered request against a persistent Haskell workbench.
///
/// MCP, provider-native fenced execution, tests, and other frontends share
/// item sequencing and input mounting through this transport-neutral value.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkbenchRequest {
    /// Compiler-prepared cell items or one trusted hosted tool invocation.
    pub items: Vec<String>,
    /// Optional structured payload mounted as @input :: Aeson.Value@.
    pub input: Option<serde_json::Value>,
    /// Request the frontend's expanded diagnostic receipt when supported.
    pub verbose: Option<bool>,
    /// Runtime-minted identity for one exact hosted tool call.
    ///
    /// This is not accepted from JSON. The transport owner derives it from
    /// authenticated call coordinates before actor dispatch, allowing the
    /// actor to return a committed receipt when that exact call is retried.
    execution_id: Option<WorkbenchExecutionId>,
    /// Trusted transport coordinates; never accepted from authored JSON.
    checkpoint_boundary: Option<ContextCheckpointBoundary>,
    /// Trusted named-handler selection; arguments are data, never Haskell source.
    tool_call: Option<WorkbenchToolCall>,
    /// Raw notebook cell awaiting GHC split/classify/preflight in the owning
    /// actor session.
    cell_source: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkbenchToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

impl WorkbenchRequest {
    /// One host-owned invocation of already installed code. Its owner supplies
    /// the entry separately; the label is diagnostic data, never authored source.
    pub fn for_retained_operation(label: &str, input: serde_json::Value) -> Self {
        Self {
            items: vec![label.to_owned()],
            input: Some(input),
            verbose: None,
            execution_id: None,
            checkpoint_boundary: None,
            tool_call: None,
            cell_source: None,
        }
    }
    pub fn for_tool(name: String, arguments: serde_json::Value) -> Self {
        Self {
            items: vec![name.clone()],
            input: None,
            verbose: None,
            execution_id: None,
            checkpoint_boundary: None,
            tool_call: Some(WorkbenchToolCall { name, arguments }),
            cell_source: None,
        }
    }

    pub fn tool_call(&self) -> Option<&WorkbenchToolCall> {
        self.tool_call.as_ref()
    }

    #[must_use]
    pub fn from_cell_input(source: &str) -> Self {
        Self {
            items: Vec::new(),
            input: None,
            verbose: None,
            execution_id: None,
            checkpoint_boundary: None,
            tool_call: None,
            cell_source: Some(source.to_owned()),
        }
    }

    #[must_use]
    pub fn cell_source(&self) -> Option<&str> {
        self.cell_source.as_deref()
    }

    pub fn install_cell_items(&mut self, items: Vec<String>) {
        self.items = items;
        self.cell_source = None;
    }

    #[must_use]
    pub fn with_execution_id(mut self, execution_id: WorkbenchExecutionId) -> Self {
        self.execution_id = Some(execution_id);
        self
    }

    #[must_use]
    pub fn execution_id(&self) -> Option<&WorkbenchExecutionId> {
        self.execution_id.as_ref()
    }

    #[must_use]
    pub fn with_checkpoint_boundary(mut self, boundary: ContextCheckpointBoundary) -> Self {
        self.checkpoint_boundary = Some(boundary);
        self
    }

    #[must_use]
    pub fn checkpoint_boundary(&self) -> Option<&ContextCheckpointBoundary> {
        self.checkpoint_boundary.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_source_and_tool_arguments_have_distinct_replay_identity() {
        let cell = WorkbenchRequest::from_cell_input("pure ()");
        assert!(cell.items.is_empty());
        assert_eq!(cell.cell_source(), Some("pure ()"));
        assert!(cell.tool_call().is_none());
        assert!(cell.execution_id().is_none());
        assert!(cell.checkpoint_boundary().is_none());
        assert_ne!(cell, WorkbenchRequest::from_cell_input("pure 1"));
        assert_ne!(
            cell,
            WorkbenchRequest::for_tool("pure ()".into(), serde_json::Value::Null)
        );
    }

    #[test]
    fn named_tool_arguments_are_data_and_part_of_replay_identity() {
        let script = "cat <<'EOF'\n\":} [bash| λ |]\"\nEOF\n";
        let request = WorkbenchRequest::for_tool("bash".into(), script.into());
        assert_eq!(request.items, ["bash"]);
        assert_eq!(request.tool_call().unwrap().arguments, script);
        assert_eq!(request, request.clone());
        assert_ne!(
            request,
            WorkbenchRequest::for_tool("bash".into(), "other".into())
        );
        assert_ne!(
            request,
            WorkbenchRequest::for_tool("other".into(), script.into())
        );
    }
}
