//! Transport-neutral model tool contracts shared by actor policies and their
//! concrete host projections.

pub mod surface;

/// One model-visible tool declaration, independent of who serves it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDeclaration {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    pub kind: ToolKind,
}

/// One raw-text tool whose concrete host transport supplies no argument
/// object or schema wrapper.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomToolDeclaration {
    pub name: String,
    pub description: String,
}

/// One model-visible resident tool, independent of the protocol that exposes
/// it to an attached agent application.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostedTool {
    Custom(CustomToolDeclaration),
    Function(ToolDeclaration),
}

impl From<ToolDeclaration> for HostedTool {
    fn from(declaration: ToolDeclaration) -> Self {
        if declaration.kind == ToolKind::Raw {
            Self::Custom(CustomToolDeclaration {
                name: declaration.name,
                description: declaration.description,
            })
        } else {
            Self::Function(declaration)
        }
    }
}

impl HostedTool {
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Custom(tool) => &tool.name,
            Self::Function(tool) => &tool.name,
        }
    }

    #[must_use]
    pub fn description(&self) -> &str {
        match self {
            Self::Custom(tool) => &tool.description,
            Self::Function(tool) => &tool.description,
        }
    }

    #[must_use]
    pub fn accepts(&self, arguments: &ToolArguments) -> bool {
        matches!(
            (self, arguments),
            (Self::Custom(_), ToolArguments::Raw(_))
                | (
                    Self::Function(_),
                    ToolArguments::Structured(serde_json::Value::Object(_))
                )
        )
    }
}

/// Arguments decoded according to the registered resident tool kind.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolArguments {
    Raw(String),
    Structured(serde_json::Value),
}

/// Conversation identity supplied by the owning host transport.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConversationOrigin {
    Embedded {
        run: String,
        actor: String,
        incarnation: String,
    },
    External {
        thread_id: String,
    },
}

/// The original model operation, independent of any nested local invocation.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct OriginalOperation {
    pub origin: ConversationOrigin,
    pub request_id: String,
    pub call_id: String,
}

impl OriginalOperation {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        let origin_complete = match &self.origin {
            ConversationOrigin::Embedded {
                run,
                actor,
                incarnation,
            } => !run.is_empty() && !actor.is_empty() && !incarnation.is_empty(),
            ConversationOrigin::External { thread_id } => !thread_id.is_empty(),
        };
        origin_complete && !self.request_id.is_empty() && !self.call_id.is_empty()
    }

    #[must_use]
    pub fn external_thread(&self) -> Option<&str> {
        match &self.origin {
            ConversationOrigin::External { thread_id } => Some(thread_id),
            ConversationOrigin::Embedded { .. } => None,
        }
    }
}

/// A direct invocation has correlation but no enclosing model operation to release.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(tag = "kind", content = "identity", rename_all = "snake_case")]
pub enum ToolInvocationOrigin {
    Model(OriginalOperation),
    Direct {
        origin: ConversationOrigin,
        request_id: String,
    },
}

/// Original operation and nested invocation identity are retained separately.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ToolInvocationContext {
    pub origin: ToolInvocationOrigin,
    pub call_id: String,
    pub namespace: Option<String>,
}

impl ToolInvocationContext {
    #[must_use]
    pub fn external(
        thread_id: String,
        request_id: String,
        call_id: String,
        context_call_id: Option<String>,
        namespace: Option<String>,
    ) -> Self {
        let origin = ConversationOrigin::External { thread_id };
        let origin = match context_call_id {
            Some(call_id) => ToolInvocationOrigin::Model(OriginalOperation {
                origin,
                request_id,
                call_id,
            }),
            None => ToolInvocationOrigin::Direct { origin, request_id },
        };
        Self {
            origin,
            call_id,
            namespace,
        }
    }

    #[must_use]
    pub fn model_operation(&self) -> Option<&OriginalOperation> {
        match &self.origin {
            ToolInvocationOrigin::Model(operation) => Some(operation),
            ToolInvocationOrigin::Direct { .. } => None,
        }
    }

    #[must_use]
    pub fn request_id(&self) -> &str {
        match &self.origin {
            ToolInvocationOrigin::Model(operation) => &operation.request_id,
            ToolInvocationOrigin::Direct { request_id, .. } => request_id,
        }
    }
}

/// One validated invocation of an actor's resident tool surface.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolInvocation {
    pub context: Option<ToolInvocationContext>,
    pub name: String,
    pub arguments: ToolArguments,
}

/// Actor-policy meaning retained from the Haskell endpoint algebra.
///
/// This class describes resident policy control flow. In particular, `Call`
/// is not an MCP `readOnlyHint`: a call handler may still invoke effects that
/// mutate external state.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    #[default]
    Call,
    Raw,
    Notify,
    Update,
    Finish,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_shape_uses_the_model_facing_schema_name() {
        let declaration = ToolDeclaration {
            name: "status".into(),
            description: "Read status".into(),
            input_schema: serde_json::json!({"type": "object"}),
            output_schema: Some(serde_json::json!({"type": "object"})),
            kind: ToolKind::Call,
        };
        let wire = serde_json::to_value(&declaration).expect("serialize declaration");
        assert_eq!(wire["inputSchema"], serde_json::json!({"type": "object"}));
        assert!(wire.get("input_schema").is_none());
        assert_eq!(wire["kind"], "call");
        assert_eq!(
            serde_json::from_value::<ToolDeclaration>(wire).expect("decode declaration"),
            declaration
        );
    }
}
