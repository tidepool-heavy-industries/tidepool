//! [`CodexAgentBackend`] — the real backend behind the
//! [`AgentBackend`](crate::backend::AgentBackend) seam.
//!
//! One `codex app-server` process, connected lazily on the first call and
//! reused for the backend's lifetime. Everything crossing back out is
//! [`crate::seam`] vocabulary: `codex-codes` types appear as locals and in
//! `pub(crate)`/private helper signatures, never in this type's public API.
//!
//! # The three shapes this module is responsible for holding
//!
//! 1. **`cwd` at turn start, never at thread start.** PRD 18 acceptance
//!    criterion 11: a `cwd` on `thread/start` triggers Codex's project-trust
//!    write into the operator's `config.toml`. [`ThreadStartWithDynamicTools`]
//!    has no `cwd` field at all, so this is structural rather than a
//!    convention — [`thread_start_omits_cwd`](tests::thread_start_omits_cwd)
//!    pins it against a future field addition.
//! 2. **The model is RESOLVED, never hardcoded.**
//!    Each [`ModelPolicy`] names an ALLOWLIST ([`preference_for`]); resolution
//!    queries `model/list` once and takes the first listed slug actually
//!    offered, failing otherwise. A model outside the list can never be
//!    selected however the server's catalogue changes — that is the mechanism,
//!    not a denylist a new slug could slip past.
//!    [`CycleOutcome::resolved_model`] carries the exact slug that ran.
//! 3. **No `item/tool/call` is ever left stranded.** A call is answered —
//!    with the parent's value, or with a refusal — because an unanswered one
//!    parks the child's turn until the timeout kills it. That holds for a
//!    declared tool, an undeclared one, and a round-cap refusal alike.

use std::time::Duration;

use codex_codes::{
    InitializeCapabilities, ModelListParams, ModelListResponse, SandboxPolicy, ThreadStartResponse,
    Turn, TurnStartParams, TurnStatus, UserInput,
};

use crate::backend::codex::dynamic_tools::{
    DynamicToolFunctionSpec, DynamicToolSpec, ThreadStartWithDynamicTools,
};
use crate::backend::codex::process::{last_agent_message_text, Session, SessionError, TurnStop};
use crate::backend::AgentBackend;
use crate::seam::{
    AgentActivity, AgentBackendError, BackendThreadId, CycleOutcome, CycleResultPayload, CycleSpec,
    DynamicToolDeclaration, ModelPolicy, ReasoningEffort, ThreadSpec, TokenUsage, ToolCall,
    ToolCallId, ToolOutcome, ToolReply, TurnEvent, TurnId,
};

/// The cheap-plumbing tier, in preference order.
///
/// An ALLOWLIST: [`choose_model`] returns the first entry that
/// `model/list` actually offers and fails otherwise, so no model outside this
/// list is reachable — including `gpt-5.6-terra`, which overnight policy bans
/// and which the replay fixture happens to have been recorded on. That
/// fixture is protocol truth, never a model choice.
pub const CHEAP_PLUMBING_PREFERENCE: [&str; 2] = ["gpt-5.4-mini", "gpt-5.6-luna"];

/// The cheapest gpt-5.6 tier, pinned to exactly one slug.
///
/// A one-entry allowlist is still an allowlist, and that is the point: a
/// specific budget grant names `gpt-5.6-luna` exactly, so resolving to
/// anything else — including the CHEAPER `gpt-5.4-mini` — would spend a
/// budget on a model nobody authorized. Cheaper is not the same as granted.
pub const CHEAPEST_GPT56_PREFERENCE: [&str; 1] = ["gpt-5.6-luna"];

/// The allowlist a policy resolves against, in preference order.
pub(crate) fn preference_for(policy: ModelPolicy) -> &'static [&'static str] {
    match policy {
        ModelPolicy::CheapPlumbing => &CHEAP_PLUMBING_PREFERENCE,
        ModelPolicy::CheapestGpt56 => &CHEAPEST_GPT56_PREFERENCE,
    }
}

/// Project the seam's effort onto the protocol's own vocabulary.
///
/// `codex_codes::ReasoningEffort` is a transparent newtype over `String` (the
/// protocol calls it "a non-empty reasoning effort value advertised by the
/// model"), so this is where the seam's closed enum meets an open wire
/// vocabulary. Keeping the seam closed is deliberate: the caller chooses among
/// efforts Tidepool has decided it supports, not among whatever strings a
/// server might advertise.
pub(crate) fn effort_to_wire(effort: ReasoningEffort) -> codex_codes::ReasoningEffort {
    codex_codes::ReasoningEffort(
        match effort {
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
        }
        .to_string(),
    )
}

