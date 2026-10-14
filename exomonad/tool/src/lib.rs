//! Transport-neutral model tool contracts shared by actor policies and their
//! concrete host projections.

#[path = "generated/effect_keys.rs"]
mod effects;
#[path = "generated/public_profiles.rs"]
mod public_profiles;
pub mod surface;
pub use effects::{ActorEffectKey, HaskellEffectIdentity, ToolEffectKey};
pub use public_profiles::DEFAULT_ACTOR_EFFECTS;

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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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

/// Executable endpoint operations admitted from the external declaration.
/// Scheduling and implementation cannot be changed independently after admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointExecution {
    ResidentAsync(ResidentInput),
    ResidentSync(ResidentInput),
    HaskellAsync,
    HaskellSync,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentInput {
    Raw,
    Call,
    Notify,
}

/// An executable endpoint. External implementation, kind and scheduling fields
/// are consumed at admission; provider declarations are derived projections.
#[derive(Debug, Clone, PartialEq)]
pub struct EndpointPlan {
    name: String,
    description: String,
    input_schema: serde_json::Value,
    output_schema: Option<serde_json::Value>,
    effect_keys: Box<[ToolEffectKey]>,
    execution: EndpointExecution,
}

impl EndpointPlan {
    pub fn admit(declaration: ToolDeclaration) -> Result<Self, ToolDeclarationError> {
        let input = match declaration.kind {
            ToolKind::Raw => ResidentInput::Raw,
            ToolKind::Call => ResidentInput::Call,
            ToolKind::Notify => ResidentInput::Notify,
            ToolKind::Update | ToolKind::Finish => {
                return Err(ToolDeclarationError::UnsupportedEndpoint {
                    name: declaration.name,
                })
            }
        };
        let execution = match (declaration.implementation, declaration.schedule, input) {
            (ToolImplementation::ResidentHandler, ToolScheduling::Async, input) => {
                EndpointExecution::ResidentAsync(input)
            }
            (ToolImplementation::ResidentHandler, ToolScheduling::BeforeNextInference, input) => {
                EndpointExecution::ResidentSync(input)
            }
            (ToolImplementation::HaskellCell, ToolScheduling::Async, ResidentInput::Raw) => {
                EndpointExecution::HaskellAsync
            }
            (
                ToolImplementation::HaskellCell,
                ToolScheduling::BeforeNextInference,
                ResidentInput::Raw,
            ) => EndpointExecution::HaskellSync,
            (ToolImplementation::HaskellCell, _, _) => {
                return Err(ToolDeclarationError::UnsupportedEndpoint {
                    name: declaration.name,
                })
            }
        };
        let mut seen = std::collections::HashSet::new();
        if declaration
            .effect_keys
            .iter()
            .any(|key| key.nominal_identity().is_none() || !seen.insert(*key))
            || (declaration.schedule == ToolScheduling::Async
                && declaration
                    .effect_keys
                    .contains(&ToolEffectKey::ContextReadWrite))
        {
            return Err(ToolDeclarationError::UnsupportedEndpoint {
                name: declaration.name,
            });
        }
        if input != ResidentInput::Raw
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
        Ok(Self {
            name: declaration.name,
            description: declaration.description,
            input_schema: declaration.input_schema,
            output_schema: declaration.output_schema,
            effect_keys: declaration.effect_keys.into(),
            execution,
        })
    }

