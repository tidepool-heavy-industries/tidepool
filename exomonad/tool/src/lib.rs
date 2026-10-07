//! Transport-neutral model tool contracts shared by actor policies and their
//! concrete host projections.

mod effects;
#[path = "generated/public_profiles.rs"]
mod public_profiles;
pub mod surface;
pub use effects::{ActorEffectKey, ToolEffectKey};
pub use public_profiles::{PublicActorEffectRow, PublicActorProfile};

/// When this invocation must settle relative to the caller's next inference.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolScheduling {
    #[default]
    Async,
    BeforeNextInference,
}

/// The installed implementation that receives this tool's input.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolImplementation {
    #[default]
    ResidentHandler,
    HaskellCell,
}

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
    #[serde(default)]
    pub schedule: ToolScheduling,
    #[serde(default)]
    pub implementation: ToolImplementation,
    #[serde(default)]
    pub effect_keys: Vec<ToolEffectKey>,
}

/// One raw-text tool whose concrete host transport supplies no argument
/// object or schema wrapper.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomToolDeclaration {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub schedule: ToolScheduling,
    #[serde(default)]
    pub implementation: ToolImplementation,
    #[serde(default)]
    pub effect_keys: Vec<ToolEffectKey>,
}

/// One model-visible resident tool, independent of the protocol that exposes
/// it to an attached agent application.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostedTool {
    Custom(CustomToolDeclaration),
    Function(ToolDeclaration),
}

/// A declaration that cannot be represented by the model-facing hosted
/// invocation contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolDeclarationError {
    FunctionInputMustBeObject { name: String },
}

impl std::fmt::Display for ToolDeclarationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FunctionInputMustBeObject { name } => write!(
                formatter,
                "function tool {name:?} must declare an object input schema"
            ),
        }
    }
}

impl std::error::Error for ToolDeclarationError {}

impl TryFrom<ToolDeclaration> for HostedTool {
    type Error = ToolDeclarationError;

