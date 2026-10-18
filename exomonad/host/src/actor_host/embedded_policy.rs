use std::{collections::HashMap, sync::Arc};

use harness::finalize::FunctionToolSchema;

use exomonad_actor::{
    ActorRef, HostedCheckpointCapture, HostedContextBinding, LocalResidentInstallation,
    ResidentToolEndpoint, ResidentToolError, ResidentToolFuture,
};
use exomonad_tool::{HostedTool, ToolArguments, ToolInvocation, ToolInvocationContext};
use serde_json::{json, Value};

/// The published transport entry for an actor. Each model request pins its
/// current handler and source through `request_snapshot`.
pub(super) struct EmbeddedPolicyInstallation {
    actor: ActorRef,
    policy: Arc<dyn ResidentToolEndpoint>,
    projection: std::sync::Mutex<Option<CachedToolProjection>>,
    observation: exomonad_actor::ActorRuntimeObservationHandle,
    reasoning: Option<exomonad_actor::Reasoning>,
    conversation_permission: Option<exomonad_actor::ConversationPermission>,
}

impl EmbeddedPolicyInstallation {
    pub(super) fn from_installation(installation: &LocalResidentInstallation) -> Self {
        let mut policy = Self::with_observation(
            installation.actor.identity(),
            installation.policy.clone(),
            installation.runtime_observation.clone(),
        );
        policy.reasoning = installation.reasoning;
        policy.conversation_permission =
            installation.capabilities.conversation_permission().cloned();
        policy
    }

    #[cfg(test)]
    fn new(actor: ActorRef, policy: Arc<dyn ResidentToolEndpoint>) -> Self {
        Self::with_observation(actor, policy, Default::default())
    }

    fn with_observation(
        actor: ActorRef,
        policy: Arc<dyn ResidentToolEndpoint>,
        observation: exomonad_actor::ActorRuntimeObservationHandle,
    ) -> Self {
        Self {
            actor,
            policy,
            projection: std::sync::Mutex::new(None),
            observation,
            reasoning: None,
            conversation_permission: None,
        }
    }

    pub(super) fn inference_reasoning(&self) -> Option<exomonad_actor::Reasoning> {
        self.reasoning
    }

    pub(super) fn conversation_permission(
        &self,
    ) -> Option<&exomonad_actor::ConversationPermission> {
        self.conversation_permission.as_ref()
    }

    pub(super) fn observation(&self) -> exomonad_actor::ActorRuntimeObservationHandle {
        self.observation.clone()
    }

    pub(super) fn actor(&self) -> ActorRef {
        self.actor
    }

    pub(super) fn complete(
        &self,
        boundary: exomonad_actor::ContextCheckpointBoundary,
    ) -> ResidentToolFuture {
        self.policy.complete_boxed(boundary)
    }

    pub(super) fn abort(
        &self,
        boundary: exomonad_actor::ContextCheckpointBoundary,
    ) -> ResidentToolFuture {
        self.policy.abort_boxed(boundary)
    }

    pub(super) fn request_snapshot(&self) -> Result<EmbeddedPolicySnapshot, ResidentToolError> {
        let policy = self.policy.snapshot_for_request()?;
        let request_activation = policy
            .request_reply_activation()
            .map(|activation| {
                activation
                    .host_attachment(harness::provider::RequestActivation::new)
                    .map(|scope| scope.as_ref().clone())
                    .map_err(ResidentToolError::Unavailable)
            })
            .transpose()?;
        // Project only the declarations retained by this request's endpoint.
        // The cache contains metadata, never an execution-owning endpoint.
        let mut cached = self.projection.lock().unwrap();
        let declared = policy.declaration_snapshot();
        if cached.as_ref().is_none_or(|cached| {
            !Arc::ptr_eq(&cached.declarations, &declared)
                && !exomonad_tool::surface::compare_surfaces(&cached.declarations, &declared)
                    .is_empty()
        }) {
            *cached = Some(CachedToolProjection {
                declarations: declared.clone(),
                projection: project_tools(&declared).map(Arc::new),
            });
        }
        // Equivalent declarations from a new installation share the projection,
        // while subsequent requests reuse that installation's Arc directly.
        cached.as_mut().unwrap().declarations = declared;
        let projection = cached
            .as_ref()
            .unwrap()
            .projection
            .as_ref()
            .map_err(|error| ResidentToolError::Unavailable(error.clone()))?
            .clone();
        Ok(EmbeddedPolicySnapshot {
            #[cfg(test)]
            actor: self.actor,
            policy,
            request_activation,
            manifest: projection.manifest.clone(),
            schemas: projection.schemas.clone(),
        })
    }
}

/// One issued request keeps the manifest and exact actor-owned handler/source
/// lease together. The opaque endpoint refuses unsupported snapshots.
pub(super) struct EmbeddedPolicySnapshot {
    #[cfg(test)]
    actor: ActorRef,
    policy: Arc<dyn ResidentToolEndpoint>,
    request_activation: Option<harness::provider::RequestActivation>,
    manifest: Arc<harness::embedding::EmbeddedToolManifest>,
    schemas: Arc<HashMap<String, FunctionToolSchema>>,
}

struct CachedToolProjection {
    declarations: Arc<[HostedTool]>,
    projection: Result<Arc<EmbeddedToolProjection>, String>,
}

struct EmbeddedToolProjection {
    manifest: Arc<harness::embedding::EmbeddedToolManifest>,
    schemas: Arc<HashMap<String, FunctionToolSchema>>,
}

impl EmbeddedPolicySnapshot {
    pub(super) fn request_reply_activation(
        &self,
    ) -> Option<Arc<exomonad_actor::RequestReplyActivation>> {
        self.policy.request_reply_activation()
    }
    pub(super) fn request_activation(&self) -> Option<harness::provider::RequestActivation> {
        self.request_activation.clone()
    }
    pub(super) fn invocation_role(&self, name: &str) -> harness::provider::InvocationRole {
        match self.policy.invocation_role(name) {
            exomonad_actor::HostedInvocationRole::Domain => {
                harness::provider::InvocationRole::Domain
            }
            exomonad_actor::HostedInvocationRole::Observation => {
                harness::provider::InvocationRole::Observation
            }
            exomonad_actor::HostedInvocationRole::Settlement => {
                harness::provider::InvocationRole::Settlement
            }
        }
    }

    pub(super) fn request_generation(
        &self,
    ) -> Option<tidepool_runtime::session::NotebookGenerationId> {
        self.policy.request_generation()
    }

    pub(super) fn request_instructions(&self) -> Option<&str> {
        self.policy.request_instructions()
    }

    #[cfg(test)]
    pub(super) fn actor(&self) -> ActorRef {
        self.actor
    }

    pub(super) fn tools(&self) -> &[Value] {
        self.manifest.tools()
    }

    pub(super) fn manifest(&self) -> Arc<harness::embedding::EmbeddedToolManifest> {
        self.manifest.clone()
    }