/// Default ceiling on one cycle. A cycle is one model turn in a workspace, so
/// this bounds a hung server or a runaway turn — it is not a latency budget.
pub const DEFAULT_TURN_TIMEOUT: Duration = Duration::from_secs(300);

/// A live `codex app-server` driving one thread at a time through the seam.
///
/// Owns its own tokio runtime (the `LlmHandler` precedent): the seam is sync
/// because effect handlers are sync, and [`Session`] is async, so exactly one
/// place blocks — here.
pub struct CodexAgentBackend {
    runtime: tokio::runtime::Runtime,
    /// Connected on first use. `None` means "not connected yet", never
    /// "connection lost" — a lost connection surfaces as a
    /// [`AgentBackendError::BackendUnavailable`] from the call that noticed.
    session: Option<Session>,
    /// Fetched once per backend and reused: `model/list` is a metadata
    /// request, but re-asking per cycle would let one agent's turns silently
    /// run on two different models.
    catalogue: Option<Vec<String>>,
    /// The exact slug the last resolution picked.
    resolved_model: Option<String>,
    turn_timeout: Duration,
}

impl CodexAgentBackend {
    /// Build a backend. Does NOT spawn the app-server — the process starts on
    /// the first [`start_thread`](AgentBackend::start_thread), so
    /// constructing one is free and side-effect-free.
    pub fn new() -> Result<Self, AgentBackendError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| AgentBackendError::BackendUnavailable {
                detail: format!("failed to build the backend's tokio runtime: {e}"),
            })?;
        Ok(Self {
            runtime,
            session: None,
            catalogue: None,
            resolved_model: None,
            turn_timeout: DEFAULT_TURN_TIMEOUT,
        })
    }

    /// Override the per-cycle timeout (default [`DEFAULT_TURN_TIMEOUT`]).
    #[must_use]
    pub fn with_turn_timeout(mut self, timeout: Duration) -> Self {
        self.turn_timeout = timeout;
        self
    }

    pub fn turn_timeout(&self) -> Duration {
        self.turn_timeout
    }

    /// The exact model this backend resolved, once it has resolved one.
    /// `None` before the first cycle.
    pub fn resolved_model(&self) -> Option<&str> {
        self.resolved_model.as_deref()
    }

    /// Every JSONL frame exchanged so far, in wire order.
    ///
    /// Exists so a live run can commit its own transcript and have
    /// [`replay`](crate::backend::codex::replay) drive the production pump
    /// over it forever after.
    pub fn frames(&self) -> &[crate::backend::codex::process::RecordedFrame] {
        self.session.as_ref().map(Session::frames).unwrap_or(&[])
    }

    /// Kill the app-server and confirm it was reaped. Best effort is not good
    /// enough here — [`Session::shutdown`] checks the pid rather than trusting
    /// that the signal was sent.
    pub fn shutdown(mut self) -> Result<(), AgentBackendError> {
        let Some(session) = self.session.take() else {
            return Ok(());
        };
        self.runtime
            .block_on(session.shutdown())
            .map_err(map_session_error)
    }

    /// The connected session, spawning + handshaking on first use.
    ///
    /// Returns the runtime alongside it because both are borrowed from `self`:
    /// `self.runtime.block_on(self.session…)` would be a double borrow.
    fn connected(&mut self) -> Result<(&tokio::runtime::Runtime, &mut Session), AgentBackendError> {
        if self.session.is_none() {
            // `experimentalApi` unlocks `thread/start.dynamicTools`
            // (PROTOCOL-NOTES.md §2). Requested unconditionally: a thread with
            // no dynamic tools does not need it, but negotiating a different
            // handshake per spec would make the wire shape diverge for no
            // gain.
            let capabilities = InitializeCapabilities {
                experimental_api: Some(true),
                ..Default::default()
            };
            let session = self
                .runtime
                .block_on(Session::connect(capabilities))
                .map_err(map_session_error)?;
            self.session = Some(session);
        }
        let Self {
            runtime, session, ..
        } = self;
        Ok((
            runtime,
            session.as_mut().expect("session connected just above"),
        ))
    }

    /// Resolve `policy` to an exact model slug, querying `model/list` at most
    /// once per backend.
    ///
    /// The catalogue is cached, not the CHOICE: two policies resolve against
    /// the same fetched list but may legitimately pick different slugs.
    pub(crate) fn resolve_model(
        &mut self,
        policy: ModelPolicy,
    ) -> Result<String, AgentBackendError> {
        if self.catalogue.is_none() {
            let (runtime, session) = self.connected()?;
            let response: ModelListResponse = runtime
                .block_on(session.request(
                    codex_codes::methods::MODEL_LIST,
                    &ModelListParams::default(),
                ))
                .map_err(map_session_error)?;
            self.catalogue = Some(model_slugs(&response));
        }
        let available = self.catalogue.as_deref().expect("fetched just above");
        let model = choose_model(policy, available)?;
        self.resolved_model = Some(model.clone());
        Ok(model)
    }
}