    pub fn surface(&self) -> HostedTool {
        let implementation = match self.execution {
            EndpointExecution::ResidentAsync(_) | EndpointExecution::ResidentSync(_) => {
                ToolImplementation::ResidentHandler
            }
            EndpointExecution::HaskellAsync | EndpointExecution::HaskellSync => {
                ToolImplementation::HaskellCell
            }
        };
        let kind = match self.execution {
            EndpointExecution::ResidentAsync(ResidentInput::Call)
            | EndpointExecution::ResidentSync(ResidentInput::Call) => ToolKind::Call,
            EndpointExecution::ResidentAsync(ResidentInput::Notify)
            | EndpointExecution::ResidentSync(ResidentInput::Notify) => ToolKind::Notify,
            _ => ToolKind::Raw,
        };
        if kind == ToolKind::Raw {
            HostedTool::Custom(CustomToolDeclaration {
                name: self.name.clone(),
                description: self.description.clone(),
                implementation,
                schedule: self.scheduling(),
                effect_keys: self.effect_keys.to_vec(),
            })
        } else {
            HostedTool::Function(ToolDeclaration {
                name: self.name.clone(),
                description: self.description.clone(),
                input_schema: self.input_schema.clone(),
                output_schema: self.output_schema.clone(),
                kind,
                implementation,
                schedule: self.scheduling(),
                effect_keys: self.effect_keys.to_vec(),
            })
        }
    }
    pub fn execution(&self) -> EndpointExecution {
        self.execution
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn effect_keys(&self) -> &[ToolEffectKey] {
        &self.effect_keys
    }
    pub fn scheduling(&self) -> ToolScheduling {
        match self.execution {
            EndpointExecution::ResidentAsync(_) | EndpointExecution::HaskellAsync => {
                ToolScheduling::Async
            }
            EndpointExecution::ResidentSync(_) | EndpointExecution::HaskellSync => {
                ToolScheduling::BeforeNextInference
            }
        }
    }
    pub fn admit_arguments(
        &self,
        arguments: ToolArguments,
    ) -> Result<EndpointArguments, ToolDeclarationError> {
        match (self.execution, arguments) {
            (
                EndpointExecution::HaskellAsync | EndpointExecution::HaskellSync,
                ToolArguments::Raw(source),
            ) => Ok(EndpointArguments::Haskell(source)),
            (
                EndpointExecution::ResidentAsync(ResidentInput::Raw)
                | EndpointExecution::ResidentSync(ResidentInput::Raw),
                ToolArguments::Raw(text),
            ) => Ok(EndpointArguments::Resident(serde_json::Value::String(text))),
            (
                EndpointExecution::ResidentAsync(ResidentInput::Call | ResidentInput::Notify)
                | EndpointExecution::ResidentSync(ResidentInput::Call | ResidentInput::Notify),
                ToolArguments::Structured(value @ serde_json::Value::Object(_)),
            ) => Ok(EndpointArguments::Resident(value)),
            _ => Err(ToolDeclarationError::InvalidArguments {
                name: self.name().into(),
            }),
        }
    }
}

/// Arguments checked by the selected plan, ready for its interpreter.
pub enum EndpointArguments {
    Resident(serde_json::Value),
    Haskell(String),
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
    UnsupportedEndpoint { name: String },
    InvalidArguments { name: String },
    FunctionInputMustBeObject { name: String },
}

impl std::fmt::Display for ToolDeclarationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedEndpoint { name } => {
                write!(formatter, "unsupported endpoint {name:?}")
            }
            Self::InvalidArguments { name } => {
                write!(formatter, "invalid argument kind for {name:?}")
            }
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
    /// Invoke the original operation itself, rather than a nested local call.
    #[must_use]
    pub fn original(operation: OriginalOperation) -> Self {
        Self {
            call_id: operation.call_id.clone(),
            origin: ToolInvocationOrigin::Model(operation),
            namespace: None,
        }
    }

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
    fn endpoint_algebra_has_exactly_eight_supported_operations() {
        let valid = [
            (
                ToolKind::Raw,
                ToolImplementation::ResidentHandler,
                ToolScheduling::Async,
            ),
            (
                ToolKind::Call,
                ToolImplementation::ResidentHandler,
                ToolScheduling::Async,
            ),
            (
                ToolKind::Notify,
                ToolImplementation::ResidentHandler,
                ToolScheduling::Async,
            ),
            (
                ToolKind::Raw,
                ToolImplementation::ResidentHandler,
                ToolScheduling::BeforeNextInference,
            ),
            (
                ToolKind::Call,
                ToolImplementation::ResidentHandler,
                ToolScheduling::BeforeNextInference,
            ),
            (
                ToolKind::Notify,
                ToolImplementation::ResidentHandler,
                ToolScheduling::BeforeNextInference,
            ),
            (
                ToolKind::Raw,
                ToolImplementation::HaskellCell,
                ToolScheduling::Async,
            ),
            (
                ToolKind::Raw,
                ToolImplementation::HaskellCell,
                ToolScheduling::BeforeNextInference,
            ),
        ];
        for kind in [
            ToolKind::Raw,
            ToolKind::Call,
            ToolKind::Notify,
            ToolKind::Update,
            ToolKind::Finish,
        ] {
            for implementation in [
                ToolImplementation::ResidentHandler,
                ToolImplementation::HaskellCell,
            ] {
                for schedule in [ToolScheduling::Async, ToolScheduling::BeforeNextInference] {
                    let mut raw = declaration(kind, serde_json::json!({"type":"object"}));
                    raw.implementation = implementation;
                    raw.schedule = schedule;
                    assert_eq!(
                        EndpointPlan::admit(raw).is_ok(),
                        valid.contains(&(kind, implementation, schedule))
                    );
                }
            }
        }
    }