    fn try_from(declaration: ToolDeclaration) -> Result<Self, Self::Error> {
        if declaration.kind != ToolKind::Raw
            && declaration
                .input_schema
                .get("type")
                .and_then(serde_json::Value::as_str)
                != Some("object")
        {
            return Err(ToolDeclarationError::FunctionInputMustBeObject {
                name: declaration.name,
            });
        }
        if declaration.kind == ToolKind::Raw {
            Ok(Self::Custom(CustomToolDeclaration {
                name: declaration.name,
                description: declaration.description,
                schedule: declaration.schedule,
                implementation: declaration.implementation,
                effect_keys: declaration.effect_keys,
            }))
        } else {
            Ok(Self::Function(declaration))
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
    pub fn scheduling(&self) -> ToolScheduling {
        match self {
            Self::Custom(tool) => tool.schedule,
            Self::Function(tool) => tool.schedule,
        }
    }

    #[must_use]
    pub fn implementation(&self) -> ToolImplementation {
        match self {
            Self::Custom(tool) => tool.implementation,
            Self::Function(tool) => tool.implementation,
        }
    }

    #[must_use]
    pub fn effect_keys(&self) -> &[ToolEffectKey] {
        match self {
            Self::Custom(tool) => &tool.effect_keys,
            Self::Function(tool) => &tool.effect_keys,
        }
    }

    #[must_use]
    pub fn accepts(&self, arguments: &ToolArguments) -> bool {
        match self {
            Self::Custom(_) => tool_arguments_match_kind(ToolKind::Raw, arguments),
            Self::Function(_) => tool_arguments_match_kind(ToolKind::Call, arguments),
        }
    }
}

impl ToolDeclaration {
    /// Whether an invocation has the argument shape this declaration kind
    /// can receive. Schema validity is checked when the declaration is
    /// registered.
    #[must_use]
    pub fn accepts_arguments(&self, arguments: &ToolArguments) -> bool {
        tool_arguments_match_kind(self.kind, arguments)
    }
}

fn tool_arguments_match_kind(kind: ToolKind, arguments: &ToolArguments) -> bool {
    match kind {
        ToolKind::Raw => matches!(arguments, ToolArguments::Raw(_)),
        _ => matches!(
            arguments,
            ToolArguments::Structured(serde_json::Value::Object(_))
        ),
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

    fn declaration(kind: ToolKind, input_schema: serde_json::Value) -> ToolDeclaration {
        ToolDeclaration {
            schedule: Default::default(),
            implementation: Default::default(),
            effect_keys: Vec::new(),
            name: "probe".into(),
            description: "Probe a model-facing tool surface".into(),
            input_schema,
            output_schema: None,
            kind,
        }
    }

    #[test]
    fn hosted_function_declarations_require_object_input_schemas() {
        for input_schema in [
            serde_json::json!({}),
            serde_json::json!({"type": "null"}),
            serde_json::json!({"type": "string"}),
            serde_json::json!({"type": "array", "items": {"type": "string"}}),
        ] {
            assert_eq!(
                HostedTool::try_from(declaration(ToolKind::Call, input_schema)),
                Err(ToolDeclarationError::FunctionInputMustBeObject {
                    name: "probe".into(),
                })
            );
        }
    }

    #[test]
    fn hosted_declarations_keep_object_functions_and_raw_text_inputs() {
        let object_declaration = declaration(
            ToolKind::Update,
            serde_json::json!({"type": "object", "properties": {}}),
        );
        let structured = ToolArguments::Structured(serde_json::json!({}));
        assert!(object_declaration.accepts_arguments(&structured));
        let object = HostedTool::try_from(object_declaration).expect("object function declaration");
        assert!(matches!(object, HostedTool::Function(_)));
        assert!(object.accepts(&structured));

        let raw_declaration = declaration(ToolKind::Raw, serde_json::json!({"type": "string"}));
        let text = ToolArguments::Raw("literal".into());
        assert!(raw_declaration.accepts_arguments(&text));
        let raw = HostedTool::try_from(raw_declaration).expect("raw text declaration");
        assert!(matches!(raw, HostedTool::Custom(_)));
        assert!(raw.accepts(&text));
    }

    #[test]
    fn raw_projection_preserves_invocation_contract_and_rejects_unknown_effects() {
        let mut declaration = declaration(ToolKind::Raw, serde_json::json!({"type": "string"}));
        declaration.schedule = ToolScheduling::BeforeNextInference;
        declaration.implementation = ToolImplementation::HaskellCell;
        declaration.effect_keys = vec![ToolEffectKey::ContextReadWrite, ActorEffectKey::Jev.into()];
        let wire = serde_json::to_value(&declaration).unwrap();
        assert_eq!(wire["schedule"], "before_next_inference");
        assert_eq!(wire["implementation"], "haskell_cell");
        assert_eq!(
            wire["effectKeys"],
            serde_json::json!(["ContextReadWrite", "Jev"])
        );
        let tool =
            HostedTool::try_from(serde_json::from_value::<ToolDeclaration>(wire.clone()).unwrap())
                .unwrap();
        assert_eq!(tool.scheduling(), declaration.schedule);
        assert_eq!(tool.implementation(), declaration.implementation);
        assert_eq!(tool.effect_keys(), declaration.effect_keys);
        let mut unknown = wire;
        unknown["effectKeys"] = serde_json::json!(["UnregisteredEffect"]);
        assert!(serde_json::from_value::<ToolDeclaration>(unknown).is_err());
        assert!(
            serde_json::from_value::<ActorEffectKey>(serde_json::json!("ContextReadWrite"))
                .is_err()
        );
    }

    #[test]
    fn wire_shape_uses_the_model_facing_schema_name() {
        let declaration = ToolDeclaration {
            schedule: Default::default(),
            implementation: Default::default(),
            effect_keys: Vec::new(),
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