impl AgentBackend for CodexAgentBackend {
    fn start_thread(&mut self, spec: &ThreadSpec) -> Result<BackendThreadId, AgentBackendError> {
        let params = thread_start_params(spec);
        let (runtime, session) = self.connected()?;
        let response: ThreadStartResponse = runtime
            .block_on(session.request(codex_codes::methods::THREAD_START, &params))
            .map_err(map_session_error)?;
        Ok(BackendThreadId(response.thread.id))
    }

    fn start_turn(
        &mut self,
        thread: &BackendThreadId,
        spec: &CycleSpec,
    ) -> Result<TurnEvent, AgentBackendError> {
        let resolved_model = self.resolve_model(spec.model)?;
        let params = turn_start_params(thread, spec, &resolved_model);
        let timeout = self.turn_timeout;
        let (runtime, session) = self.connected()?;
        let stop = runtime
            .block_on(session.start_turn(&params, timeout))
            .map_err(map_session_error)?;
        project_stop(stop, session, &resolved_model)
    }

    /// The recorded frames, rendered one per line — the exact format
    /// [`replay::TranscriptTransport`](crate::backend::codex::replay::TranscriptTransport)
    /// reads back.
    fn transcript_jsonl(&self) -> Vec<String> {
        self.frames()
            .iter()
            .map(|f| serde_json::to_string(f).expect("a RecordedFrame always serializes"))
            .collect()
    }

    fn resume(&mut self, reply: ToolReply) -> Result<TurnEvent, AgentBackendError> {
        // The model was resolved when the turn started; re-resolving here could
        // silently move an in-flight turn onto a different model.
        let resolved_model =
            self.resolved_model
                .clone()
                .ok_or_else(|| AgentBackendError::ProtocolRejected {
                    detail: "resume with no turn in flight: no model has been resolved".to_string(),
                })?;
        let response = tool_outcome_to_response(&reply.outcome);
        let timeout = self.turn_timeout;
        let (runtime, session) = self.connected()?;
        let stop = runtime
            .block_on(session.reply_and_pump(&reply.call.0, &response, timeout))
            .map_err(map_session_error)?;
        project_stop(stop, session, &resolved_model)
    }
}

/// Project a pump stop into the seam's [`TurnEvent`].
///
/// The `session` borrow is what supplies token usage: it rides
/// `thread/tokenUsage/updated` notifications rather than the terminal frame, so
/// it is session state by the time the turn completes, not something readable
/// off the `Turn`.
fn project_stop(
    stop: TurnStop,
    session: &Session,
    resolved_model: &str,
) -> Result<TurnEvent, AgentBackendError> {
    match stop {
        TurnStop::ToolCall(params) => Ok(TurnEvent::ToolCall(ToolCall {
            call: ToolCallId(params.call_id),
            thread: BackendThreadId(params.thread_id),
            turn: TurnId(params.turn_id),
            tool: params.tool,
            arguments: params.arguments,
        })),
        TurnStop::Completed(turn) => {
            if let Some(error) = turn_failure(&turn) {
                return Err(error);
            }
            Ok(TurnEvent::Completed(CycleOutcome {
                turn: TurnId(turn.id.clone()),
                payload: project_payload(&turn),
                activity: project_activity(&turn),
                resolved_model: resolved_model.to_string(),
                usage: session.token_usage().map(project_usage),
            }))
        }
    }
}

/// Project the backend's cumulative thread totals into the seam's usage.
///
/// `last` rather than `total`: the seam reports what THIS turn cost, and a
/// thread's running total would double-count on any thread that ran more than
/// one turn.
pub(crate) fn project_usage(usage: &codex_codes::ThreadTokenUsage) -> TokenUsage {
    let last = &usage.last;
    TokenUsage {
        input_tokens: last.input_tokens,
        cached_input_tokens: last.cached_input_tokens,
        output_tokens: last.output_tokens,
        reasoning_output_tokens: last.reasoning_output_tokens,
        total_tokens: last.total_tokens,
    }
}

/// Project a seam [`ToolOutcome`] onto the protocol's own reply shape.
///
/// `success: false` with content is the protocol's failure shape, not a
/// JSON-RPC error (PROTOCOL-NOTES.md §3) — so a refusal is an ordinary
/// conversational fact the child reads and reacts to, and a stranded call is
/// impossible by construction.
pub(crate) fn tool_outcome_to_response(
    outcome: &ToolOutcome,
) -> codex_codes::DynamicToolCallResponse {
    let (success, text) = match outcome {
        ToolOutcome::Answered(value) => (true, value.to_string()),
        ToolOutcome::Refused(detail) => (false, detail.clone()),
    };
    codex_codes::DynamicToolCallResponse {
        success,
        content_items: vec![codex_codes::DynamicToolCallOutputContentItem::InputText { text }],
    }
}