    proptest::proptest! {
        #[test]
        fn admitted_endpoint_arguments_follow_the_operation(
            raw_input in proptest::bool::ANY,
            native in proptest::bool::ANY,
            sync in proptest::bool::ANY,
            notify in proptest::bool::ANY,
            text in ".{0,64}",
        ) {
            let kind = if raw_input || native { ToolKind::Raw } else if notify { ToolKind::Notify } else { ToolKind::Call };
            let mut wire = declaration(kind, serde_json::json!({"type":"object"}));
            wire.implementation = if native { ToolImplementation::HaskellCell } else { ToolImplementation::ResidentHandler };
            wire.schedule = if sync { ToolScheduling::BeforeNextInference } else { ToolScheduling::Async };
            let plan = EndpointPlan::admit(wire).unwrap();
            let text_input = ToolArguments::Raw(text.clone());
            let object_input = ToolArguments::Structured(serde_json::json!({"value": text}));
            let expects_text = raw_input || native;
            proptest::prop_assert_eq!(plan.admit_arguments(text_input).is_ok(), expects_text);
            proptest::prop_assert_eq!(plan.admit_arguments(object_input).is_ok(), !expects_text);
            proptest::prop_assert!(plan.admit_arguments(ToolArguments::Structured(serde_json::Value::Null)).is_err());
            proptest::prop_assert_eq!(plan.scheduling() == ToolScheduling::BeforeNextInference, sync);
            proptest::prop_assert_eq!(matches!(plan.execution(), EndpointExecution::HaskellAsync | EndpointExecution::HaskellSync), native);
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

#[cfg(test)]
mod effect_catalogue_tests {
    use super::*;

    #[test]
    fn every_nominal_key_roundtrips_exact_wire_and_module_identity() {
        for key in ToolEffectKey::ALL {
            let encoded = serde_json::to_string(key).unwrap();
            assert_eq!(encoded, format!("\"{}\"", key.wire_name()));
            let decoded: ToolEffectKey = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, *key);
            let identity = key.nominal_identity().unwrap();
            assert_eq!(identity.name, key.haskell_name());
            assert_eq!(Some(identity.module), key.haskell_module());
            assert_eq!(
                key.is_invocation_only(),
                !matches!(key, ToolEffectKey::Actor(_))
            );
        }
        for key in ActorEffectKey::ALL {
            let decoded: ActorEffectKey =
                serde_json::from_str(&serde_json::to_string(key).unwrap()).unwrap();
            assert_eq!(decoded, *key);
        }
        let legacy: ToolEffectKey = serde_json::from_str("\"Forks\"").unwrap();
        assert_eq!(legacy, ToolEffectKey::Actor(ActorEffectKey::Forks));
        assert_eq!(serde_json::to_string(&legacy).unwrap(), "\"Forks\"");
        assert_eq!(legacy.nominal_identity(), None);
        assert!(!ActorEffectKey::ALL.contains(&ActorEffectKey::Forks));
        assert!(!DEFAULT_ACTOR_EFFECTS.contains(&ActorEffectKey::Forks));
        assert!(serde_json::from_str::<ToolEffectKey>("\"NotAnEffect\"").is_err());
    }
}