    pub(super) fn dispatch(
        &self,
        name: String,
        arguments: ToolArguments,
        context: ToolInvocationContext,
        checkpoint_capture: Option<Arc<dyn HostedCheckpointCapture>>,
        context_binding: Option<Arc<dyn HostedContextBinding>>,
    ) -> ResidentToolFuture {
        let validated = match (self.policy.argument_stage(&name), arguments.clone()) {
            (exomonad_actor::HostedInputStage::ProviderWire, wire) => Ok(wire),
            (_, ToolArguments::Structured(value)) => match self.schemas.get(&name) {
                Some(schema) => schema
                    .decode_arguments(value)
                    .map(ToolArguments::Structured)
                    .map_err(|error| ResidentToolError::InvalidInvocation(error.to_string())),
                None => Ok(ToolArguments::Structured(value)),
            },
            (_, raw) => Ok(raw),
        };
        let future = self.policy.dispatch_validated_with_context_boxed(
            ToolInvocation {
                context: Some(context.clone()),
                name,
                arguments,
            },
            validated,
            checkpoint_capture,
            context_binding,
        );
        let operation = context.model_operation().cloned();
        Box::pin(async move {
            let operation = operation.ok_or_else(|| {
                ResidentToolError::InvalidInvocation(
                    "native result requires its exact issuing operation".into(),
                )
            })?;
            encode_native_result(future.await?, operation).map_err(ResidentToolError::Encoding)
        })
    }

    pub(super) fn implementation(&self, name: &str) -> Option<exomonad_tool::ToolImplementation> {
        self.declaration(name).map(HostedTool::implementation)
    }

    fn declaration(&self, name: &str) -> Option<&HostedTool> {
        self.policy.tools().iter().find(|tool| tool.name() == name)
    }

    pub(super) fn context_read_write(&self, name: &str) -> Result<bool, ResidentToolError> {
        let Some(tool) = self.declaration(name) else {
            return Ok(false);
        };
        if !tool
            .effect_keys()
            .contains(&exomonad_tool::ToolEffectKey::ContextReadWrite)
        {
            return Ok(false);
        }
        if tool.scheduling() != exomonad_tool::ToolScheduling::BeforeNextInference {
            return Err(ResidentToolError::InvalidInvocation(
                "ContextReadWrite requires synchronous tool scheduling".into(),
            ));
        }
        Ok(true)
    }

    pub(super) fn retained_operation(
        &self,
        context: ToolInvocationContext,
    ) -> Result<exomonad_actor::HostedOperationSettlement, ResidentToolError> {
        self.policy.retained_operation(context)
    }

    pub(super) async fn abort_operation(
        &self,
        context: ToolInvocationContext,
    ) -> Result<(), ResidentToolError> {
        let original = context.model_operation().ok_or_else(|| {
            ResidentToolError::InvalidInvocation(
                "provider finalization needs an original operation".into(),
            )
        })?;
        self.policy
            .abort_boxed(exomonad_actor::ContextCheckpointBoundary::Hosted(
                original.clone(),
            ))
            .await
            .map(|_| ())
    }

    pub(super) async fn cancel(
        &self,
        context: ToolInvocationContext,
        cause: exomonad_actor::WorkbenchCancellationCause,
    ) -> Result<exomonad_actor::WorkbenchCancellationOutcome, ResidentToolError> {
        self.policy.cancel_workbench_boxed(context, cause).await
    }
}

