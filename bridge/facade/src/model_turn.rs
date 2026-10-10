//! Host-bound, per-cell adapter for the Engine's bounded invocation profile.
use harness::{
    engine::{EngineConfig, ResponsesTransport},
    invocation::{
        Callback, CellBudget, Hook, HookAnnotation, Invocation, InvocationCloseHandle,
        InvocationOptions, Limits, Step,
    },
    item::Item,
    model::{AgentPath, Effort, OperationId},
    provider::ToolFailure,
    store::Store,
    transport::Auth,
    turn::JobScheduler,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    marker::PhantomData,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
};
use tidepool_handlers::handlers::model::{ModelBoundaryError, ModelService};
use tidepool_repr::PrincipalId;
use tokio::runtime::Handle;

/// Choices admitted by the host; authored requests can only select these models
/// and efforts, and can only narrow the shared allowance.
/// Narrower limits apply to this invocation; nested calls have their own limits
/// and share the cell allowance. Any exhausted scope latches the cell breaker.
#[derive(Clone)]
pub struct ModelPolicy {
    pub default_model: String,
    pub models: Vec<String>,
    pub default_effort: Effort,
    pub efforts: Vec<Effort>,
    pub limits: Limits,
}
use tidepool_bridge_effects::{
    ModelAnnotationEnvelope, ModelControlStep, ModelEffortEnvelope, ModelRequestEnvelope,
};
enum Pending {
    Callback(Callback),
    Hook { hook: Hook, handle: String },
}
struct Entry {
    invocation: Invocation,
    pending: Option<Pending>,
    calls: HashMap<OperationId, (String, Value)>,
    closing: Arc<AtomicBool>,
}
struct Registered {
    entry: Arc<Mutex<Entry>>,
    close: InvocationCloseHandle,
    closing: Arc<AtomicBool>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetainedResult {
    request: harness::model::RequestId,
    ordinal: i64,
}
const RESULT_HANDLE_PREFIX: &str = "model-tool-result:";
/// Resolve a retained result against its owning Store after the invocation has
/// finished. The handle names the durable result event, never a live registry.
pub fn retained_model_result(
    store: &Store,
    handle: &str,
) -> Result<Option<Item>, ModelBoundaryError> {
    let reference: RetainedResult = serde_json::from_str(
        handle
            .strip_prefix(RESULT_HANDLE_PREFIX)
            .ok_or_else(|| rejected("invalid retained model result handle"))?,
    )
    .map_err(|error| rejected(error.to_string()))?;
    let events = store
        .events(Some(&reference.request))
        .map_err(|error| transport_error(error.to_string()))?;
    let Some(event) = events
        .into_iter()
        .find(|event| event.id == reference.ordinal && event.kind == "model_tool_result")
    else {
        return Ok(None);
    };
    let payload: Value =
        serde_json::from_str(&event.payload).map_err(|error| transport_error(error.to_string()))?;
    if payload["operation"]["request"] != json!(reference.request) {
        return Err(transport_error("retained result request identity mismatch"));
    }
    Ok(payload.get("output").cloned().map(Item))
}
/// Construct once for each admitted cell. Clones must share this value through
/// an Arc, so nested and concurrent calls share budget and authority.
pub struct CellModelService<A, C> {
    runtime: Handle,
    principal: PrincipalId,
    parent_cell: String,
    store: Arc<Store>,
    scheduler: Arc<JobScheduler>,
    policy: ModelPolicy,
    transport: Arc<dyn Fn() -> C + Send + Sync>,
    budget: OnceLock<CellBudget>,
    accepting: AtomicBool,
    registry: Mutex<HashMap<String, Registered>>,
    auth: PhantomData<fn() -> A>,
}
fn rejected(message: impl Into<String>) -> ModelBoundaryError {
    ModelBoundaryError::ModelRejected(message.into())
}
fn transport_error(message: impl Into<String>) -> ModelBoundaryError {
    ModelBoundaryError::ModelTransportFailed(message.into())
}
fn validate_tools(tools: &[exomonad_tool::ToolDeclaration]) -> Result<(), ModelBoundaryError> {
    for tool in tools {
        if tool.implementation == exomonad_tool::ToolImplementation::HaskellCell {
            return Err(rejected(
                "bounded model calls do not admit HaskellCell tools",
            ));
        }
        if tool.schedule == exomonad_tool::ToolScheduling::BeforeNextInference {
            return Err(rejected(
                "bounded model calls do not admit BeforeNextInference tools",
            ));
        }
        if tool
            .effect_keys
            .contains(&exomonad_tool::ToolEffectKey::ContextReadWrite)
        {
            return Err(rejected(
                "bounded model calls do not admit ContextReadWrite tools",
            ));
        }
        if !matches!(
            tool.kind,
            exomonad_tool::ToolKind::Call | exomonad_tool::ToolKind::Notify
        ) {
            return Err(rejected(
                "bounded model calls support only Call and Notify tools",
            ));
        }
        if tool.input_schema["type"] != "object" {
            return Err(rejected(
                "bounded model tool inputs must have an object schema",
            ));
        }
    }
    Ok(())
}
impl<A: Auth + 'static, C: ResponsesTransport + 'static> CellModelService<A, C> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        runtime: Handle,
        principal: PrincipalId,
        parent_cell: String,
        store: Arc<Store>,
        scheduler: Arc<JobScheduler>,
        policy: ModelPolicy,
        transport: Arc<dyn Fn() -> C + Send + Sync>,
    ) -> Self {
        Self {
            runtime,
            principal,
            parent_cell,
            store,
            scheduler,
            policy,
            transport,
            budget: OnceLock::new(),
            accepting: AtomicBool::new(true),
            registry: Mutex::new(HashMap::new()),
            auth: PhantomData,
        }
    }
    /// Called by the admitted execution owner on cancellation or disposal.
    /// Stops admission and signals every invocation without waiting for callbacks.
    /// Already admitted callbacks may still submit their actual outcomes.
    pub fn cancel(&self) {
        let registry = self.registry.lock().unwrap();
        self.accepting.store(false, Ordering::Release);
        let entries = registry
            .values()
            .map(|registered| {
                registered.closing.store(true, Ordering::Release);
                registered.close.close();
                registered.entry.clone()
            })
            .collect::<Vec<_>>();
        drop(registry);
        for entry in entries {
            if let Ok(mut entry) = entry.try_lock() {
                if matches!(entry.pending, Some(Pending::Hook { .. })) {
                    if let Some(Pending::Hook { hook, .. }) = entry.pending.take() {
                        let _ = hook.complete(HookAnnotation::NoAnnotation);
                    }
                }
            }
        }
    }
    /// The cell owner calls this after its native callback can no longer resume.
    /// Signal-only cancellation cannot abandon a handed-out callback safely.
    pub fn settle(&self) -> Result<(), ModelBoundaryError> {
        self.cancel();
        let entries = self
            .registry
            .lock()
            .unwrap()
            .iter()
            .map(|(token, registered)| (token.clone(), registered.entry.clone()))
            .collect::<Vec<_>>();
        for (_, entry) in &entries {
            let mut entry = entry.lock().unwrap();
            match entry.pending.take() {
                Some(Pending::Callback(callback)) => {
                    // Only this still-unanswered continuation is abandoned. A
                    // callback that already completed has left `pending`.
                    let _ = callback.complete(Err("cell ended before callback completion".into()));
                }
                Some(Pending::Hook { hook, .. }) => {
                    let _ = hook.complete(HookAnnotation::NoAnnotation);
                }
                None => {}
            }
        }
        let mut failure = None;
        for (token, entry) in entries {
            let mut entry = entry.lock().unwrap();
            if let Err(error) = self.next(&token, &mut entry) {
                if failure.is_none() {
                    failure = Some(error);
                }
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
    fn authorize(&self, caller: PrincipalId) -> Result<(), ModelBoundaryError> {
        if caller != self.principal {
            return Err(rejected("model service belongs to another principal"));
        }
        Ok(())
    }
    fn entry(
        &self,
        caller: PrincipalId,
        token: &str,
    ) -> Result<Arc<Mutex<Entry>>, ModelBoundaryError> {
        self.authorize(caller)?;
        self.registry
            .lock()
            .unwrap()
            .get(token)
            .map(|registered| registered.entry.clone())
            .ok_or_else(|| rejected("unknown or finished model invocation"))
    }
    fn next(&self, token: &str, entry: &mut Entry) -> Result<Value, ModelBoundaryError> {
        match self.runtime.block_on(entry.invocation.next()) {
            Step::Callback(callback) => {
                if entry.closing.load(Ordering::Acquire) {
                    callback
                        .complete(Err("invocation closed before callback admission".into()))
                        .map_err(|_| transport_error("callback continuation closed"))?;
                    return self.next(token, entry);
                }
                let call_id = serde_json::to_string(&callback.operation)
                    .map_err(|error| transport_error(error.to_string()))?;
                let value = serde_json::to_value(ModelControlStep::ModelCallback {
                    invocation: token.to_owned(),
                    call_id,
                    name: callback.name.clone(),
                    arguments: callback.arguments.clone(),
                })
                .map_err(|error| transport_error(error.to_string()))?;
                entry.calls.insert(
                    callback.operation.clone(),
                    (callback.name.clone(), callback.arguments.clone()),
                );
                entry.pending = Some(Pending::Callback(callback));
                Ok(value)
            }
            Step::HookRequested(hook) => {
                if entry.closing.load(Ordering::Acquire) {
                    hook.complete(HookAnnotation::NoAnnotation)
                        .map_err(|_| transport_error("hook continuation closed"))?;
                    return self.next(token, entry);
                }
                let Some((name, arguments)) = entry.calls.get(&hook.operation) else {
                    entry.invocation.close();
                    self.registry.lock().unwrap().remove(token);
                    return Err(transport_error("hook has no admitted callback"));
                };
                let operation = serde_json::to_string(&hook.operation)
                    .map_err(|error| transport_error(error.to_string()))?;
                let handle = format!(
                    "{RESULT_HANDLE_PREFIX}{}",
                    serde_json::to_string(&RetainedResult {
                        request: hook.operation.request.clone(),
                        ordinal: hook.ordinal
                    })
                    .map_err(|error| transport_error(error.to_string()))?
                );
                let output = hook.output.0["output"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| hook.output.0["output"].to_string());
                let semantic = serde_json::from_str(&output).map_err(|error| {
                    transport_error(format!("retained callback output is not JSON: {error}"))
                })?;
                let value = serde_json::to_value(ModelControlStep::ModelHook {
                    invocation: token.to_owned(),
                    operation,
                    name: name.clone(),
                    arguments: arguments.clone(),
                    handle: handle.clone(),
                    ordinal: hook.ordinal,
                    value: semantic,
                    output,
                })
                .map_err(|error| transport_error(error.to_string()))?;
                entry.pending = Some(Pending::Hook { hook, handle });
                Ok(value)
            }
            Step::Finished(receipt) => {
                let retained = json!(receipt);
                let events = self
                    .store
                    .events(receipt.requests.last())
                    .map_err(|error| transport_error(error.to_string()))?;
                let confirmed = events
                    .iter()
                    .filter(|event| event.kind == "model_invocation_receipt")
                    .any(|event| {
                        serde_json::from_str::<Value>(&event.payload)
                            .is_ok_and(|payload| payload == retained)
                    });
                if !confirmed {
                    return Err(transport_error(
                        "original model invocation receipt is not retained",
                    ));
                }
                self.registry.lock().unwrap().remove(token);
                let receipt = serde_json::from_value(retained)
                    .map_err(|error| transport_error(error.to_string()))?;
                serde_json::to_value(ModelControlStep::ModelFinished { receipt })
                    .map_err(|error| transport_error(error.to_string()))
            }
        }
    }
}
impl<A: Auth + 'static, C: ResponsesTransport + 'static> ModelService for CellModelService<A, C> {
    fn start(&self, caller: PrincipalId, request: Value) -> Result<Value, ModelBoundaryError> {
        self.authorize(caller)?;
        let request: ModelRequestEnvelope =
            serde_json::from_value(request).map_err(|error| rejected(error.to_string()))?;
        let model = request
            .model
            .unwrap_or_else(|| self.policy.default_model.clone());
        let effort = request
            .effort
            .map(|effort| match effort {
                ModelEffortEnvelope::ModelLowEffort => Effort::Low,
                ModelEffortEnvelope::ModelMediumEffort => Effort::Medium,
                ModelEffortEnvelope::ModelHighEffort => Effort::High,
            })
            .unwrap_or(self.policy.default_effort);
        if !self.policy.models.contains(&model) || !self.policy.efforts.contains(&effort) {
            return Err(rejected("model or effort is outside admitted host policy"));
        }
        let limits = Limits {
            requests: request
                .limits
                .requests
                .unwrap_or(self.policy.limits.requests),
            tools: request.limits.tools.unwrap_or(self.policy.limits.tools),
            reported_tokens: request
                .limits
                .reported_tokens
                .unwrap_or(self.policy.limits.reported_tokens),
            seconds: request.limits.seconds.unwrap_or(self.policy.limits.seconds),
        };
        validate_tools(&request.tools)?;
        let tools = request.tools.into_iter().map(|tool| json!({"type":"function","name":tool.name,"description":tool.description,"parameters":tool.input_schema,"strict":true})).collect();
        let config = EngineConfig {
            instructions: request.instructions,
            tools,
            model,
            effort,
            session_id: String::new(),
            agent: AgentPath(String::new()),
        };
        let initial = vec![Item(
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":request.input}]}),
        )];
        let budget = self
            .budget
            .get_or_init(|| CellBudget::new(self.policy.limits))
            .narrow(limits);
        // Admission and cancellation share this short lock; cancellation cannot
        // miss an invocation between its creation and registry publication.
        let mut registry = self.registry.lock().unwrap();
        if !self.accepting.load(Ordering::Acquire) {
            return Err(rejected("calling cell's model service was cancelled"));
        }
        let invocation = {
            let _entered = self.runtime.enter();
            Invocation::start::<A, C>(
                (self.transport)(),
                self.store.clone(),
                self.scheduler.clone(),
                config,
                self.parent_cell.clone(),
                budget,
                initial,
                InvocationOptions {
                    result_schema: request.result_schema,
                    after_tool: request.after_tool,
                },
            )
        }
        .map_err(|error| rejected(error.to_string()))?;
        let token = uuid::Uuid::new_v4().to_string();
        let close = invocation.close_handle();
        let closing = Arc::new(AtomicBool::new(false));
        let entry = Arc::new(Mutex::new(Entry {
            invocation,
            pending: None,
            calls: HashMap::new(),
            closing: closing.clone(),
        }));
        registry.insert(
            token.clone(),
            Registered {
                entry: entry.clone(),
                close,
                closing,
            },
        );
        drop(registry);
        let mut locked = entry.lock().unwrap();
        self.next(&token, &mut locked)
    }
    fn resume(
        &self,
        caller: PrincipalId,
        token: &str,
        call_id: &str,
        answer: Value,
    ) -> Result<Value, ModelBoundaryError> {
        let entry = self.entry(caller, token)?;
        let mut entry = entry.lock().unwrap();
        let operation: OperationId =
            serde_json::from_str(call_id).map_err(|error| rejected(error.to_string()))?;
        if !matches!(&entry.pending, Some(Pending::Callback(callback)) if callback.operation == operation)
        {
            return Err(rejected(
                "callback identity does not match pending continuation",
            ));
        }
        let reply: exomonad_actor::ToolDispatchReply = serde_json::from_value(answer)
            .map_err(|error| rejected(format!("invalid callback reply: {error}")))?;
        let Some(Pending::Callback(callback)) = &entry.pending else {
            unreachable!()
        };
        let result = match reply.into_output() {
            Ok(output) => Ok(output),
            Err(error) => {
                if error.tool() != callback.name {
                    return Err(rejected("callback refusal names another tool"));
                }
                let metadata = serde_json::to_value(&error)
                    .map_err(|error| transport_error(error.to_string()))?;
                Err(ToolFailure::with_metadata(error.to_string(), metadata))
            }
        };
        let Some(Pending::Callback(callback)) = entry.pending.take() else {
            unreachable!()
        };
        callback
            .complete(result)
            .map_err(|_| transport_error("callback continuation closed"))?;
        self.next(token, &mut entry)
    }
    fn annotate(
        &self,
        caller: PrincipalId,
        token: &str,
        operation: &str,
        annotation: Value,
    ) -> Result<Value, ModelBoundaryError> {
        let entry = self.entry(caller, token)?;
        let mut entry = entry.lock().unwrap();
        let Some(Pending::Hook { hook, handle }) = &entry.pending else {
            return Err(rejected("no pending after-tool hook"));
        };
        if serde_json::to_string(&hook.operation)
            .map_err(|error| transport_error(error.to_string()))?
            != operation
        {
            return Err(rejected("hook operation identity mismatch"));
        }
        let annotation: ModelAnnotationEnvelope =
            serde_json::from_value(annotation).map_err(|error| rejected(error.to_string()))?;
        let annotation = match annotation {
            ModelAnnotationEnvelope::ModelNoAnnotation => HookAnnotation::NoAnnotation,
            ModelAnnotationEnvelope::ModelAbstained { reason } => HookAnnotation::Abstained(reason),
            ModelAnnotationEnvelope::ModelAnnotated { text } => HookAnnotation::Annotated(text),
            ModelAnnotationEnvelope::ModelPruned {
                handle: retained,
                text,
            } if retained == *handle => HookAnnotation::Pruned {
                replacement: Value::String(text),
                annotation: String::new(),
            },
            ModelAnnotationEnvelope::ModelPruned { .. } => {
                return Err(rejected("foreign retained handle"))
            }
        };
        let Some(Pending::Hook { hook, .. }) = entry.pending.take() else {
            unreachable!()
        };
        hook.complete(annotation)
            .map_err(|_| transport_error("hook continuation closed"))?;
        self.next(token, &mut entry)
    }
    fn close(&self, caller: PrincipalId, token: &str) -> Result<(), ModelBoundaryError> {
        self.authorize(caller)?;
        let registry = self.registry.lock().unwrap();
        let registered = registry
            .get(token)
            .ok_or_else(|| rejected("unknown or finished model invocation"))?;
        registered.closing.store(true, Ordering::Release);
        registered.close.close();
        let entry = registered.entry.clone();
        drop(registry);
        if let Ok(mut entry) = entry.try_lock() {
            if matches!(entry.pending, Some(Pending::Hook { .. })) {
                let Some(Pending::Hook { hook, .. }) = entry.pending.take() else {
                    unreachable!()
                };
                hook.complete(HookAnnotation::NoAnnotation)
                    .map_err(|_| transport_error("hook continuation closed"))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use harness::transport::{ResponsesRequest, ResponsesTurn, TransportError, Usage};
    use std::collections::VecDeque;
    #[test]
    fn export_production_effect_core_for_haskell_check() {
        let source = tidepool_mcp::effects_core_module_source();
        assert!(source.contains("data ModelCall"));
        if let Some(directory) = std::env::var_os("TIDEPOOL_MODEL_HASKELL_CHECK_DIR") {
            let path = std::path::PathBuf::from(directory).join("Tidepool/Effects/Core.hs");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, source).unwrap();
        }
    }
    struct TestAuth;
    impl Auth for TestAuth {
        fn access(&self) -> Result<(String, String), TransportError> {
            unreachable!()
        }
    }
    struct Script(Mutex<VecDeque<ResponsesTurn>>);
    #[async_trait]
    impl ResponsesTransport for Script {
        async fn create(&self, _: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
            self.0
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| TransportError::Stream("script exhausted".into()))
        }
    }
    fn turn(items: Vec<Item>) -> ResponsesTurn {
        ResponsesTurn {
            response_id: uuid::Uuid::new_v4().to_string(),
            items,
            usage: Usage {
                reported: true,
                input_tokens: 1,
                output_tokens: 1,
                ..Usage::default()
            },
        }
    }
    fn final_turn() -> ResponsesTurn {
        turn(vec![Item(
            json!({"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"done"}]}),
        )])
    }
    fn callback() -> ResponsesTurn {
        turn(vec![Item(
            json!({"type":"function_call","call_id":"call-1","name":"echo","arguments":"{}"}),
        )])
    }
    fn callback_reply(output: Value) -> Value {
        json!({"status":"success","output":output})
    }
    fn request(hook: bool) -> Value {
        json!({"instructions":"offline","input":"run","model":null,"effort":null,"limits":{"requests":null,"tools":null,"reported_tokens":null,"seconds":null},"tools":[{"name":"echo","description":"echo","kind":"call","inputSchema":{"type":"object","properties":{},"required":[],"additionalProperties":false},"outputSchema":null}],"result_schema":null,"after_tool":hook})
    }
    fn service(
        runtime: &tokio::runtime::Runtime,
        scripts: Vec<Vec<ResponsesTurn>>,
        limits: Limits,
    ) -> CellModelService<TestAuth, Script> {
        let scripts = Arc::new(Mutex::new(VecDeque::from(scripts)));
        CellModelService::new(
            runtime.handle().clone(),
            PrincipalId::new(1, 2),
            "retained-cell".into(),
            Arc::new(Store::memory().unwrap()),
            Arc::new(JobScheduler::new(1).unwrap()),
            ModelPolicy {
                default_model: "offline".into(),
                models: vec!["offline".into()],
                default_effort: Effort::Low,
                efforts: vec![Effort::Low],
                limits,
            },
            Arc::new(move || {
                Script(Mutex::new(
                    scripts.lock().unwrap().pop_front().unwrap().into(),
                ))
            }),
        )
    }
    #[test]
    fn settlement_abandons_unanswered_callbacks_without_waiting_for_binding_drop() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let owner = Arc::new(service(
            &runtime,
            vec![vec![callback(), final_turn()]],
            Limits::default(),
        ));
        let retained = owner.clone();
        let caller = PrincipalId::new(1, 2);
        let first = owner.start(caller, request(false)).unwrap();
        let operation: OperationId =
            serde_json::from_str(first["call_id"].as_str().unwrap()).unwrap();
        owner.cancel();
        assert!(owner
            .store
            .events(None)
            .unwrap()
            .iter()
            .all(|event| event.kind != "model_invocation_receipt"));
        assert!(matches!(
            owner
                .entry(caller, first["invocation"].as_str().unwrap())
                .unwrap()
                .lock()
                .unwrap()
                .pending,
            Some(Pending::Callback(_))
        ));
        owner.settle().unwrap();
        assert_eq!(Arc::strong_count(&owner), 2);
        assert!(retained.registry.lock().unwrap().is_empty());
        let receipts = retained
            .store
            .events(None)
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == "model_invocation_receipt")
            .collect::<Vec<_>>();
        assert_eq!(receipts.len(), 1);
        assert_eq!(
            serde_json::from_str::<Value>(&receipts[0].payload).unwrap()["outcome"]["kind"],
            "cancelled"
        );
        assert!(retained
            .store
            .replay_tool_output_operation(&operation)
            .unwrap()
            .is_some());
        assert!(retained.start(caller, request(false)).is_err());
        retained.settle().unwrap();
        assert_eq!(
            retained
                .store
                .events(None)
                .unwrap()
                .iter()
                .filter(|event| event.kind == "model_invocation_receipt")
                .count(),
            1
        );

        let completed = service(
            &runtime,
            vec![vec![callback(), final_turn()]],
            Limits::default(),
        );
        let first = completed.start(caller, request(false)).unwrap();
        let operation: OperationId =
            serde_json::from_str(first["call_id"].as_str().unwrap()).unwrap();
        completed
            .resume(
                caller,
                first["invocation"].as_str().unwrap(),
                first["call_id"].as_str().unwrap(),
                callback_reply(json!("actual completed result")),
            )
            .unwrap();
        let original = completed
            .store
            .replay_tool_output_operation(&operation)
            .unwrap()
            .unwrap();
        completed.settle().unwrap();
        assert_eq!(
            completed
                .store
                .replay_tool_output_operation(&operation)
                .unwrap()
                .unwrap(),
            original
        );
        assert_eq!(original.terminal, harness::store::TerminalOutcome::Success);
    }
    #[test]
    fn malformed_callback_envelopes_do_not_consume_the_original_continuation() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let service = service(
            &runtime,
            vec![vec![callback(), final_turn()]],
            Limits::default(),
        );
        let caller = PrincipalId::new(1, 2);
        let first = service.start(caller, request(false)).unwrap();
        let token = first["invocation"].as_str().unwrap();
        let call = first["call_id"].as_str().unwrap();
        for invalid in [
            json!({"Right":"accidental Either encoding"}),
            json!({"Left":{"UnknownTool":"echo"}}),
            json!({"status":"refused","kind":"invented","error":"bad","tool":"echo"}),
            json!({"status":"refused","kind":"invalid_input","error":"bad","tool":"echo"}),
            json!({"status":"refused","kind":"unknown_tool","error":"bad","tool":"foreign"}),
        ] {
            assert!(matches!(
                service.resume(caller, token, call, invalid),
                Err(ModelBoundaryError::ModelRejected(_))
            ));
            assert!(matches!(
                service
                    .entry(caller, token)
                    .unwrap()
                    .lock()
                    .unwrap()
                    .pending,
                Some(Pending::Callback(_))
            ));
        }
        let authored = json!({"error":"ordinary authored output","status":"refused"});
        let finished = service
            .resume(caller, token, call, callback_reply(authored.clone()))
            .unwrap();
        assert_eq!(finished["receipt"]["outcome"]["kind"], "text");
        let operation: OperationId = serde_json::from_str(call).unwrap();
        let recorded = service
            .store
            .replay_tool_output_operation(&operation)
            .unwrap()
            .unwrap();
        assert_eq!(recorded.terminal, harness::store::TerminalOutcome::Success);
        assert_eq!(
            serde_json::from_str::<Value>(recorded.item.0["output"].as_str().unwrap()).unwrap(),
            authored
        );
    }
    #[test]
    fn callback_dispatch_refusals_retain_the_original_typed_error() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        for refusal in [
            json!({"status":"refused","kind":"unknown_tool","error":"no such tool: echo","tool":"echo"}),
            json!({"status":"refused","kind":"invalid_input","error":"invalid input for tool echo: semantic rejection","tool":"echo","detail":"semantic rejection"}),
        ] {
            let service = service(
                &runtime,
                vec![vec![callback(), final_turn()]],
                Limits::default(),
            );
            let caller = PrincipalId::new(1, 2);
            let first = service.start(caller, request(false)).unwrap();
            let call = first["call_id"].as_str().unwrap();
            let finished = service
                .resume(
                    caller,
                    first["invocation"].as_str().unwrap(),
                    call,
                    refusal.clone(),
                )
                .unwrap();
            assert_eq!(finished["receipt"]["outcome"]["kind"], "text");
            let operation: OperationId = serde_json::from_str(call).unwrap();
            let recorded = service
                .store
                .replay_tool_output_operation(&operation)
                .unwrap()
                .unwrap();
            let harness::store::TerminalOutcome::Failure(failure) = recorded.terminal else {
                panic!("typed dispatch refusal expected")
            };
            let mut metadata = refusal.clone();
            metadata.as_object_mut().unwrap().remove("status");
            metadata.as_object_mut().unwrap().remove("error");
            assert_eq!(failure.metadata(), Some(&metadata));
            assert_eq!(
                failure.message(),
                format!("tool failed: {}", refusal["error"].as_str().unwrap())
            );
            let output: Value =
                serde_json::from_str(recorded.item.0["output"].as_str().unwrap()).unwrap();
            assert_eq!(output["failure"], metadata);
            assert_eq!(output["error"], failure.message());
        }
    }
    #[test]
    fn invalid_policy_choices_and_negative_limits_do_not_admit_work() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let service = service(&runtime, vec![], Limits::default());
        let caller = PrincipalId::new(1, 2);
        let mut invalid = request(false);
        invalid["model"] = json!("foreign-model");
        assert!(service.start(caller, invalid).is_err());
        let mut invalid = request(false);
        invalid["effort"] = json!("high");
        assert!(service.start(caller, invalid).is_err());
        let mut invalid = request(false);
        invalid["limits"]["requests"] = json!(-1);
        assert!(service.start(caller, invalid).is_err());
        assert!(service.budget.get().is_none());
        assert!(service.registry.lock().unwrap().is_empty());
        let mut invalid = request(false);
        for kind in ["raw", "update", "finish"] {
            invalid["tools"][0]["kind"] = json!(kind);
            assert!(service.start(caller, invalid.clone()).is_err());
        }
        for (field, value) in [
            ("implementation", json!("haskell_cell")),
            ("schedule", json!("before_next_inference")),
            ("effectKeys", json!(["ContextReadWrite"])),
        ] {
            let mut invalid = request(false);
            invalid["tools"][0][field] = value;
            assert!(matches!(
                service.start(caller, invalid),
                Err(ModelBoundaryError::ModelRejected(_))
            ));
        }
        let mut invalid = request(false);
        invalid["tools"][0]["inputSchema"] = json!({"type":"string"});
        assert!(service.start(caller, invalid).is_err());
        assert!(service.budget.get().is_none());
    }
    #[test]
    fn nested_calls_share_budget_and_reject_foreign_and_duplicate_callbacks() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let service = service(
            &runtime,
            vec![vec![callback(), final_turn()], vec![final_turn()]],
            Limits {
                requests: 2,
                ..Limits::default()
            },
        );
        let caller = PrincipalId::new(1, 2);
        assert!(service
            .start(PrincipalId::new(1, 3), request(false))
            .is_err());
        let first = service.start(caller, request(false)).unwrap();
        let token = first["invocation"].as_str().unwrap();
        assert!(service
            .resume(caller, token, "wrong", callback_reply(json!("result")))
            .is_err());
        assert!(service
            .resume(
                PrincipalId::SYSTEM,
                token,
                "call-1",
                callback_reply(json!("result"))
            )
            .is_err());
        let nested = service.start(caller, request(false)).unwrap();
        assert_eq!(nested["receipt"]["outcome"]["kind"], "text");
        let finished = service
            .resume(
                caller,
                token,
                first["call_id"].as_str().unwrap(),
                callback_reply(json!("nested result")),
            )
            .unwrap();
        assert_eq!(finished["receipt"]["parent_cell"], "retained-cell");
        assert_eq!(finished["receipt"]["outcome"]["kind"], "exhausted");
        assert!(finished["receipt"].get("transcript").is_none());
        assert!(service
            .resume(
                caller,
                token,
                first["call_id"].as_str().unwrap(),
                callback_reply(json!("duplicate"))
            )
            .is_err());
    }
    #[test]
    fn reused_provider_call_ids_cannot_resume_a_later_operation() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let service = service(
            &runtime,
            vec![vec![callback(), callback(), final_turn()]],
            Limits::default(),
        );
        let caller = PrincipalId::new(1, 2);
        let first = service.start(caller, request(true)).unwrap();
        let token = first["invocation"].as_str().unwrap();
        let first_id = first["call_id"].as_str().unwrap();
        let first_hook = service
            .resume(caller, token, first_id, callback_reply(json!("first")))
            .unwrap();
        let second = service
            .annotate(
                caller,
                token,
                first_hook["operation"].as_str().unwrap(),
                json!({"kind":"none"}),
            )
            .unwrap();
        let second_id = second["call_id"].as_str().unwrap();
        assert_ne!(first_id, second_id);
        let first_operation: OperationId = serde_json::from_str(first_id).unwrap();
        let second_operation: OperationId = serde_json::from_str(second_id).unwrap();
        assert_eq!(first_operation.call, second_operation.call);
        assert_ne!(first_operation.request, second_operation.request);
        assert!(service
            .resume(
                caller,
                token,
                first_id,
                callback_reply(json!("late duplicate"))
            )
            .is_err());
        assert!(service
            .resume(
                caller,
                token,
                "call-1",
                callback_reply(json!("unqualified"))
            )
            .is_err());
        let second_hook = service
            .resume(caller, token, second_id, callback_reply(json!("second")))
            .unwrap();
        assert_eq!(second_hook["operation"], second["call_id"]);
        let finished = service
            .annotate(
                caller,
                token,
                second_hook["operation"].as_str().unwrap(),
                json!({"kind":"none"}),
            )
            .unwrap();
        assert_eq!(finished["receipt"]["outcome"]["kind"], "text");
    }
    #[test]
    fn after_tool_control_preserves_the_actual_semantic_callback_value() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let service = service(
            &runtime,
            vec![vec![callback(), final_turn()]],
            Limits::default(),
        );
        let caller = PrincipalId::new(1, 2);
        let first = service.start(caller, request(true)).unwrap();
        let token = first["invocation"].as_str().unwrap();
        let semantic = json!({"answer":42,"error":"ordinary authored output","status":"refused"});
        let hook = service
            .resume(
                caller,
                token,
                first["call_id"].as_str().unwrap(),
                callback_reply(semantic.clone()),
            )
            .unwrap();
        let control: ModelControlStep = serde_json::from_value(hook.clone()).unwrap();
        let ModelControlStep::ModelHook { value, output, .. } = control else {
            panic!("expected after-tool continuation")
        };
        assert_eq!(value, semantic);
        assert_eq!(serde_json::from_str::<Value>(&output).unwrap(), semantic);
        let retained = retained_model_result(&service.store, hook["handle"].as_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(retained.0["output"].as_str().unwrap()).unwrap(),
            semantic
        );
        let mut missing = hook.clone();
        missing.as_object_mut().unwrap().remove("value");
        assert!(serde_json::from_value::<ModelControlStep>(missing).is_err());
        let final_step = service
            .annotate(
                caller,
                token,
                hook["operation"].as_str().unwrap(),
                serde_json::to_value(ModelAnnotationEnvelope::ModelNoAnnotation).unwrap(),
            )
            .unwrap();
        assert_eq!(final_step["receipt"]["outcome"]["kind"], "text");
    }
    #[test]
    fn pruning_requires_the_exact_retained_handle() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let service = service(
            &runtime,
            vec![vec![callback(), final_turn()]],
            Limits::default(),
        );
        let caller = PrincipalId::new(1, 2);
        let first = service.start(caller, request(true)).unwrap();
        let token = first["invocation"].as_str().unwrap();
        let hook = service
            .resume(
                caller,
                token,
                first["call_id"].as_str().unwrap(),
                callback_reply(json!("original")),
            )
            .unwrap();
        assert_eq!(hook["kind"], "hook");
        assert_eq!(hook["value"], json!("original"));
        let operation = hook["operation"].as_str().unwrap();
        assert!(service
            .annotate(caller, token, "wrong", json!({"kind":"none"}))
            .is_err());
        assert!(service
            .annotate(
                caller,
                token,
                operation,
                json!({"kind":"pruned","handle":"foreign","text":"short"})
            )
            .is_err());
        let final_step = service
            .annotate(
                caller,
                token,
                operation,
                json!({"kind":"pruned","handle":hook["handle"],"text":"short"}),
            )
            .unwrap();
        assert_eq!(final_step["receipt"]["outcome"]["kind"], "text");
        let retained = retained_model_result(&service.store, hook["handle"].as_str().unwrap())
            .unwrap()
            .unwrap();
        assert!(retained.0["output"].as_str().unwrap().contains("original"));
        let events = service.store.events(None).unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == "model_tool_annotation"));
    }
    #[test]
    fn close_allows_an_admitted_callback_to_finish() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let service = service(
            &runtime,
            vec![vec![callback(), final_turn()]],
            Limits::default(),
        );
        let caller = PrincipalId::new(1, 2);
        let first = service.start(caller, request(false)).unwrap();
        let token = first["invocation"].as_str().unwrap();
        service.cancel();
        assert!(service.start(caller, request(false)).is_err());
        let finished = service
            .resume(
                caller,
                token,
                first["call_id"].as_str().unwrap(),
                callback_reply(json!("completed after close")),
            )
            .unwrap();
        assert_eq!(finished["receipt"]["outcome"]["kind"], "cancelled");
    }
    #[test]
    fn abstention_reason_is_retained() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let service = service(
            &runtime,
            vec![vec![callback(), final_turn()]],
            Limits::default(),
        );
        let caller = PrincipalId::new(1, 2);
        let first = service.start(caller, request(true)).unwrap();
        let token = first["invocation"].as_str().unwrap();
        let hook = service
            .resume(
                caller,
                token,
                first["call_id"].as_str().unwrap(),
                callback_reply(json!("result")),
            )
            .unwrap();
        service
            .annotate(
                caller,
                token,
                hook["operation"].as_str().unwrap(),
                json!({"kind":"abstained","reason":"insufficient evidence"}),
            )
            .unwrap();
        let event = service
            .store
            .events(None)
            .unwrap()
            .into_iter()
            .find(|event| event.kind == "model_tool_annotation")
            .unwrap();
        let payload: Value = serde_json::from_str(&event.payload).unwrap();
        assert_eq!(payload["annotation"]["value"], "insufficient evidence");
    }
    #[test]
    fn close_settles_an_abandoned_hook_after_annotation_rejection() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let service = service(
            &runtime,
            vec![vec![callback(), final_turn()]],
            Limits::default(),
        );
        let caller = PrincipalId::new(1, 2);
        let first = service.start(caller, request(true)).unwrap();
        let token = first["invocation"].as_str().unwrap();
        let hook = service
            .resume(
                caller,
                token,
                first["call_id"].as_str().unwrap(),
                callback_reply(json!("original")),
            )
            .unwrap();
        assert!(service
            .annotate(
                caller,
                token,
                hook["operation"].as_str().unwrap(),
                json!({"kind":"pruned","handle":"foreign","text":"short"})
            )
            .is_err());
        service.close(caller, token).unwrap();
        let entry = service.entry(caller, token).unwrap();
        let mut entry = entry.lock().unwrap();
        let finished = runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(2), entry.invocation.next()).await
        });
        let Step::Finished(receipt) = finished.unwrap() else {
            panic!("finished receipt expected")
        };
        assert!(matches!(
            receipt.outcome,
            harness::invocation::Outcome::Cancelled
        ));
    }
    #[test]
    fn foreign_cell_tokens_are_refused_even_for_the_same_actor_principal() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let owner = service(
            &runtime,
            vec![vec![callback(), final_turn()]],
            Limits::default(),
        );
        let foreign = service(&runtime, vec![], Limits::default());
        let caller = PrincipalId::new(1, 2);
        let first = owner.start(caller, request(false)).unwrap();
        let token = first["invocation"].as_str().unwrap();
        let operation = first["call_id"].as_str().unwrap();
        assert!(matches!(
            foreign.resume(caller, token, operation, callback_reply(json!("foreign"))),
            Err(ModelBoundaryError::ModelRejected(_))
        ));
        assert!(matches!(
            foreign.close(caller, token),
            Err(ModelBoundaryError::ModelRejected(_))
        ));
        assert!(foreign.registry.lock().unwrap().is_empty());
        assert!(foreign.budget.get().is_none());
        let finished = owner
            .resume(caller, token, operation, callback_reply(json!("original")))
            .unwrap();
        assert_eq!(finished["receipt"]["outcome"]["kind"], "text");
    }
}
