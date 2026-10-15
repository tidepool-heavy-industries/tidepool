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
}

impl EmbeddedPolicyInstallation {
    pub(super) fn from_installation(installation: &LocalResidentInstallation) -> Self {
        Self::with_observation(
            installation.actor.identity(),
            installation.policy.clone(),
            installation.runtime_observation.clone(),
        )
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
        }
    }

    pub(super) fn observation(&self) -> exomonad_actor::ActorRuntimeObservationHandle {
        self.observation.clone()
    }

    pub(super) fn actor(&self) -> ActorRef {
        self.actor
    }

    pub(super) fn complete(
        &self,
        boundary: tidepool_runtime::session::ContextCheckpointBoundary,
    ) -> ResidentToolFuture {
        self.policy.complete_boxed(boundary)
    }

    pub(super) fn abort(
        &self,
        boundary: tidepool_runtime::session::ContextCheckpointBoundary,
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
        Box::pin(async move {
            project_model_result(future.await?, &context).map_err(ResidentToolError::Encoding)
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
            .abort_boxed(
                tidepool_runtime::session::ContextCheckpointBoundary::Hosted(original.clone()),
            )
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

// This allowance bounds semantic data exposed to one model response, separately
// from the actor's equally sized presentation allowance: structured data cannot
// bypass the model output policy. Native receipts and afterTool retain
// the complete value. Failed candidates leave room for later useful small values.
const MODEL_SEMANTIC_VALUE_BYTE_BUDGET: usize = 28 * 1024;

fn bounded_model_value(
    value: &Value,
    budget: usize,
) -> Result<Option<(Value, usize)>, serde_json::Error> {
    struct Bytes {
        written: usize,
        budget: usize,
        exhausted: bool,
    }
    impl std::io::Write for Bytes {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.budget.saturating_sub(self.written) {
                self.exhausted = true;
                return Err(std::io::Error::other(
                    "model semantic value allowance exhausted",
                ));
            }
            self.written += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut bytes = Bytes {
        written: 0,
        budget,
        exhausted: false,
    };
    match serde_json::to_writer(&mut bytes, value) {
        Ok(()) => Ok(Some((value.clone(), bytes.written))),
        Err(_) if bytes.exhausted => Ok(None),
        Err(error) => Err(error),
    }
}

fn project_model_result(
    response: exomonad_actor::ResidentToolResponse,
    context: &ToolInvocationContext,
) -> Result<Value, serde_json::Error> {
    use tidepool_runtime::session::{
        WorkbenchDisplayPublication, WorkbenchItemStatus, WorkbenchPublicationOutcome,
        WorkbenchRunStatus,
    };
    let exomonad_actor::ResidentToolResponse::Workbench(mut response) = response else {
        return response.into_json();
    };
    let successful = matches!(response.status, WorkbenchRunStatus::Committed)
        && matches!(
            response.publication,
            Some(WorkbenchPublicationOutcome::Published { .. })
        );
    // Move semantic values out before serializing the response: serializing the
    // full response first would allocate every oversized value just to delete it.
    let values: Vec<_> = response
        .items
        .iter_mut()
        .map(|item| item.value.take())
        .collect();
    let mut projected = serde_json::to_value(&response)?;
    let mut remaining = MODEL_SEMANTIC_VALUE_BYTE_BUDGET;
    for (value, encoded) in values
        .into_iter()
        .zip(projected["items"].as_array_mut().unwrap())
    {
        let Some(value) = value else { continue };
        match bounded_model_value(&value, remaining)? {
            Some((value, bytes)) => {
                encoded["value"] = value;
                remaining -= bytes;
            }
            None => {
                encoded["valueOmitted"] = json!({
                    "reason": "modelBudget",
                    "limitBytes": MODEL_SEMANTIC_VALUE_BYTE_BUDGET,
                    "nativeReceipt": {"availability": "nativeOnly", "operation": context},
                });
            }
        }
    }
    if successful {
        let Some(items) = projected.get_mut("items").and_then(Value::as_array_mut) else {
            return Ok(projected);
        };
        for (position, (item, encoded)) in response.items.iter().zip(items).enumerate() {
            if item.status != WorkbenchItemStatus::Committed {
                continue;
            }
            for (operation, encoded) in item.operations.iter().zip(
                encoded
                    .get_mut("operations")
                    .and_then(Value::as_array_mut)
                    .into_iter()
                    .flatten(),
            ) {
                if matches!(
                    operation.display_publication,
                    Some(WorkbenchDisplayPublication::Published { .. })
                ) && operation
                    .display
                    .as_ref()
                    .is_some_and(|display| item.output.contains(&display.text))
                {
                    // Settlement appends exact pages, but afterTool may prune item.output.
                    // Reference it only while the entire page remains in the final output.
                    if let Some(display) = encoded.get_mut("display").and_then(Value::as_object_mut)
                    {
                        display.remove("text");
                        display.insert(
                            "textSource".into(),
                            json!(format!("/items/{position}/output")),
                        );
                    }
                }
            }
        }
    }
    Ok(projected)
}

/// Validate the transport contract before a generation can be published.
pub(super) fn validate_tools(tools: &[HostedTool]) -> Result<(), String> {
    project_tools(tools).map(|_| ())
}

fn project_tools(tools: &[HostedTool]) -> Result<EmbeddedToolProjection, String> {
    let mut projected = Vec::with_capacity(tools.len());
    let mut schemas = HashMap::new();
    let mut scheduling = HashMap::new();
    for tool in tools {
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
    let manifest = harness::embedding::EmbeddedToolManifest::with_scheduling(projected, scheduling)
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
    fn model_success_projects_display_content_once_and_preserves_receipt_facts() {
        use tidepool_runtime::session::*;
        let mut response = display_response();
        let mut declaration = response.items[0].clone();
        declaration.index = 1;
        declaration.kind = Some(WorkbenchCellItemKind::Declaration);
        declaration.output = "accepted declaration".into();
        declaration.operations.clear();
        response.items.push(declaration);
        response.next_index = 2;
        response.total = 2;
        let page = WorkbenchDisplayPublicationIdentity {
            display: (1, 2, 3),
            page_ordinal: 1,
        };
        let full = serde_json::to_value(&response).unwrap();
        assert!(full["items"][1].get("operations").is_none());
        let projected = project_model_result(exomonad_actor::ResidentToolResponse::Workbench(
            response.clone(),
        ))
        .unwrap();
        let mut expected = full.clone();
        expected["items"][0]["operations"][0]["display"]
            .as_object_mut()
            .unwrap()
            .remove("text");
        expected["items"][0]["operations"][0]["display"]["textSource"] = json!("/items/0/output");
        assert_eq!(projected, expected);
        assert!(projected["items"][1].get("operations").is_none());
        assert_eq!(
            full["items"][0]["operations"][0]["display"]["text"],
            "observed value"
        );

        let mut pruned = response.clone();
        pruned.items[0].output = "selected summary".into();
        assert_eq!(
            project_model_result(exomonad_actor::ResidentToolResponse::Workbench(
                pruned.clone()
            ))
            .unwrap(),
            serde_json::to_value(pruned).unwrap()
        );

        // Partial execution and uncertain publication retain the entire receipt.
        for publication in [
            WorkbenchPublicationOutcome::NotPublished {
                reason: WorkbenchNotPublishedReason::Failed,
            },
            WorkbenchPublicationOutcome::RecoveryRequired {
                detail: "publication uncertain".into(),
            },
        ] {
            let mut partial = response.clone();
            partial.publication = Some(publication);
            assert_eq!(
                project_model_result(exomonad_actor::ResidentToolResponse::Workbench(
                    partial.clone()
                ))
                .unwrap(),
                serde_json::to_value(partial).unwrap()
            );
        }
        let mut uncertain = response;
        uncertain.items[0].operations[0].display_publication =
            Some(WorkbenchDisplayPublication::Unconfirmed {
                publication: page,
                detail: "store outcome unknown".into(),
            });
        assert_eq!(
            project_model_result(exomonad_actor::ResidentToolResponse::Workbench(
                uncertain.clone()
            ))
            .unwrap(),
            serde_json::to_value(uncertain).unwrap()
        );
    }

    #[test]
    fn semantic_projection_distinguishes_absent_null_and_omitted_values() {
        let mut response = display_response();
        let template = response.items[0].clone();
        let values = [
            None,
            Some(Value::Null),
            Some(json!("λ".repeat(70000))),
            Some(json!({"answer":42})),
        ];
        response.items = values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let mut item = template.clone();
                item.index = index;
                item.value = value.clone();
                item
            })
            .collect();
        let projected = project_model_result(exomonad_actor::ResidentToolResponse::Workbench(
            response.clone(),
        ))
        .unwrap();
        let items = projected["items"].as_array().unwrap();
        assert!(items[0].get("value").is_none());
        assert!(items[0].get("valueOmitted").is_none());
        assert_eq!(items[1].get("value"), Some(&Value::Null));
        assert!(items[1].get("valueOmitted").is_none());
        assert!(items[2].get("value").is_none());
        assert_eq!(
            items[2]["valueOmitted"],
            json!({
                "reason":"modelBudget", "limitBytes":MODEL_SEMANTIC_VALUE_BYTE_BUDGET,
            "nativeReceipt":{"availability":"nativeOnly","operation":context()},
            })
        );
        assert_eq!(
            items[3]["value"],
            json!({"answer":42}),
            "failed serialization must not spend the allowance"
        );
        assert_eq!(response.items[2].value.as_ref(), values[2].as_ref());
        for item in items {
            assert_eq!(item["output"], template.output);
        }
    }

    #[test]
    fn semantic_projection_respects_complete_utf8_and_json_boundaries() {
        let value = json!({"text":"λ🦀\n\"\\"});
        let encoded = serde_json::to_vec(&value).unwrap();
        for limit in 0..=encoded.len() + 1 {
            let selected = bounded_model_value(&value, limit).unwrap();
            if limit < encoded.len() {
                assert!(selected.is_none(), "partial JSON at limit {limit}");
            } else {
                assert_eq!(selected, Some((value.clone(), encoded.len())));
            }
        }
    }

    #[test]
    fn semantic_projection_keeps_small_deep_native_values_without_redecoding() {
        let value = (0..192).fold(json!(42), |value, _| json!({"nested":value}));
        let encoded = serde_json::to_vec(&value).unwrap();
        assert!(encoded.len() < MODEL_SEMANTIC_VALUE_BYTE_BUDGET);
        // Projection already has a typed Value; the text decoder's depth policy
        // must not turn a completed native result into an observation failure.
        assert!(serde_json::from_slice::<Value>(&encoded).is_err());
        assert_eq!(
            bounded_model_value(&value, MODEL_SEMANTIC_VALUE_BYTE_BUDGET).unwrap(),
            Some((value, encoded.len()))
        );
    }

    #[test]
    fn semantic_projection_cannot_multiply_the_allowance_by_item_count() {
        let mut response = display_response();
        let value = json!("λ".repeat(256));
        let bytes = serde_json::to_vec(&value).unwrap().len();
        assert!(bytes < MODEL_SEMANTIC_VALUE_BYTE_BUDGET);
        let mut item = response.items[0].clone();
        item.value = Some(value.clone());
        response.items = (0..128)
            .map(|index| {
                let mut item = item.clone();
                item.index = index;
                item
            })
            .collect();
        let projected =
            project_model_result(exomonad_actor::ResidentToolResponse::Workbench(response))
                .unwrap();
        let items = projected["items"].as_array().unwrap();
        let kept = items
            .iter()
            .filter(|item| item.get("value").is_some())
            .count();
        assert_eq!(kept, MODEL_SEMANTIC_VALUE_BYTE_BUDGET / bytes);
        assert!(kept < items.len());
        for item in items {
            if let Some(visible) = item.get("value") {
                assert_eq!(visible, &value);
                assert!(item.get("valueOmitted").is_none());
            } else {
                assert_eq!(item["valueOmitted"]["reason"], "modelBudget");
            }
        }
    }

    proptest::proptest! {
        #[test]
        fn semantic_projection_uses_one_allowance_for_arbitrary_item_batches(
            strings in proptest::collection::vec(proptest::option::of("(?s).{0,2048}"), 0..64),
        ) {
            let mut response = display_response();
            let template = response.items[0].clone();
            response.items = strings.iter().enumerate().map(|(index, value)| {
                let mut item = template.clone();
                item.index = index;
                item.value = value.as_ref().map(|text| json!({"text":text}));
                item
            }).collect();
            let projected = project_model_result(exomonad_actor::ResidentToolResponse::Workbench(response.clone())).unwrap();
            let mut remaining = MODEL_SEMANTIC_VALUE_BYTE_BUDGET;
            let mut visible_bytes = 0;
            for (item, encoded) in response.items.iter().zip(projected["items"].as_array().unwrap()) {
                if let Some(value) = &item.value {
                    let bytes = serde_json::to_vec(value).unwrap().len();
                    if bytes <= remaining {
                        proptest::prop_assert_eq!(encoded.get("value"), Some(value));
                        proptest::prop_assert!(encoded.get("valueOmitted").is_none());
                        remaining -= bytes;
                        visible_bytes += bytes;
                    } else {
                        proptest::prop_assert!(encoded.get("value").is_none());
                        proptest::prop_assert_eq!(&encoded["valueOmitted"]["reason"], "modelBudget");
                    }
                } else {
                    proptest::prop_assert!(encoded.get("value").is_none());
                    proptest::prop_assert!(encoded.get("valueOmitted").is_none());
                }
            }
            proptest::prop_assert!(visible_bytes <= MODEL_SEMANTIC_VALUE_BYTE_BUDGET);
        }

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
                // Resolve the model's reference or inline value, then verify full page reachability.
                let visible = if let Some(pointer) = encoded["display"]["textSource"].as_str() {
                    projected.pointer(pointer).and_then(Value::as_str).unwrap()
                } else {
                    encoded["display"]["text"].as_str().unwrap()
                };
                proptest::prop_assert!(visible.contains(&display.text));
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