/// Ordinary model output follows the authored presentation. The exact operation
/// keeps canonical semantic JSON in Store, independently of this projection.
fn compact_workbench(mut projected: Value, operation: Value) -> Value {
    let successful =
        projected["status"] == "committed" && projected["publication"]["status"] == "published";
    if let Some(fields) = projected.as_object_mut() {
        fields.remove("result");
    }
    if let Some(failure) = projected.get_mut("failure").and_then(Value::as_object_mut) {
        failure.insert("retainedEvidence".into(), json!({"owner":"canonical operation output", "inspection":"inspectResult resultRef", "modelTool":true}));
    }
    if let Some(items) = projected
        .get_mut("failure")
        .and_then(|failure| failure.get_mut("items"))
        .and_then(Value::as_array_mut)
    {
        for item in items {
            if let Some(fields) = item.as_object_mut() {
                fields.remove("value");
            }
        }
    }
    for (position, item) in projected
        .get_mut("items")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let Some(fields) = item.as_object_mut() else {
            continue;
        };
        fields.remove("value");
        let output = fields
            .get("output")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if let Some(operations) = fields.get_mut("operations").and_then(Value::as_array_mut) {
            for operation in operations {
                let published = operation["displayPublication"]["status"] == "published";
                if let Some(display) = operation.get_mut("display").and_then(Value::as_object_mut) {
                    let in_output = display
                        .get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|text| output.contains(text));
                    if display.remove("text").is_some() {
                        if successful && published && in_output {
                            display.insert(
                                "textSource".into(),
                                json!(format!("/items/{position}/output")),
                            );
                        } else {
                            display.insert("textOmitted".into(), json!(true));
                        }
                    }
                }
            }
        }
    }
    projected["resultRef"] = if operation.get("origin").is_some() {
        json!({"resultRun":operation["origin"]["run"],"resultActor":operation["origin"]["actor"],
            "resultIncarnation":operation["origin"]["incarnation"],"resultRequest":operation["request"],"resultCall":operation["call"]})
    } else {
        operation
    };
    projected
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct NativeEnvelope {
    native_result: NativeResult,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct NativeResult {
    version: u32,
    operation: exomonad_tool::OriginalOperation,
    #[serde(flatten)]
    result: NativeBody,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
enum NativeBody {
    Workbench(Value),
    Value(Value),
    Failure(Value),
}

pub(super) fn encode_native_result(
    response: exomonad_actor::ResidentToolResponse,
    operation: exomonad_tool::OriginalOperation,
) -> Result<Value, serde_json::Error> {
    let result = match response {
        exomonad_actor::ResidentToolResponse::Workbench(body) => {
            NativeBody::Workbench(serde_json::to_value(body)?)
        }
        exomonad_actor::ResidentToolResponse::Value(body) => NativeBody::Value(body),
    };
    serde_json::to_value(NativeEnvelope {
        native_result: NativeResult {
            version: 1,
            operation,
            result,
        },
    })
}

pub(super) fn encode_native_failure(
    body: Value,
    operation: exomonad_tool::OriginalOperation,
) -> Value {
    serde_json::to_value(NativeEnvelope {
        native_result: NativeResult {
            version: 1,
            operation,
            result: NativeBody::Failure(body),
        },
    })
    .expect("native result envelope is serializable")
}

pub(super) fn native_original(
    operation: &harness::model::OperationId,
) -> Result<exomonad_tool::OriginalOperation, String> {
    let harness::model::ConversationIdentity::Embedded {
        run,
        actor,
        incarnation,
    } = &operation.origin
    else {
        return Err("native receipt has no embedded issuer".into());
    };
    Ok(exomonad_tool::OriginalOperation {
        origin: exomonad_tool::ConversationOrigin::Embedded {
            run: run.clone(),
            actor: actor.0.clone(),
            incarnation: incarnation.clone(),
        },
        request_id: operation.request.0.clone(),
        call_id: operation.call.0.clone(),
    })
}

/// Only settlement-issued format metadata permits interpretation. Legacy
/// payloads stay opaque even when their fields imitate this private envelope.
pub(super) fn project_retained_result(
    recorded: &harness::store::RecordedToolOutput,
    operation: &harness::model::OperationId,
) -> Result<Option<Value>, String> {
    use harness::{provider::OutputFormat, store::TerminalOutcome};
    if recorded.output_format == OutputFormat::LegacyOpaque {
        return Ok(None);
    }
    let path = match &recorded.terminal {
        TerminalOutcome::Success => "",
        TerminalOutcome::Failure(_) | TerminalOutcome::CancellationUnconfirmed(_) => "/result",
        TerminalOutcome::CancelledWithReceipt(Ok(_))
        | TerminalOutcome::CancelledWithValueReceipt => "/receipt",
        TerminalOutcome::CancelledWithReceipt(Err(_)) => "/receipt/result",
        TerminalOutcome::NotStarted | TerminalOutcome::Cancelled | TerminalOutcome::Interrupted => {
            return Err("native result format on a terminal without a receipt".into())
        }
    };
    let text = recorded.item.0["output"]
        .as_str()
        .ok_or("native canonical output has no text body")?;
    let mut output: Value = serde_json::from_str(text)
        .map_err(|error| format!("native canonical output cannot be decoded: {error}"))?;
    let envelope_value = output
        .pointer(path)
        .cloned()
        .ok_or("native canonical receipt is missing")?;
    let version = envelope_value
        .pointer("/nativeResult/version")
        .and_then(Value::as_u64)
        .ok_or("native receipt version is missing or malformed")?;
    if version != 1 {
        return Err(format!("unsupported native receipt version {version}"));
    }
    let envelope: NativeEnvelope = serde_json::from_value(envelope_value)
        .map_err(|error| format!("native receipt envelope is invalid: {error}"))?;
    let native = envelope.native_result;
    if native.operation != native_original(operation)? {
        return Err("native receipt belongs to a different operation".into());
    }
    let original = serde_json::to_value(operation).map_err(|error| error.to_string())?;
    if matches!(
        &recorded.terminal,
        TerminalOutcome::Success
            | TerminalOutcome::CancelledWithReceipt(Ok(_))
            | TerminalOutcome::CancelledWithValueReceipt
    ) && matches!(&native.result, NativeBody::Failure(_))
    {
        return Err("native failure receipt cannot certify a value terminal".into());
    }
    let projected = match native.result {
        NativeBody::Value(body) => body,
        NativeBody::Workbench(body) | NativeBody::Failure(body) => {
            compact_workbench(body, original.clone())
        }
    };
    if path.is_empty() {
        return Ok(Some(projected));
    }
    *output
        .pointer_mut(path)
        .ok_or("native receipt location changed")? = projected;
    // Failure diagnostics are bounded separately; canonical result payloads are
    // available through inspectResult rather than repeated in their summary.
    match &recorded.terminal {
        TerminalOutcome::Failure(_) | TerminalOutcome::CancellationUnconfirmed(_) => {
            output = compact_workbench(output, original)
        }
        TerminalOutcome::CancelledWithReceipt(Err(_)) => {
            let receipt = output
                .get_mut("receipt")
                .ok_or("cancelled receipt is missing")?;
            *receipt = compact_workbench(receipt.take(), original);
        }
        _ => {}
    }
    Ok(Some(output))
}

#[cfg(test)]
pub(super) fn project_workbench_model_output(
    response: Value,
    context: &ToolInvocationContext,
) -> Result<Value, serde_json::Error> {
    Ok(compact_workbench(response, serde_json::to_value(context)?))
}

#[cfg(test)]
fn project_model_result(
    response: exomonad_actor::ResidentToolResponse,
    context: &ToolInvocationContext,
) -> Result<Value, serde_json::Error> {
    match response {
        exomonad_actor::ResidentToolResponse::Workbench(body) => {
            project_workbench_model_output(serde_json::to_value(body)?, context)
        }
        exomonad_actor::ResidentToolResponse::Value(body) => Ok(body),
    }
}

/// Validate the transport contract before a generation can be published.
pub(super) fn validate_tools(tools: &[HostedTool]) -> Result<(), String> {
    project_tools(tools).map(|_| ())
}

fn project_tools(tools: &[HostedTool]) -> Result<EmbeddedToolProjection, String> {
    let mut projected = Vec::with_capacity(tools.len());
    let mut schemas = HashMap::new();
    let mut scheduling = HashMap::new();
    let mut text_fields = HashMap::new();
    for tool in tools {
        if matches!(tool, HostedTool::Custom(_))
            && tool.implementation() == exomonad_tool::ToolImplementation::HaskellCell
        {
            text_fields.insert(tool.name().to_owned(), "source".to_owned());
        }
        scheduling.insert(
            tool.name().to_owned(),
            match tool.scheduling() {
                exomonad_tool::ToolScheduling::Async => harness::provider::ToolScheduling::Async,
                exomonad_tool::ToolScheduling::BeforeNextInference => {
                    harness::provider::ToolScheduling::BeforeNextInference
                }
            },
        );
        projected.push(match tool {
            HostedTool::Function(declaration) => {
                let schema =
                    FunctionToolSchema::new(declaration.input_schema.clone()).map_err(|error| {
                        format!("invalid tool schema for {}: {error}", declaration.name)
                    })?;
                let tool = json!({
                    "type": "function",
                    "name": declaration.name,
                    "description": declaration.description,
                    "parameters": schema.parameters(),
                    "strict": true,
                });
                schemas.insert(declaration.name.clone(), schema);
                tool
            }
            HostedTool::Custom(declaration) => json!({
                "type": "custom",
                "name": declaration.name,
                "description": declaration.description,
                "format": { "type": "text" },
            }),
        });
    }
    let manifest = harness::embedding::EmbeddedToolManifest::with_text_fields(
        projected,
        scheduling,
        text_fields,
    )
    .map_err(|error| error.to_string())?;
    Ok(EmbeddedToolProjection {
        manifest: Arc::new(manifest),
        schemas: Arc::new(schemas),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use exomonad_actor::ResidentToolDispatchFuture;
    use exomonad_tool::{CustomToolDeclaration, ToolDeclaration, ToolKind};
    use std::future::ready;

    fn project_model_result(
        response: exomonad_actor::ResidentToolResponse,
    ) -> Result<Value, serde_json::Error> {
        // Pure correlation fixture; the hosted history checks the real issuer.
        super::project_model_result(response, &context())
    }

    fn native_operation() -> harness::model::OperationId {
        harness::model::OperationId {
            origin: harness::model::ConversationIdentity::Embedded {
                run: "native-test".into(),
                actor: harness::model::AgentPath("/root".into()),
                incarnation: "1".into(),
            },
            request: harness::model::RequestId("request".into()),
            call: harness::model::CallId("call".into()),
        }
    }

    fn native_record(
        body: Value,
        format: harness::provider::OutputFormat,
    ) -> harness::store::RecordedToolOutput {
        let operation = native_operation();
        let mut item = harness::item::Item::tool_output(
            &operation.call,
            harness::item::ToolKind::Function,
            &harness::turn::JobOutput::Completed(Ok(body)),
        );
        if format != harness::provider::OutputFormat::LegacyOpaque {
            item.0["output_format"] = serde_json::to_value(format).unwrap();
        }
        harness::store::RecordedToolOutput {
            item,
            terminal: harness::store::TerminalOutcome::Success,
            output_format: format,
        }
    }

    #[test]
    fn native_format_is_required_and_opaque_value_fields_are_never_receipt_evidence() {
        use harness::provider::OutputFormat;
        let operation = native_operation();
        let opaque =
            json!({"result":{"semantic":"retain"},"items":[1,2],"receipt":"authored field"});
        let encoded = encode_native_result(
            exomonad_actor::ResidentToolResponse::Value(opaque.clone()),
            native_original(&operation).unwrap(),
        )
        .unwrap();
        for body in [opaque.clone(), encoded.clone(), json!("plain result")] {
            assert_eq!(
                project_retained_result(
                    &native_record(body, OutputFormat::LegacyOpaque),
                    &operation
                )
                .unwrap(),
                None
            );
        }
        let mut plaintext = native_record(json!(null), OutputFormat::LegacyOpaque);
        plaintext.item.0["output"] = json!("a plain legacy refusal, not JSON");
        plaintext.terminal = harness::store::TerminalOutcome::Failure("refused".into());
        assert_eq!(
            project_retained_result(&plaintext, &operation).unwrap(),
            None
        );
        assert_eq!(
            project_retained_result(
                &native_record(encoded, OutputFormat::NativeReceiptV1),
                &operation
            )
            .unwrap(),
            Some(opaque)
        );
    }

    #[test]
    fn marked_native_envelope_requires_exact_operation_and_supported_version() {
        use harness::provider::OutputFormat;
        let operation = native_operation();
        let encoded = encode_native_result(
            exomonad_actor::ResidentToolResponse::Workbench(display_response()),
            native_original(&operation).unwrap(),
        )
        .unwrap();
        let projected = project_retained_result(
            &native_record(encoded.clone(), OutputFormat::NativeReceiptV1),
            &operation,
        )
        .unwrap()
        .unwrap();
        assert!(projected["items"][0].get("value").is_none());
        let mut unsupported = encoded.clone();
        unsupported["nativeResult"]["version"] = json!(99);
        assert!(project_retained_result(
            &native_record(unsupported, OutputFormat::NativeReceiptV1),
            &operation
        )
        .unwrap_err()
        .contains("unsupported"));
        let mut foreign = encoded;
        foreign["nativeResult"]["operation"]["call_id"] = json!("different");
        assert!(project_retained_result(
            &native_record(foreign, OutputFormat::NativeReceiptV1),
            &operation
        )
        .unwrap_err()
        .contains("different operation"));
    }

    #[test]
    fn marked_depth_refusal_keeps_complete_canonical_bytes() {
        use harness::provider::OutputFormat;
        let operation = native_operation();
        let mut deep = json!("leaf");
        for _ in 0..160 {
            deep = json!([deep]);
        }
        let encoded = encode_native_result(
            exomonad_actor::ResidentToolResponse::Value(deep),
            native_original(&operation).unwrap(),
        )
        .unwrap();
        let record = native_record(encoded, OutputFormat::NativeReceiptV1);
        let canonical = record.item.clone();
        assert!(project_retained_result(&record, &operation).is_err());
        assert_eq!(record.item, canonical);
        assert!(record.item.0["output"].as_str().unwrap().contains("leaf"));
    }

    fn display_response() -> tidepool_runtime::session::WorkbenchResponse {
        use tidepool_runtime::session::*;
        let page = WorkbenchDisplayPublicationIdentity {
            display: (1, 2, 3),
            page_ordinal: 1,
        };
        let output = ActorOutputReference {
            run: "run".into(),
            sequence: 42,
        };
        WorkbenchResponse {
            status: WorkbenchRunStatus::Committed,
            summary: None,
            next_index: 1,
            total: 1,
            publication: Some(WorkbenchPublicationOutcome::Published {
                bindings: vec!["observed".into()],
            }),
            items: vec![WorkbenchItemReceipt {
                presentation: None,
                index: 0,
                kind: None,
                span: None,
                source_items: vec![],
                status: WorkbenchItemStatus::Committed,
                output: "observed value\n[bound observed]".into(),
                value: None,
                diagnostics: vec![],
                failure_layer: None,
                warnings: vec!["warning".into()],
                installed_bindings: vec!["observed".into()],
                terminal_transfer: None,
                operations: vec![WorkbenchOperationReceipt {
                    id: WorkbenchOperationId {
                        execution: WorkbenchExecutionId::from_digest([1; 16]),
                        input_unit_index: 0,
                        effect_ordinal: 0,
                    },
                    effect: "Console.DisplayWith".into(),
                    disposition: WorkbenchOperationDisposition::Committed,
                    display: Some(WorkbenchDisplayOutput {
                        page: WorkbenchDisplayPage {
                            identity: page.display,
                            text: "observed value".into(),
                            expansions: vec![(1, "detail".into())],
                            unavailable: true,
                        },
                        output: output.clone(),
                    }),
                    display_publication: Some(WorkbenchDisplayPublication::Published {
                        publication: page,
                        output,
                    }),
                }],
            }],
        }
    }

    #[test]
    fn model_presentation_keeps_published_display_references_and_pruned_pages_canonical() {
        let response = display_response();
        let projected = project_model_result(exomonad_actor::ResidentToolResponse::Workbench(
            response.clone(),
        ))
        .unwrap();
        assert_eq!(
            projected["items"][0]["operations"][0]["display"]["textSource"],
            "/items/0/output"
        );
        assert!(projected["items"][0]["operations"][0]["display"]
            .get("text")
            .is_none());
        let mut pruned = response.clone();
        pruned.items[0].output = "selected summary".into();
        let projected =
            project_model_result(exomonad_actor::ResidentToolResponse::Workbench(pruned)).unwrap();
        assert_eq!(projected["items"][0]["output"], "selected summary");
        assert_eq!(
            projected["items"][0]["operations"][0]["display"]["textOmitted"],
            true
        );
        assert_eq!(
            response.items[0].operations[0]
                .display
                .as_ref()
                .unwrap()
                .text,
            "observed value"
        );
    }

    #[test]
    fn authored_presentation_omits_all_semantics_but_keeps_canonical_input() {
        let mut response = display_response();
        response.items[0].value = Some(json!({"nested":"λ".repeat(40_000)}));
        let canonical = response.clone();
        let projected =
            project_model_result(exomonad_actor::ResidentToolResponse::Workbench(response))
                .unwrap();
        assert!(projected["items"][0].get("value").is_none());
        assert!(projected.get("resultRef").is_some());
        assert_eq!(projected["items"][0]["output"], canonical.items[0].output);
        assert!(canonical.items[0].value.is_some());
    }

    proptest::proptest! {
        #[test]
        fn model_projection_preserves_arbitrary_display_values(
            texts in proptest::collection::vec("(?s).{0,128}", 2..8),
            budget in 0usize..1024,
            published in proptest::bool::ANY,
            refused in proptest::bool::ANY,
        ) {
            use tidepool_runtime::session::*;
            let mut response = display_response();
            let template = response.items[0].operations[0].clone();
            response.items[0].operations = texts.iter().enumerate().map(|(ordinal, text)| {
                let mut operation = template.clone();
                operation.id.effect_ordinal = ordinal;
                operation.display.as_mut().unwrap().page.text = text.clone();
                if refused {
                    operation.display_publication = Some(WorkbenchDisplayPublication::Refused {
                        publication: WorkbenchDisplayPublicationIdentity { display: (1, 2, 3), page_ordinal: 1 },
                        detail: "refused".into(),
                    });
                }
                operation
            }).collect();
            // Simulate a final output selection/budget after exact display publication.
            // The original published pages may outlive this shortened presentation.
            let mut selected = texts.join("\n");
            let mut end = budget.min(selected.len());
            while !selected.is_char_boundary(end) {
                end -= 1;
            }
            selected.truncate(end);
            response.items[0].output = selected;
            if !published {
                response.publication = Some(WorkbenchPublicationOutcome::RecoveryRequired { detail: "unknown".into() });
            }
            let projected = project_model_result(exomonad_actor::ResidentToolResponse::Workbench(response.clone())).unwrap();
            proptest::prop_assert_eq!(&projected["status"], &serde_json::to_value(response.status).unwrap());
            proptest::prop_assert_eq!(&projected["total"], &json!(response.total));
            proptest::prop_assert_eq!(&projected["nextIndex"], &json!(response.next_index));
            proptest::prop_assert_eq!(&projected["publication"], &serde_json::to_value(&response.publication).unwrap());
            proptest::prop_assert_eq!(&projected["items"][0]["output"], &json!(response.items[0].output));
            proptest::prop_assert_eq!(&projected["items"][0]["installedBindings"], &json!(response.items[0].installed_bindings));
            proptest::prop_assert_eq!(&projected["items"][0]["warnings"], &json!(response.items[0].warnings));
            for (ordinal, original) in response.items[0].operations.iter().enumerate() {
                let encoded = &projected["items"][0]["operations"][ordinal];
                let display = original.display.as_ref().unwrap();
                proptest::prop_assert!(encoded["display"].get("text").is_none());
                if let Some(pointer) = encoded["display"]["textSource"].as_str() {
                    let visible = projected.pointer(pointer).and_then(Value::as_str).unwrap();
                    proptest::prop_assert!(visible.contains(&display.text));
                } else {
                    proptest::prop_assert_eq!(&encoded["display"]["textOmitted"], &json!(true));
                }
                proptest::prop_assert_eq!(&encoded["id"], &serde_json::to_value(&original.id).unwrap());
                proptest::prop_assert_eq!(&encoded["effect"], &json!(original.effect));
                proptest::prop_assert_eq!(&encoded["disposition"], &serde_json::to_value(original.disposition).unwrap());
                proptest::prop_assert_eq!(&encoded["displayPublication"], &serde_json::to_value(&original.display_publication).unwrap());
                proptest::prop_assert_eq!(&encoded["display"]["identity"], &json!(display.identity));
                proptest::prop_assert_eq!(&encoded["display"]["expansions"], &json!(display.expansions));
                proptest::prop_assert_eq!(&encoded["display"]["unavailable"], &json!(display.unavailable));
                proptest::prop_assert_eq!(&encoded["display"]["output"], &serde_json::to_value(&display.output).unwrap());
                if !published || refused {
                    proptest::prop_assert_eq!(encoded, &serde_json::to_value(original).unwrap());
                }
            }
        }
    }

    struct ProbePolicy {
        tools: Vec<HostedTool>,
        marker: &'static str,
        request_policy: Option<Arc<dyn ResidentToolEndpoint>>,
    }

    impl ResidentToolEndpoint for ProbePolicy {
        fn request_instructions(&self) -> Option<&str> {
            Some(self.marker)
        }

        fn snapshot_for_request(&self) -> Result<Arc<dyn ResidentToolEndpoint>, ResidentToolError> {
            Ok(self
                .request_policy
                .clone()
                .unwrap_or_else(|| policy_with_tools(self.marker, self.tools.clone())))
        }
        fn tools(&self) -> &[HostedTool] {
            &self.tools
        }

        fn instructions(&self) -> Option<&str> {
            None
        }

        fn dispatch_boxed(&self, invocation: ToolInvocation) -> ResidentToolDispatchFuture {
            let marker = self.marker;
            Box::pin(ready(Ok(exomonad_actor::ResidentToolResponse::Value(
                json!({
                    "marker": marker,
                    "name": invocation.name,
                    "context": invocation.context.as_ref().map(|context| context.call_id.as_str()),
                    "arguments": match invocation.arguments {
                        ToolArguments::Raw(raw) => json!({"raw": raw}),
                        ToolArguments::Structured(value) => json!({"structured": value}),
                    },
                }),
            ))))
        }
    }

    fn policy(marker: &'static str) -> Arc<dyn ResidentToolEndpoint> {
        policy_with_tools(marker, declared_tools())
    }

    fn declared_tools() -> Vec<HostedTool> {
        vec![
            HostedTool::Custom(CustomToolDeclaration {
                schedule: Default::default(),
                implementation: Default::default(),
                effect_keys: Vec::new(),
                name: "haskell".into(),
                description: "Run a notebook cell".into(),
            }),
            HostedTool::Function(ToolDeclaration {
                schedule: Default::default(),
                implementation: Default::default(),
                effect_keys: Vec::new(),
                name: "lookup".into(),
                description: "Look up a value".into(),
                input_schema: json!({"type": "object", "properties": {}}),
                output_schema: None,
                kind: ToolKind::Call,
            }),
        ]
    }

    fn policy_with_tools(
        marker: &'static str,
        tools: Vec<HostedTool>,
    ) -> Arc<dyn ResidentToolEndpoint> {
        Arc::new(ProbePolicy {
            marker,
            tools,
            request_policy: None,
        })
    }

    fn reloading_policy() -> Arc<dyn ResidentToolEndpoint> {
        let pinned = policy("issued-handler");
        Arc::new(ProbePolicy {
            tools: declared_tools(),
            marker: "stale-handler",
            request_policy: Some(pinned),
        })
    }

    fn context() -> ToolInvocationContext {
        ToolInvocationContext::external(
            "embedded-run".into(),
            "request-1".into(),
            "call-1".into(),
            None,
            Some("embedded".into()),
        )
    }

    #[test]
    fn installed_context_authority_follows_pinned_effects_and_schedule() {
        use exomonad_tool::{ToolEffectKey, ToolImplementation, ToolScheduling};
        let tool = |name: &str, schedule, effect_keys| {
            HostedTool::Custom(CustomToolDeclaration {
                name: name.into(),
                description: "Selected notebook".into(),
                schedule,
                implementation: ToolImplementation::HaskellCell,
                effect_keys,
            })
        };
        let actor = exomonad_actor::ActorRef::first(exomonad_actor::ActorId(7));
        let ordinary = EmbeddedPolicyInstallation::new(
            actor,
            policy_with_tools(
                "ordinary",
                vec![tool(
                    "unusual-notebook",
                    ToolScheduling::BeforeNextInference,
                    vec![],
                )],
            ),
        )
        .request_snapshot()
        .unwrap();
        let curation = EmbeddedPolicyInstallation::new(
            actor,
            policy_with_tools(
                "curation",
                vec![tool(
                    "unusual-notebook",
                    ToolScheduling::BeforeNextInference,
                    vec![ToolEffectKey::ContextReadWrite],
                )],
            ),
        )
        .request_snapshot()
        .unwrap();
        assert!(!ordinary.context_read_write("unusual-notebook").unwrap());
        assert!(curation.context_read_write("unusual-notebook").unwrap());
        assert!(!curation.context_read_write("haskell_sync").unwrap());
        assert!(!ordinary.context_read_write("unusual-notebook").unwrap());
        let asynchronous = EmbeddedPolicyInstallation::new(
            actor,
            policy_with_tools(
                "async",
                vec![
                    tool("ordinary-async", ToolScheduling::Async, vec![]),
                    tool(
                        "invalid-curation",
                        ToolScheduling::Async,
                        vec![ToolEffectKey::ContextReadWrite],
                    ),
                ],
            ),
        )
        .request_snapshot()
        .unwrap();
        assert!(!asynchronous.context_read_write("ordinary-async").unwrap());
        assert!(matches!(
            asynchronous.context_read_write("invalid-curation"),
            Err(ResidentToolError::InvalidInvocation(_))
        ));
    }

    #[test]
    fn installed_haskell_text_fields_reach_function_only_surface_without_name_inference() {
        struct UnusedDispatcher;
        #[async_trait::async_trait]
        impl harness::provider::Provider for UnusedDispatcher {
            async fn call(
                &self,
                _: &str,
                _: Value,
            ) -> Result<Value, harness::provider::ProviderError> {
                panic!("schema observation must not dispatch a tool")
            }
            fn tools(&self) -> Vec<Value> {
                vec![]
            }
        }
        use exomonad_tool::{ToolImplementation, ToolScheduling};
        let custom = |name: &str, implementation, schedule| {
            HostedTool::Custom(CustomToolDeclaration {
                name: name.into(),
                description: "Raw endpoint".into(),
                implementation,
                schedule,
                effect_keys: vec![],
            })
        };
        let snapshot = EmbeddedPolicyInstallation::new(
            exomonad_actor::ActorRef::first(exomonad_actor::ActorId(7)),
            policy_with_tools(
                "field-owner",
                vec![
                    custom(
                        "unusual-notebook",
                        ToolImplementation::HaskellCell,
                        ToolScheduling::BeforeNextInference,
                    ),
                    custom(
                        "async-notebook",
                        ToolImplementation::HaskellCell,
                        ToolScheduling::Async,
                    ),
                    custom(
                        "haskell_sync",
                        ToolImplementation::ResidentHandler,
                        ToolScheduling::BeforeNextInference,
                    ),
                ],
            ),
        )
        .request_snapshot()
        .unwrap();
        let native = harness::embedding::ToolSurface::from_manifest(
            "field-owner".into(),
            snapshot.manifest(),
            Arc::new(UnusedDispatcher),
        )
        .unwrap();
        assert_eq!(native.tools(), snapshot.tools());
        let original = native
            .with_encoding(harness::transport::ToolEncodingProfile::Native)
            .unwrap();
        assert_eq!(original.tools(), snapshot.tools());
        assert!(original
            .tools()
            .iter()
            .all(|tool| tool["type"] == "custom" && tool["format"] == json!({"type":"text"})));
        let encoded = native
            .with_encoding(harness::transport::ToolEncodingProfile::FunctionOnlyExplicitDelivery)
            .unwrap();
        let tool = |name: &str| {
            encoded
                .tools()
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap()
        };
        assert_eq!(
            tool("unusual-notebook")["parameters"],
            json!({"type":"object","properties":{"source":{"type":"string"}},"required":["source"],"additionalProperties":false})
        );
        assert_eq!(
            tool("async-notebook")["parameters"],
            json!({"type":"object","properties":{"source":{"type":"string"},"delivery":{"type":"string","enum":["wait","background"]}},"required":["source","delivery"],"additionalProperties":false})
        );
        assert_eq!(
            tool("haskell_sync")["parameters"],
            json!({"type":"object","properties":{"input":{"type":"string"}},"required":["input"],"additionalProperties":false})
        );
    }

    #[test]
    fn installed_policy_projects_raw_and_strict_function_tools() {
        let snapshot = EmbeddedPolicyInstallation::new(
            exomonad_actor::ActorRef::first(exomonad_actor::ActorId(7)),
            policy("first"),
        )
        .request_snapshot()
        .unwrap();
        assert_eq!(
            snapshot.actor(),
            exomonad_actor::ActorRef::first(exomonad_actor::ActorId(7))
        );
        assert_eq!(
            snapshot.tools()[0],
            json!({
                "type": "custom",
                "name": "haskell",
                "description": "Run a notebook cell",
                "format": {"type": "text"},
                "async": true,
            })
        );
        assert_eq!(
            snapshot.tools()[1],
            json!({
                "type": "function",
                "name": "lookup",
                "description": "Look up a value",
                "parameters": {"type": "object", "properties": {}, "required": [], "additionalProperties": false},
                "strict": true,
                "async": true,
            })
        );
    }

    struct PromptOnlyReload {
        tools: Vec<HostedTool>,
        current: std::sync::Mutex<Arc<dyn ResidentToolEndpoint>>,
    }
    impl ResidentToolEndpoint for PromptOnlyReload {
        fn tools(&self) -> &[HostedTool] {
            &self.tools
        }
        fn instructions(&self) -> Option<&str> {
            None
        }
        fn snapshot_for_request(&self) -> Result<Arc<dyn ResidentToolEndpoint>, ResidentToolError> {
            Ok(self.current.lock().unwrap().clone())
        }
        fn dispatch_boxed(&self, _: ToolInvocation) -> ResidentToolDispatchFuture {
            panic!("dispatch must retain the issued generation")
        }
    }

    #[tokio::test]
    async fn prompt_only_reload_keeps_cached_manifest_but_selects_new_prompt_and_dispatcher() {
        let current = Arc::new(PromptOnlyReload {
            tools: declared_tools(),
            current: std::sync::Mutex::new(policy("A")),
        });
        let installation = EmbeddedPolicyInstallation::new(
            exomonad_actor::ActorRef::first(exomonad_actor::ActorId(7)),
            current.clone(),
        );
        let first = installation.request_snapshot().unwrap();
        *current.current.lock().unwrap() = policy("B");
        let second = installation.request_snapshot().unwrap();
        assert!(Arc::ptr_eq(&first.manifest, &second.manifest));
        assert_eq!(first.request_instructions(), Some("A"));
        assert_eq!(second.request_instructions(), Some("B"));
        for (snapshot, expected) in [(first, "A"), (second, "B")] {
            let result = snapshot
                .dispatch(
                    "haskell".into(),
                    ToolArguments::Raw("1".into()),
                    context(),
                    None,
                    None,
                )
                .await
                .unwrap();
            assert_eq!(result["marker"], expected);
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(64))]
        #[test]
        fn issued_surface_histories_keep_schema_schedule_prompt_and_dispatch_together(
            history in proptest::collection::vec((1usize..6, proptest::bool::ANY, proptest::bool::ANY, proptest::bool::ANY, proptest::bool::ANY), 2..12)
        ) {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let current = Arc::new(PromptOnlyReload {
                tools: declared_tools(),
                current: std::sync::Mutex::new(policy("initial")),
            });
            let installation = EmbeddedPolicyInstallation::new(
                exomonad_actor::ActorRef::first(exomonad_actor::ActorId(7)), current.clone());
            let mut issued = Vec::new();
            for (generation, (count, renamed, reverse, synchronous, integer)) in history.into_iter().enumerate() {
                let marker = if generation % 2 == 0 { "even" } else { "odd" };
                let mut tools = (0..count).map(|index| HostedTool::Function(ToolDeclaration {
                    schedule: if synchronous { exomonad_tool::ToolScheduling::BeforeNextInference } else { exomonad_tool::ToolScheduling::Async },
                    implementation: Default::default(),
                    effect_keys: Vec::new(),
                    name: format!("{}_{}", if renamed { "new" } else { "old" }, index),
                    description: "Probe the retained generation".into(),
                    input_schema: json!({"type":"object","properties":{"value":{"type":if integer { "integer" } else { "string" }}},"required":if integer { json!(["value"]) } else { json!([]) },"additionalProperties":false}),
                    output_schema: None,
                    kind: ToolKind::Call,
                })).collect::<Vec<_>>();
                if reverse { tools.reverse(); }
                *current.current.lock().unwrap() = policy_with_tools(marker, tools.clone());
                let snapshot = installation.request_snapshot().unwrap();
                let repeated = installation.request_snapshot().unwrap();
                proptest::prop_assert!(Arc::ptr_eq(&snapshot.manifest, &repeated.manifest));
                issued.push((snapshot, tools, marker, integer));
                // Every earlier request remains usable after additions, removals,
                // renaming, reordering, schema changes and scheduling changes.
                for (snapshot, tools, marker, integer) in &issued {
                    proptest::prop_assert_eq!(snapshot.policy.tools(), tools.as_slice());
                    proptest::prop_assert_eq!(snapshot.request_instructions(), Some(*marker));
                    let names = snapshot.tools().iter().map(|tool| tool["name"].as_str().unwrap()).collect::<Vec<_>>();
                    proptest::prop_assert_eq!(names, tools.iter().map(HostedTool::name).collect::<Vec<_>>());
                    for tool in tools {
                        let projected = snapshot.tools().iter().find(|entry| entry["name"] == tool.name()).unwrap();
                        proptest::prop_assert_eq!(&projected["async"], &json!(tool.scheduling() == exomonad_tool::ToolScheduling::Async));
                        let value = if *integer { json!(17) } else { json!("retained") };
                        let result = runtime.block_on(snapshot.dispatch(tool.name().into(), ToolArguments::Structured(json!({"value":value})), context(), None, None)).unwrap();
                        proptest::prop_assert_eq!(&result["marker"], &json!(marker));
                        proptest::prop_assert_eq!(&result["arguments"]["structured"]["value"], &value);
                        let null = runtime.block_on(snapshot.dispatch(tool.name().into(), ToolArguments::Structured(json!({"value":null})), context(), None, None)).unwrap();
                        proptest::prop_assert_eq!(&null["arguments"]["structured"], &if *integer { json!({"value":null}) } else { json!({}) });
                        // Primitive value validation belongs to the typed actor endpoint;
                        // this transport owner preserves its advertised type and null projection.
                        let value_schema = &projected["parameters"]["properties"]["value"];
                        let expected_type = if *integer { "integer" } else { "string" };
                        if *integer {
                            proptest::prop_assert_eq!(&value_schema["type"], &json!(expected_type));
                        } else {
                            proptest::prop_assert_eq!(&value_schema["anyOf"][0]["type"], &json!(expected_type));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn requests_share_unchanged_projection_and_accept_new_issued_declarations() {
        let actor = exomonad_actor::ActorRef::first(exomonad_actor::ActorId(7));
        let installation = EmbeddedPolicyInstallation::new(actor, policy("first"));
        let first = installation.request_snapshot().unwrap();
        let second = installation.request_snapshot().unwrap();
        assert!(Arc::ptr_eq(&first.manifest, &second.manifest));
        let incompatible = Arc::new(ProbePolicy {
            tools: declared_tools(),
            marker: "original",
            request_policy: Some(policy_with_tools("changed", vec![])),
        });
        let changed = EmbeddedPolicyInstallation::new(actor, incompatible)
            .request_snapshot()
            .unwrap();
        assert!(changed.tools().is_empty());
        assert!(!first.tools().is_empty());
    }

    #[tokio::test]
    async fn installed_builtin_tools_project_strict_schemas_and_preserve_host_omission_defaults() {
        let campaign = super::super::test_campaign::TestCampaign::start_with_shell().await;
        campaign
            .run_scenario(|campaign| {
                Box::pin(async move {
                    // Take the real installed declarations, rather than fixtures that copy
                    // the actor-local schema and can drift from its owner.
                    let tools = campaign.root_installation.policy.tools().to_vec();
                    let snapshot = EmbeddedPolicyInstallation::new(
                        campaign.actor.identity(),
                        policy_with_tools("builtin-projection", tools),
                    )
                    .request_snapshot()
                    .unwrap();
                    for (name, field) in [("status", "view")] {
                        let declaration = snapshot
                            .tools()
                            .iter()
                            .find(|tool| tool["name"] == name)
                            .unwrap();
                        assert_eq!(declaration["strict"], true);
                        assert_eq!(declaration["parameters"]["required"], json!([field]));
                        assert_eq!(declaration["parameters"]["additionalProperties"], false);
                        assert_eq!(
                            declaration["parameters"]["properties"][field]["anyOf"][1],
                            json!({"type":"null"})
                        );
                        let result = snapshot
                            .dispatch(
                                name.into(),
                                ToolArguments::Structured(json!({(field):null})),
                                context(),
                                None,
                                None,
                            )
                            .await
                            .unwrap();
                        assert_eq!(result["arguments"]["structured"], json!({}));
                    }
                    let bash = snapshot
                        .tools()
                        .iter()
                        .find(|tool| tool["name"] == "bash")
                        .expect("the installed shell spec exposes bash");
                    assert_eq!(bash["strict"], true);
                    let environment = &bash["parameters"]["properties"]["environment"];
                    let entries = &environment["anyOf"][0];
                    assert_eq!(entries["type"], "array");
                    assert_eq!(entries["items"]["type"], "object");
                    assert_eq!(entries["items"]["required"], json!(["name", "value"]));
                    assert_eq!(entries["items"]["additionalProperties"], false);
                    assert_eq!(entries["items"]["properties"]["name"]["type"], "string");
                    assert_eq!(entries["items"]["properties"]["value"]["type"], "string");
                    let request = harness::transport::ResponsesRequest {
                        input: vec![],
                        instructions: String::new(),
                        tools: snapshot.manifest.tools().clone(),
                        tools_allowed: None,
                        prompt_cache_comparison: None,
                        model: "test-model".into(),
                        pinned_effort: harness::model::Effort::Low,
                        session_id: "builtin-contract".into(),
                    };
                    harness::transport::client::request_body(&request).unwrap();
                    campaign
                        .actor
                        .shutdown(exomonad_actor::ActorTerminal {
                            kind: exomonad_actor::ActorExitKind::Completed,
                            summary: "builtin schema contract checked".into(),
                            diagnostic: None,
                        })
                        .await
                        .unwrap();
                    campaign.observe_hosted_completion().await.unwrap();
                })
            })
            .await;
    }

    #[test]
    fn unsupported_installed_schema_refuses_request_before_dispatch() {
        let unsupported = HostedTool::Function(ToolDeclaration {
            schedule: Default::default(),
            implementation: Default::default(),
            effect_keys: Vec::new(),
            name: "unsupported".into(),
            description: "Unsupported input constraint".into(),
            input_schema: json!({"type":"object","properties":{"value":{"type":"string","pattern":".*"}}}),
            output_schema: None,
            kind: ToolKind::Call,
        });
        assert!(validate_tools(std::slice::from_ref(&unsupported)).is_err());
        let installation = EmbeddedPolicyInstallation::new(
            exomonad_actor::ActorRef::first(exomonad_actor::ActorId(7)),
            policy_with_tools("never-dispatched", vec![unsupported]),
        );
        assert!(matches!(
            installation.request_snapshot(),
            Err(ResidentToolError::Unavailable(_))
        ));
    }

    #[tokio::test]
    async fn rejected_projection_preserves_issued_requests_and_allows_a_later_valid_surface() {
        let current = Arc::new(PromptOnlyReload {
            tools: declared_tools(),
            current: std::sync::Mutex::new(policy("A")),
        });
        let installation = EmbeddedPolicyInstallation::new(
            exomonad_actor::ActorRef::first(exomonad_actor::ActorId(7)),
            current.clone(),
        );
        let issued = installation.request_snapshot().unwrap();
        let mut invalid = declared_tools();
        let HostedTool::Function(tool) = &mut invalid[1] else {
            unreachable!()
        };
        tool.input_schema =
            json!({"type":"object","properties":{"value":{"type":"string","pattern":".*"}}});
        assert!(validate_tools(&invalid).is_err());
        // The pure admission check cannot mutate the active endpoint or an
        // already-issued projection. Publication remains the actor owner's job.
        let unchanged = installation.request_snapshot().unwrap();
        assert!(Arc::ptr_eq(&issued.manifest, &unchanged.manifest));
        assert_eq!(unchanged.request_instructions(), Some("A"));
        let mut valid = declared_tools();
        valid.remove(0);
        *current.current.lock().unwrap() = policy_with_tools("B", valid);
        let next = installation.request_snapshot().unwrap();
        assert_eq!(next.tools().len(), 1);
        assert_eq!(next.tools()[0]["name"], "lookup");
        assert_eq!(next.request_instructions(), Some("B"));
        let old = issued
            .dispatch(
                "haskell".into(),
                ToolArguments::Raw("retained".into()),
                context(),
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(old["marker"], "A");
    }

    #[tokio::test]
    async fn ambiguous_optional_null_arguments_are_refused_before_host_dispatch() {
        let ambiguous = HostedTool::Function(ToolDeclaration {
            schedule: Default::default(),
            implementation: Default::default(),
            effect_keys: Vec::new(),
            name: "ambiguous".into(),
            description: "Overlapping union input".into(),
            input_schema: json!({
                "type":"object","properties":{"choice":{"anyOf":[
                    {"type":"object","properties":{"value":{"type":"string"}},"required":[]},
                    {"type":"object","properties":{"value":{"type":["string","null"]}},"required":["value"]}
                ]}},"required":["choice"]
            }),
            output_schema: None,
            kind: ToolKind::Call,
        });
        let snapshot = EmbeddedPolicyInstallation::new(
            exomonad_actor::ActorRef::first(exomonad_actor::ActorId(7)),
            policy_with_tools("never-dispatched", vec![ambiguous]),
        )
        .request_snapshot()
        .unwrap();
        assert!(matches!(
            snapshot
                .dispatch(
                    "ambiguous".into(),
                    ToolArguments::Structured(json!({"choice":{"value":null}})),
                    context(),
                    None,
                    None
                )
                .await,
            Err(ResidentToolError::InvalidInvocation(_))
        ));
    }

    #[tokio::test]
    async fn request_manifest_and_handler_come_from_the_same_reloaded_endpoint() {
        let snapshot = EmbeddedPolicyInstallation::new(
            exomonad_actor::ActorRef::first(exomonad_actor::ActorId(7)),
            reloading_policy(),
        )
        .request_snapshot()
        .unwrap();

        assert_eq!(snapshot.tools().len(), 2);
        assert_eq!(snapshot.tools()[0]["name"], "haskell");
        let result = snapshot
            .dispatch(
                "haskell".into(),
                ToolArguments::Raw("1".into()),
                context(),
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(result["marker"], "issued-handler");
        assert_eq!(result["name"], "haskell");
    }

    #[tokio::test]
    async fn captured_transport_dispatches_both_input_kinds_after_new_installation() {
        let actor = exomonad_actor::ActorRef::first(exomonad_actor::ActorId(7));
        let first = EmbeddedPolicyInstallation::new(actor, policy("first"))
            .request_snapshot()
            .unwrap();
        let second = EmbeddedPolicyInstallation::new(actor, policy("second"))
            .request_snapshot()
            .unwrap();
        let raw = first
            .dispatch(
                "haskell".into(),
                ToolArguments::Raw("λ = 1".into()),
                context(),
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            raw,
            json!({
                "marker": "first", "name": "haskell", "context": "call-1",
                "arguments": {"raw": "λ = 1"},
            })
        );
        let structured = second
            .dispatch(
                "lookup".into(),
                ToolArguments::Structured(json!({"key": "x"})),
                context(),
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            structured,
            json!({
                "marker": "second", "name": "lookup", "context": "call-1",
                "arguments": {"structured": {"key": "x"}},
            })
        );
        assert!(matches!(
            first
                .dispatch(
                    "haskell".into(),
                    ToolArguments::Raw("again".into()),
                    context(),
                    None,
                    None,
                )
                .await,
            Ok(value) if value["marker"] == "first"
        ));
    }
}

#[cfg(test)]
#[path = "embedded_operation_settlement_tests.rs"]
pub(super) mod operation_settlement_tests;