/// `thread/start` params for a seam [`ThreadSpec`].
///
/// Deliberately built from [`ThreadStartWithDynamicTools`], which carries no
/// `cwd` — see this module's docs, point 1.
fn thread_start_params(spec: &ThreadSpec) -> ThreadStartWithDynamicTools {
    ThreadStartWithDynamicTools {
        ephemeral: spec.ephemeral,
        dynamic_tools: spec.dynamic_tools.iter().map(declaration_to_spec).collect(),
    }
}

/// Project one seam tool declaration onto the codex wire shape.
fn declaration_to_spec(declaration: &DynamicToolDeclaration) -> DynamicToolSpec {
    DynamicToolSpec::Function(DynamicToolFunctionSpec {
        name: declaration.name.clone(),
        description: declaration.description.clone(),
        input_schema: declaration.input_schema.clone(),
        defer_loading: false,
    })
}

/// `turn/start` params for one cycle: `cwd` HERE, sandboxed to write only
/// inside that same `cwd`.
fn turn_start_params(
    thread: &BackendThreadId,
    spec: &CycleSpec,
    resolved_model: &str,
) -> TurnStartParams {
    TurnStartParams {
        thread_id: thread.0.clone(),
        cwd: Some(spec.cwd.clone()),
        model: Some(resolved_model.to_string()),
        sandbox_policy: Some(SandboxPolicy::WorkspaceWrite {
            exclude_slash_tmp: Some(false),
            exclude_tmpdir_env_var: Some(false),
            network_access: Some(false),
            // The worker's bound worktree is the ONLY writable root — the
            // sandbox is where that one-worktree-per-agent coupling is
            // enforced against the process, not merely asserted about it.
            writable_roots: Some(vec![codex_codes::AbsolutePathBuf(spec.cwd.clone())]),
        }),
        // Never ASK: containment is the SANDBOX's job (above), not a
        // conversational consent layer — there is no human on this seam to
        // answer. Left unset, the default policy sent
        // `item/commandExecution/requestApproval` for an in-worktree
        // `git commit`, which the session pump answered method-not-found and
        // Codex read as a rejection — the curator's first live run filed
        // every memory and then could not commit (dogfood, 2026-08-14).
        approval_policy: Some(codex_codes::AskForApproval::Never),
        effort: Some(effort_to_wire(spec.effort)),
        input: vec![UserInput::Text {
            text: spec.task.clone(),
            text_elements: None,
        }],
        output_schema: spec.output_schema.clone(),
        ..Default::default()
    }
}

/// The model slugs `model/list` offered, in the order it offered them.
///
/// `model` is the slug `turn/start` expects; `id` is its fallback, since both
/// carry the same value in the observed catalogue and a future response that
/// populated only one of them should still resolve.
fn model_slugs(response: &ModelListResponse) -> Vec<String> {
    response
        .data
        .iter()
        .map(|model| {
            if model.model.is_empty() {
                model.id.clone()
            } else {
                model.model.clone()
            }
        })
        .filter(|slug| !slug.is_empty())
        .collect()
}

/// Pick the model `policy` names from what the backend actually offers.
///
/// Pure and allowlist-shaped: the first [`CHEAP_PLUMBING_PREFERENCE`] entry
/// present wins, and nothing else is reachable. The failure names what WAS
/// available — "no cheap model" with no catalogue is not a diagnosable
/// receipt.
pub(crate) fn choose_model(
    policy: ModelPolicy,
    available: &[String],
) -> Result<String, AgentBackendError> {
    let preference = preference_for(policy);
    for preferred in preference {
        if available.iter().any(|slug| slug == preferred) {
            return Ok((*preferred).to_string());
        }
    }
    Err(AgentBackendError::ProtocolRejected {
        detail: format!(
            "no model available for {policy:?}: wanted one of {preference:?} (in that order); \
             model/list offered {available:?}"
        ),
    })
}

/// The seam error for a turn that did not complete, or `None` when it did.
///
/// A turn that reaches `turn/completed` with a non-`completed` status is a
/// TURN-level failure: the backend accepted everything and the run itself went
/// wrong, which is [`AgentBackendError::RunFailed`], not a protocol problem.
pub(crate) fn turn_failure(turn: &Turn) -> Option<AgentBackendError> {
    match turn.status {
        TurnStatus::Completed => None,
        TurnStatus::Interrupted => Some(AgentBackendError::RunFailed {
            detail: format!(
                "turn {} was interrupted: {}",
                turn.id,
                turn_error_text(turn)
            ),
        }),
        TurnStatus::Failed => Some(AgentBackendError::RunFailed {
            detail: format!("turn {} failed: {}", turn.id, turn_error_text(turn)),
        }),
        // `turn/completed` carrying `inProgress` is a protocol contradiction,
        // but the run is still the thing that did not finish — reporting it as
        // a rejected request would point a reader at the wrong side.
        TurnStatus::InProgress => Some(AgentBackendError::RunFailed {
            detail: format!(
                "turn {} reported status inProgress on turn/completed: {}",
                turn.id,
                turn_error_text(turn)
            ),
        }),
    }
}

