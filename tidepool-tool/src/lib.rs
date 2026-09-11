//! Transport-neutral model tool contracts shared by actor policies and their
//! concrete host projections.

#![warn(clippy::unwrap_used, clippy::expect_used)]

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

/// Transport correlation carried unchanged when the host protocol supplies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInvocationContext {
    /// Recorded model invocation owning this execution, when supplied by the host.
    pub context_call_id: Option<String>,
    pub thread_id: String,
    pub turn_id: String,
    pub call_id: String,
    pub namespace: Option<String>,
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