/// The backend's own words about why a turn ended badly, verbatim.
fn turn_error_text(turn: &Turn) -> String {
    match &turn.error {
        Some(error) => match &error.additional_details {
            Some(details) => format!("{} ({details})", error.message),
            None => error.message.clone(),
        },
        None => "no error detail reported".to_string(),
    }
}

/// Project the terminal agent message into the seam payload.
///
/// `outputSchema` constrains the final agent message TEXT — there is no
/// separate structured-output field (PROTOCOL-NOTES.md §4) — so "parsed as
/// JSON" is the whole of the structured/unstructured distinction. Whether the
/// JSON matches the caller's type is decided on the Haskell side; `Structured`
/// is not yet a typed success.
pub(crate) fn project_payload(turn: &Turn) -> CycleResultPayload {
    match last_agent_message_text(turn) {
        None => CycleResultPayload::Absent,
        Some(text) => match serde_json::from_str::<serde_json::Value>(text) {
            Ok(value) => CycleResultPayload::Structured(value),
            Err(_) => CycleResultPayload::Unstructured(text.to_string()),
        },
    }
}

/// Project the completed turn's items into receipt-bearing activity.
///
/// Shallow by design — see this module's docs. Only the two item kinds that
/// map onto an existing [`AgentActivity`] variant with no interpretation:
/// commands the backend ran, and paths it changed.
pub(crate) fn project_activity(turn: &Turn) -> Vec<AgentActivity> {
    let mut activity = Vec::new();
    for item in &turn.items {
        match item {
            codex_codes::ThreadItem::CommandExecution {
                command, exit_code, ..
            } => activity.push(AgentActivity::Command {
                command: command.clone(),
                exit_code: exit_code.and_then(|code| i32::try_from(code).ok()),
            }),
            codex_codes::ThreadItem::FileChange { changes, .. } => {
                for change in changes {
                    activity.push(AgentActivity::FileChanged {
                        path: change.path.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    activity
}

/// Project a transport/protocol failure into the seam's causes.
///
/// The split is mine-vs-theirs: a dead or unreachable process is
/// `BackendUnavailable` (every agent it hosted is lost); anything the server
/// understood well enough to refuse, or that this crate could not encode or
/// decode, is `ProtocolRejected` (a Tidepool bug or a version skew). Detail
/// strings are the source error's own `Display`, verbatim — a reworded error
/// is one more thing to keep in sync with the wire.
pub(crate) fn map_session_error(error: SessionError) -> AgentBackendError {
    let detail = error.to_string();
    match error {
        SessionError::Spawn(_)
        | SessionError::Timeout { .. }
        | SessionError::Closed { .. }
        | SessionError::OrphanedProcess { .. }
        | SessionError::Transport(_) => AgentBackendError::BackendUnavailable { detail },
        SessionError::Rpc { .. }
        | SessionError::Decode { .. }
        | SessionError::Encode { .. }
        | SessionError::MalformedLine(_)
        | SessionError::MissingField { .. }
        // A misrouted or unmatched tool reply is OUR bug, not the server's —
        // same class as a request this crate could not encode, and reported
        // the same way rather than being softened into a run failure.
        | SessionError::NoParkedCall { .. }
        | SessionError::WrongCall { .. } => AgentBackendError::ProtocolRejected { detail },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slugs(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn turn_from_json(value: serde_json::Value) -> Turn {
        serde_json::from_value(value).expect("Turn fixture must match the real wire shape")
    }

    /// The shape the operator's real `model/list` response takes
    /// (`fixtures/app-server-0.146.0/phase3-handshake.jsonl`), reduced to the
    /// fields `model_slugs` reads plus the one field `Model` requires.
    fn model_list_from_json(models: &[&str]) -> ModelListResponse {
        serde_json::from_value(serde_json::json!({
            "data": models
                .iter()
                .map(|slug| serde_json::json!({
                    "id": slug,
                    "model": slug,
                    "displayName": slug,
                    "defaultReasoningEffort": "medium"
                }))
                .collect::<Vec<_>>()
        }))
        .expect("ModelListResponse fixture must match the real wire shape")
    }

    // --- model resolution ---------------------------------------------------

    #[test]
    fn mini_wins_when_present() {
        let available = slugs(&["gpt-5.6-sol", "gpt-5.6-luna", "gpt-5.4-mini", "gpt-5.4"]);
        assert_eq!(
            choose_model(ModelPolicy::CheapPlumbing, &available).unwrap(),
            "gpt-5.4-mini"
        );
    }

    #[test]
    fn luna_is_the_fallback_when_mini_is_absent() {
        let available = slugs(&["gpt-5.6-sol", "gpt-5.6-luna", "gpt-5.5"]);
        assert_eq!(
            choose_model(ModelPolicy::CheapPlumbing, &available).unwrap(),
            "gpt-5.6-luna"
        );
    }

    /// The policy's hard edge: a catalogue offering only the banned model must
    /// FAIL, never fall through to it. The refusal is structural — the
    /// preference list is an allowlist — so this test also covers any future
    /// slug that is not explicitly sanctioned.
    #[test]
    fn a_catalogue_without_a_sanctioned_model_is_refused() {
        let available = slugs(&["gpt-5.6-terra"]);
        let error = choose_model(ModelPolicy::CheapPlumbing, &available).unwrap_err();
        assert!(
            matches!(error, AgentBackendError::ProtocolRejected { .. }),
            "expected ProtocolRejected, got {error:?}"
        );
        let detail = error.to_string();
        assert!(
            detail.contains("gpt-5.6-terra"),
            "the failure must name what WAS available: {detail}"
        );
        assert!(
            detail.contains("gpt-5.4-mini") && detail.contains("gpt-5.6-luna"),
            "the failure must name what was wanted: {detail}"
        );
    }

    #[test]
    fn an_empty_catalogue_is_refused() {
        let error = choose_model(ModelPolicy::CheapPlumbing, &[]).unwrap_err();
        assert!(matches!(error, AgentBackendError::ProtocolRejected { .. }));
    }

    #[test]
    fn slugs_come_from_the_model_field_in_wire_order() {
        let response = model_list_from_json(&["gpt-5.6-sol", "gpt-5.4-mini"]);
        assert_eq!(
            model_slugs(&response),
            slugs(&["gpt-5.6-sol", "gpt-5.4-mini"])
        );
    }

    #[test]
    fn slugs_fall_back_to_id_when_model_is_empty() {
        let response: ModelListResponse = serde_json::from_value(serde_json::json!({
            "data": [{"id": "gpt-5.4-mini", "defaultReasoningEffort": "medium"}]
        }))
        .unwrap();
        assert_eq!(model_slugs(&response), slugs(&["gpt-5.4-mini"]));
        assert_eq!(
            choose_model(ModelPolicy::CheapPlumbing, &model_slugs(&response)).unwrap(),
            "gpt-5.4-mini"
        );
    }

    // --- request shapes -----------------------------------------------------

    /// PRD 18 acceptance criterion 11, pinned at the serialization boundary:
    /// `thread/start` must carry no `cwd`, because that is the request that
    /// writes `projects.<path>` into the operator's `config.toml`.
    #[test]
    fn thread_start_omits_cwd() {
        let params = thread_start_params(&ThreadSpec {
            ephemeral: true,
            dynamic_tools: Vec::new(),
        });
        let value = serde_json::to_value(&params).unwrap();
        assert!(
            value.get("cwd").is_none(),
            "thread/start must not carry cwd: {value}"
        );
        assert_eq!(value["ephemeral"], serde_json::json!(true));
        assert_eq!(value["dynamicTools"], serde_json::json!([]));
    }

    #[test]
    fn thread_start_projects_seam_tool_declarations() {
        let params = thread_start_params(&ThreadSpec {
            ephemeral: false,
            dynamic_tools: vec![DynamicToolDeclaration {
                name: "ask_parent".to_string(),
                description: "Ask the parent.".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            }],
        });
        let value = serde_json::to_value(&params).unwrap();
        assert_eq!(value["ephemeral"], serde_json::json!(false));
        assert_eq!(
            value["dynamicTools"][0],
            serde_json::json!({
                "type": "function",
                "name": "ask_parent",
                "description": "Ask the parent.",
                "inputSchema": {"type": "object"}
            })
        );
    }

    /// The other half of criterion 11: `cwd` IS supplied at turn start, and
    /// the sandbox's only writable root is that same directory.
    #[test]
    fn turn_start_carries_cwd_and_sandboxes_to_it() {
        let spec = CycleSpec {
            cwd: "/tmp/worker-tree".to_string(),
            task: "write the word cobalt".to_string(),
            output_schema: Some(serde_json::json!({"type": "object"})),
            model: ModelPolicy::CheapPlumbing,
            effort: ReasoningEffort::Low,
        };
        let params = turn_start_params(
            &BackendThreadId("thread-1".to_string()),
            &spec,
            "gpt-5.4-mini",
        );
        let value = serde_json::to_value(&params).unwrap();
        assert_eq!(value["threadId"], serde_json::json!("thread-1"));
        assert_eq!(value["cwd"], serde_json::json!("/tmp/worker-tree"));
        assert_eq!(value["model"], serde_json::json!("gpt-5.4-mini"));
        assert_eq!(
            value["input"][0],
            serde_json::json!({"type": "text", "text": "write the word cobalt"})
        );
        assert_eq!(value["outputSchema"], serde_json::json!({"type": "object"}));
        assert_eq!(
            value["sandboxPolicy"]["writableRoots"],
            serde_json::json!(["/tmp/worker-tree"])
        );
        assert_eq!(
            value["sandboxPolicy"]["networkAccess"],
            serde_json::json!(false)
        );
    }

    #[test]
    fn an_undeclared_tool_call_is_answered_with_a_failure_never_stranded() {
        // The refusal text is built by the caller that knows WHY (the no-tools
        // combinator); this pins the projection onto the protocol's own failure
        // shape — `success: false` with content, never a JSON-RPC error, which
        // is what keeps a refused call from stranding the child's turn.
        let params: codex_codes::DynamicToolCallParams =
            serde_json::from_value(serde_json::json!({
                "threadId": "t1",
                "turnId": "turn_1",
                "callId": "call_1",
                "tool": "ask_parent",
                "arguments": {"question": "what is the passphrase?"}
            }))
            .expect("DynamicToolCallParams fixture must match the real wire shape");
        let response = tool_outcome_to_response(&ToolOutcome::Refused(format!(
            "no such tool: {} — this agent was created with no dynamic tools",
            params.tool
        )));
        assert!(!response.success);
        let rendered = serde_json::to_value(&response).unwrap();
        assert!(
            rendered["contentItems"][0]["text"]
                .as_str()
                .unwrap()
                .contains("ask_parent"),
            "the refusal must name the tool it refused: {rendered}"
        );
    }

    // --- payload projection -------------------------------------------------

    /// Four terminal-message shapes: a JSON agent message projects
    /// Structured, prose projects Unstructured, the LAST agent message wins
    /// when several precede it (an earlier JSON one must not win), and no
    /// agent message at all projects Absent.
    #[test]
    fn project_payload_matches_terminal_message_shape() {
        let cases = vec![
            (
                "json_terminal_message",
                serde_json::json!({
                    "id": "turn_1",
                    "status": "completed",
                    "items": [{"type": "agentMessage", "id": "i1", "text": "{\"result\":\"cobalt\"}"}]
                }),
                CycleResultPayload::Structured(serde_json::json!({"result": "cobalt"})),
            ),
            (
                "prose_terminal_message",
                serde_json::json!({
                    "id": "turn_1",
                    "status": "completed",
                    "items": [{"type": "agentMessage", "id": "i1", "text": "I wrote the word cobalt."}]
                }),
                CycleResultPayload::Unstructured("I wrote the word cobalt.".to_string()),
            ),
            (
                "last_agent_message_wins",
                serde_json::json!({
                    "id": "turn_1",
                    "status": "completed",
                    "items": [
                        {"type": "agentMessage", "id": "i1", "text": "{\"result\":\"first\"}"},
                        {"type": "reasoning", "id": "i2"},
                        {"type": "agentMessage", "id": "i3", "text": "{\"result\":\"final\"}"}
                    ]
                }),
                CycleResultPayload::Structured(serde_json::json!({"result": "final"})),
            ),
            (
                "no_agent_message",
                serde_json::json!({
                    "id": "turn_1",
                    "status": "completed",
                    "items": [{"type": "reasoning", "id": "i1"}]
                }),
                CycleResultPayload::Absent,
            ),
        ];

        for (label, raw, expected) in cases {
            let turn = turn_from_json(raw);
            assert_eq!(project_payload(&turn), expected, "case: {label}");
        }
    }

    // --- activity projection ------------------------------------------------

    #[test]
    fn commands_and_file_changes_project_into_activity() {
        let turn = turn_from_json(serde_json::json!({
            "id": "turn_1",
            "status": "completed",
            "items": [
                {
                    "type": "commandExecution",
                    "id": "i1",
                    "command": "git status",
                    "commandActions": [],
                    "cwd": "/tmp/worker-tree",
                    "exitCode": 0,
                    "status": "completed"
                },
                {
                    "type": "fileChange",
                    "id": "i2",
                    "status": "completed",
                    "changes": [
                        {"path": "/tmp/worker-tree/a.txt", "kind": {"type": "add"}, "diff": ""}
                    ]
                },
                {"type": "agentMessage", "id": "i3", "text": "done"}
            ]
        }));
        assert_eq!(
            project_activity(&turn),
            vec![
                AgentActivity::Command {
                    command: "git status".to_string(),
                    exit_code: Some(0),
                },
                AgentActivity::FileChanged {
                    path: "/tmp/worker-tree/a.txt".to_string(),
                },
            ]
        );
    }

    /// Shallow projection: a turn carrying only an `agentMessage` item
    /// produces an empty activity vector — the expected shape, not a bug.
    #[test]
    fn a_message_only_turn_projects_no_activity() {
        let turn = turn_from_json(serde_json::json!({
            "id": "turn_1",
            "status": "completed",
            "items": [{"type": "agentMessage", "id": "i1", "text": "{}"}]
        }));
        assert!(project_activity(&turn).is_empty());
    }

    // --- turn-level failure -------------------------------------------------

    #[test]
    fn a_completed_turn_is_not_a_failure() {
        let turn = turn_from_json(serde_json::json!({
            "id": "turn_1", "status": "completed", "items": []
        }));
        assert!(turn_failure(&turn).is_none());
    }

    #[test]
    fn a_failed_turn_maps_to_run_failed_carrying_the_backends_own_words() {
        let turn = turn_from_json(serde_json::json!({
            "id": "turn_1",
            "status": "failed",
            "items": [],
            "error": {"message": "rate limit exceeded", "additionalDetails": "retry after 60s"}
        }));
        let error = turn_failure(&turn).expect("a failed turn must map to an error");
        assert!(matches!(error, AgentBackendError::RunFailed { .. }));
        let detail = error.to_string();
        assert!(detail.contains("rate limit exceeded"), "{detail}");
        assert!(detail.contains("retry after 60s"), "{detail}");
        assert!(detail.contains("turn_1"), "{detail}");
    }

    #[test]
    fn an_interrupted_turn_maps_to_run_failed() {
        let turn = turn_from_json(serde_json::json!({
            "id": "turn_1", "status": "interrupted", "items": []
        }));
        let error = turn_failure(&turn).expect("an interrupted turn must map to an error");
        assert!(matches!(error, AgentBackendError::RunFailed { .. }));
        assert!(error.to_string().contains("interrupted"));
    }

    #[test]
    fn a_failed_turn_without_error_detail_still_says_so() {
        let turn = turn_from_json(serde_json::json!({
            "id": "turn_1", "status": "failed", "items": []
        }));
        let error = turn_failure(&turn).expect("a failed turn must map to an error");
        assert!(error.to_string().contains("no error detail reported"));
    }

    // --- error mapping ------------------------------------------------------

    #[test]
    fn transport_level_failures_are_backend_unavailable() {
        for error in [
            SessionError::Timeout {
                method: "turn/start".to_string(),
                timeout: Duration::from_secs(1),
            },
            SessionError::Closed {
                method: "thread/start".to_string(),
            },
            SessionError::OrphanedProcess {
                timeout: Duration::from_secs(10),
            },
        ] {
            let expected_detail = error.to_string();
            let mapped = map_session_error(error);
            assert_eq!(
                mapped,
                AgentBackendError::BackendUnavailable {
                    detail: expected_detail,
                },
                "detail must be the source error's own words, verbatim"
            );
        }
    }

    #[test]
    fn a_json_rpc_error_is_protocol_rejected() {
        let error = SessionError::Rpc {
            method: "thread/start".to_string(),
            code: -32602,
            message: "thread/start.dynamicTools requires experimentalApi capability".to_string(),
        };
        let expected_detail = error.to_string();
        let mapped = map_session_error(error);
        assert_eq!(
            mapped,
            AgentBackendError::ProtocolRejected {
                detail: expected_detail
            }
        );
        assert!(mapped.to_string().contains("experimentalApi"));
    }

    #[test]
    fn a_decode_failure_is_protocol_rejected_not_a_run_failure() {
        let error = SessionError::MissingField {
            method: "turn/completed".to_string(),
            field: "params",
        };
        let expected_detail = error.to_string();
        assert_eq!(
            map_session_error(error),
            AgentBackendError::ProtocolRejected {
                detail: expected_detail
            }
        );
    }

    // --- construction (no process) -----------------------------------------

    /// Constructing a backend must not spawn anything — the app-server starts
    /// on the first seam call, which is what keeps this test (and the fast
    /// tier it runs in) free of live processes.
    #[test]
    fn construction_spawns_no_process_and_resolves_no_model() {
        let backend = CodexAgentBackend::new().expect("build the backend");
        assert!(backend.session.is_none());
        assert_eq!(backend.resolved_model(), None);
        assert_eq!(backend.turn_timeout(), DEFAULT_TURN_TIMEOUT);
        let backend = backend.with_turn_timeout(Duration::from_secs(30));
        assert_eq!(backend.turn_timeout(), Duration::from_secs(30));
        backend
            .shutdown()
            .expect("shutdown of an unconnected backend");
    }
}
