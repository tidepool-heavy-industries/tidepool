//! The MCP server implementation: the [`TidepoolMcpServer`] public wrapper, its
//! non-generic [`TidepoolMcpServerImpl`] core, and the [`ServerHandler`] glue
//! that dispatches the `eval`/`resume`/`abort` tools.
//!
//! The session-driving logic (timeout-as-yield, continuation eviction,
//! result/suspension handling) lives on `TidepoolMcpServerImpl`; the
//! Ask-effect state machine it drives is in [`crate::ask`]. Builder methods on
//! `TidepoolMcpServer<H>` assemble the preamble, effect-stack type, and tool
//! description (from [`crate::preamble`] / [`crate::eval_prep`]) and start the
//! stdio / HTTP transports.

use crate::*;
use dyn_clone::{clone_trait_object, DynClone};
use parking_lot::Mutex;
use rmcp::{
    model::*, service::RequestContext, ErrorData as McpError, RoleServer, ServerHandler, ServiceExt,
};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use tidepool_runtime::DispatchEffect;
use tokio::io::{stdin, stdout};
use tokio::time::{timeout, Duration};

/// Trait combining effect dispatch with cloning for the MCP server.
pub trait McpEffectHandler:
    DispatchEffect<CapturedOutput> + DynClone + Send + Sync + 'static
{
}
clone_trait_object!(McpEffectHandler);

impl<T> McpEffectHandler for T where
    T: DispatchEffect<CapturedOutput> + Clone + Send + Sync + 'static
{
}

/// Generic MCP server wrapper that compiles and runs Haskell via Tidepool.
#[derive(Clone)]
pub struct TidepoolMcpServer<H> {
    pub(crate) inner: TidepoolMcpServerImpl,
    pub(crate) _phantom: PhantomData<H>,
}

/// Non-generic internal implementation to satisfy trait requirements.
#[derive(Clone)]
pub struct TidepoolMcpServerImpl {
    pub(crate) handler_factory: Arc<dyn McpEffectHandler>,
    pub(crate) include: Vec<PathBuf>,
    /// Generated `Tidepool/Effects.hs` source, kept so the eval path can
    /// re-materialize its staging dir if it is reaped mid-session.
    pub(crate) effects_source: String,
    pub(crate) haskell_preamble: String,
    pub(crate) effect_stack_type: String,
    pub(crate) eval_tool_description: String,
    // User library support
    pub(crate) has_user_library: bool,
    // Ask effect support
    pub(crate) ask_tag: u64,
    // Effect names for error annotation (indexed by tag)
    pub(crate) effect_names: Vec<String>,
    pub(crate) continuations: Arc<Mutex<HashMap<String, EvalSession>>>,
    pub(crate) next_cont_id: Arc<AtomicU64>,
    pub(crate) eval_semaphore: Arc<tokio::sync::Semaphore>,
    pub(crate) orphaned_threads: Arc<AtomicUsize>,
}

impl TidepoolMcpServerImpl {
    fn next_continuation_id(&self) -> String {
        let id = self.next_cont_id.fetch_add(1, Ordering::Relaxed);
        format!("cont_{}", id)
    }

    /// Evict the oldest continuation, freeing its semaphore permit.
    /// AwaitingAnswer: dropping `EvalSession` drops `response_tx` → the
    /// blocked eval thread's `response_rx.recv()` returns Err → thread
    /// exits → permit freed. Paused: the thread is parked on the gate's
    /// condvar (dropping the session would leak it parked forever) — wake
    /// it with an abort and reap; the permit frees when it exits.
    fn evict_oldest_continuation(&self) {
        let mut conts = self.continuations.lock();
        if let Some(oldest_key) = conts
            .iter()
            .min_by_key(|(_, s)| s.created_at)
            .map(|(k, _)| k.clone())
        {
            tracing::info!(cont_id = %oldest_key, "evicting oldest continuation under pressure");
            if let Some(session) = conts.remove(&oldest_key) {
                if matches!(session.kind, SessionKind::Paused) {
                    session
                        .gate
                        .request_abort("evicted under pressure while paused".into());
                    self.reap_detached(session.thread);
                }
            }
        }
    }

    /// Default-timeout entry (resume/abort + tests): drive a session with the
    /// server's standard window. Delegates to the timeout-aware impl.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn handle_session_result(
        &self,
        op: &str,
        session_rx: tokio::sync::mpsc::UnboundedReceiver<SessionMessage>,
        source: Arc<str>,
        response_tx: std::sync::mpsc::Sender<ResumeMsg>,
        captured_output: CapturedOutput,
        handle: Option<JoinHandle<()>>,
        gate: Arc<PauseGate>,
    ) -> Result<CallToolResult, McpError> {
        self.handle_session_result_with_timeout(
            op,
            session_rx,
            source,
            response_tx,
            captured_output,
            handle,
            gate,
            EVAL_TIMEOUT_SECS,
        )
        .await
    }

    /// Drive an eval session to its first result/yield, with the window set by
    /// `timeout_secs`. At the window: an eval at (or about to reach) an effect
    /// boundary parks as a continuation; a pure runaway is detached. The
    /// per-eval `timeout_secs` (clamped by the caller) lets heavy dev evals run
    /// longer than the default without weakening the runaway backstop.
    #[allow(clippy::too_many_arguments)]
    async fn handle_session_result_with_timeout(
        &self,
        op: &str,
        mut session_rx: tokio::sync::mpsc::UnboundedReceiver<SessionMessage>,
        source: Arc<str>,
        response_tx: std::sync::mpsc::Sender<ResumeMsg>,
        captured_output: CapturedOutput,
        mut handle: Option<JoinHandle<()>>,
        gate: Arc<PauseGate>,
        timeout_secs: u64,
    ) -> Result<CallToolResult, McpError> {
        let eval_timeout = Duration::from_secs(timeout_secs);
        let received = match timeout(eval_timeout, session_rx.recv()).await {
            Ok(received) => received,
            Err(_elapsed) => {
                // The window expired. A message may have raced the
                // deadline (e.g. the thread suspended on an Ask just as
                // we timed out) — drain it rather than pausing a thread
                // that is actually parked on the answer channel.
                match session_rx.try_recv() {
                    Ok(message) => Some(message),
                    Err(_) => {
                        // Timeout is a YIELD POINT, not a failure: ask the
                        // eval thread to pause at its next effect dispatch.
                        // An eval only computes during an MCP call.
                        gate.request_pause();
                        let gate_for_wait = Arc::clone(&gate);
                        let parked = tokio::task::spawn_blocking(move || {
                            gate_for_wait.parked_or_in_effect(Duration::from_secs(2))
                        })
                        .await
                        .unwrap_or(false);

                        let output = captured_output.snapshot();
                        if parked {
                            // Parked (or mid-effect and will park at the
                            // boundary): hand the caller a continuation.
                            tracing::info!(
                                "{} paused after {}s — parked as continuation",
                                op,
                                timeout_secs
                            );
                            let cont_id = self.next_continuation_id();
                            let mut json_obj = serde_json::json!({
                                "suspended": true,
                                "paused": true,
                                "continuation_id": cont_id,
                                "note": format!(
                                    "Paused after {}s at an effect boundary (no compute happens \
                                     while paused). Call resume with this continuation_id to run \
                                     another window (response payload ignored), or abort to kill it.",
                                    timeout_secs
                                ),
                            });
                            if !output.is_empty() {
                                if let Some(obj) = json_obj.as_object_mut() {
                                    obj.insert("output".into(), serde_json::Value::from(output));
                                }
                            }
                            self.continuations.lock().insert(
                                cont_id,
                                EvalSession {
                                    response_tx,
                                    session_rx,
                                    source: Arc::clone(&source),
                                    created_at: std::time::Instant::now(),
                                    captured_output,
                                    kind: SessionKind::Paused,
                                    thread: handle.take(),
                                    gate,
                                },
                            );
                            return Ok(CallToolResult::success(vec![Content::text(
                                json_obj.to_string(),
                            )]));
                        }

                        // Pure-compute runaway: no effect dispatch within
                        // the grace period — nothing to park at. Old
                        // timeout behavior, reserved for exactly this case:
                        // detach to the reaper (the abort flag terminates
                        // it if it ever reaches an effect).
                        tracing::error!(
                            "{} reached no yield point within grace after {}s — detaching",
                            op,
                            timeout_secs
                        );
                        gate.request_abort(
                            "detached after timeout (no yield point reached)".into(),
                        );
                        self.reap_detached(handle.take());
                        let mut detail = format!(
                            "{} timed out after {}s WITHOUT reaching an effect boundary — \
                             likely a pure infinite loop or unbounded pure recursion. The \
                             thread was detached.",
                            op, timeout_secs
                        );
                        if !output.is_empty() {
                            detail.push_str("\n\n## Output Before Timeout\n");
                            for line in &output {
                                detail.push_str(line);
                                detail.push('\n');
                            }
                        }
                        let error_msg = format_error_with_source(
                            FailureClass::Timeout,
                            "Timeout",
                            &detail,
                            &source,
                        );
                        return Ok(CallToolResult::error(vec![Content::text(error_msg)]));
                    }
                }
            }
        };
        match received {
            Some(message) => {
                let output = match &message {
                    SessionMessage::Completed { .. } | SessionMessage::Error { .. } => {
                        captured_output.drain()
                    }
                    SessionMessage::Suspended { .. } => captured_output.snapshot(),
                };

                match message {
                    SessionMessage::Completed { result } => {
                        tracing::info!("{} completed", op);
                        let mut response = String::new();
                        if !output.is_empty() {
                            response.push_str("## Output\n");
                            for line in &output {
                                response.push_str(line);
                                response.push('\n');
                            }
                            response.push_str("\n## Result\n");
                        }
                        response.push_str(&result);
                        Ok(CallToolResult::success(vec![Content::text(response)]))
                    }
                    SessionMessage::Suspended { prompt, meta } => {
                        tracing::info!(prompt = %prompt, "{} suspended on Ask", op);
                        let cont_id = self.next_continuation_id();
                        let mut json_obj = serde_json::json!({
                            "suspended": true,
                            "continuation_id": cont_id,
                            "prompt": prompt,
                        });
                        // AskWith metadata: hoist "schema" top-level (it
                        // arms resume validation); everything else rides
                        // under "meta" verbatim — no reserved-key
                        // collisions, no silent drops.
                        let mut expected_schema = None;
                        match meta {
                            Some(serde_json::Value::Object(mut meta_map)) => {
                                if let Some(obj) = json_obj.as_object_mut() {
                                    if let Some(schema) = meta_map.remove("schema") {
                                        obj.insert("schema".into(), schema.clone());
                                        expected_schema = Some(schema);
                                    }
                                    if !meta_map.is_empty() {
                                        obj.insert(
                                            "meta".into(),
                                            serde_json::Value::Object(meta_map),
                                        );
                                    }
                                }
                            }
                            Some(other) => {
                                // Non-object metadata: pass through verbatim.
                                if let Some(obj) = json_obj.as_object_mut() {
                                    obj.insert("meta".into(), other);
                                }
                            }
                            None => {}
                        }
                        if !output.is_empty() {
                            if let Some(obj) = json_obj.as_object_mut() {
                                obj.insert("output".into(), serde_json::Value::from(output));
                            }
                        }
                        self.continuations.lock().insert(
                            cont_id,
                            EvalSession {
                                response_tx,
                                session_rx,
                                source: Arc::clone(&source),
                                created_at: std::time::Instant::now(),
                                captured_output,
                                kind: SessionKind::AwaitingAnswer { expected_schema },
                                thread: handle.take(),
                                gate,
                            },
                        );
                        Ok(CallToolResult::success(vec![Content::text(
                            json_obj.to_string(),
                        )]))
                    }
                    SessionMessage::Error { error } => {
                        // The in-band error channel carries both clean Haskell
                        // errors and runtime yields (and caught JIT signals) —
                        // split them by content for the failure-class tag.
                        let class = FailureClass::classify_error_text(&error);
                        let mut error_msg =
                            format_error_with_source(class, "Error", &error, &source);
                        if !output.is_empty() {
                            error_msg.push_str("\n\n## Output So Far\n");
                            for line in &output {
                                error_msg.push_str(line);
                                error_msg.push('\n');
                            }
                        }
                        tracing::error!("{} failed: {}", op, error);
                        Ok(CallToolResult::error(vec![Content::text(error_msg)]))
                    }
                }
            }
            None => {
                tracing::error!("{} thread crashed", op);
                let mut crash_info = String::new();

                // The program's last words are the cheapest forensics there
                // are — surface anything it printed before the signal.
                let output = captured_output.snapshot();
                if !output.is_empty() {
                    crash_info.push_str("\n\n## Output Before Crash\n");
                    for line in &output {
                        crash_info.push_str(line);
                        crash_info.push('\n');
                    }
                }

                // If we have the handle, joining it gives us the panic payload
                if let Some(h) = handle.take() {
                    if let Err(e) = h.join() {
                        crash_info.push_str("\n\n## Thread Panic\n");
                        crash_info.push_str(&format_panic_payload(e));
                    }
                }

                let crash_log = async {
                    use tokio::io::{AsyncReadExt, AsyncSeekExt};
                    let mut file = tokio::fs::File::open(".tidepool/crash.log").await.ok()?;
                    let meta = file.metadata().await.ok()?;
                    let len = meta.len();
                    const MAX_CRASH_LOG_BYTES: u64 = 65536;
                    if len > MAX_CRASH_LOG_BYTES {
                        file.seek(std::io::SeekFrom::End(-(MAX_CRASH_LOG_BYTES as i64)))
                            .await
                            .ok()?;
                    }
                    let mut buf = Vec::new();
                    file.read_to_end(&mut buf).await.ok()?;
                    Some(String::from_utf8_lossy(&buf).into_owned())
                }
                .await;

                if let Some(content) = crash_log {
                    let lines: Vec<&str> = content.lines().rev().take(5).collect();
                    if !lines.is_empty() {
                        crash_info.push_str("\n\n## Recent Crash Log Entries\n```\n");
                        for line in lines.into_iter().rev() {
                            crash_info.push_str(line);
                            crash_info.push('\n');
                        }
                        crash_info.push_str("```\n");
                    }
                }
                let error_msg = format_error_with_source(
                    FailureClass::SignalCrash,
                    "Crash",
                    &format!(
                        "{} thread crashed (likely SIGILL from exhausted case branch or SIGSEGV from invalid memory access). Set RUST_LOG=debug for JIT diagnostics on stderr.{}",
                        op, crash_info
                    ),
                    &source,
                );
                Ok(CallToolResult::error(vec![Content::text(error_msg)]))
            }
        }
    }

    /// Detach an eval thread to a background reaper: a grace period, then
    /// join, with orphan accounting (the admission gate refuses new evals
    /// when too many detached threads are still running). std::thread, not
    /// spawn_blocking — a tight infinite loop must not starve the runtime's
    /// blocking pool.
    fn reap_detached(&self, handle: Option<JoinHandle<()>>) {
        if let Some(h) = handle {
            let orphan_count = Arc::clone(&self.orphaned_threads);
            orphan_count.fetch_add(1, Ordering::Relaxed);
            std::thread::spawn(move || {
                // Grace period for the thread to hit an Ask (where a queued
                // Abort poison-pill terminates it) or return naturally.
                std::thread::sleep(Duration::from_secs(2));
                let _ = h.join();
                orphan_count.fetch_sub(1, Ordering::Relaxed);
            });
        }
    }

    pub(crate) async fn eval(&self, req: EvalRequest) -> Result<CallToolResult, McpError> {
        tracing::info!(len = req.code.len(), "eval request");

        if self.orphaned_threads.load(Ordering::Relaxed) >= MAX_ORPHANED_EVALS {
            return Ok(CallToolResult::error(vec![Content::text(
                "Server overloaded: too many timed-out evaluations still running. Please wait.",
            )]));
        }

        // Reject unsafe/IO imports before compilation
        for imp in req.imports.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if let Some(module) = rejected_import(imp) {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Blocked import: `{}` is not available in the Tidepool sandbox.",
                    module,
                ))]));
            }
        }

        let mut all_imports = aeson_imports();
        // Tidepool.QQ is injected ONLY when a quoter token appears: the
        // import alone drags the quoter home-module graph into every eval
        // (~+385ms, plans/qq-spike.md M3); no-splice evals keep an
        // import-identical (and cache-identical) module source. The
        // QuasiQuotes/ViewPatterns PRAGMAS are always-on in build_preamble
        // (root decision — see the comment there for the latency FIXME).
        if uses_qq(&req.code) || uses_qq(&req.helpers) {
            all_imports.push_str("Tidepool.QQ (fmt, j, patch, sg, uri)\n");
        }
        all_imports.push_str(&req.imports);
        let normalized_input = req.input.as_ref().map(normalize_input);
        let source: Arc<str> = template_haskell(
            &self.haskell_preamble,
            &self.effect_stack_type,
            &req.code,
            &all_imports,
            &req.helpers,
            normalized_input.as_ref(),
            Some(req.max_len.unwrap_or(4096)),
        )
        .into();

        let handlers = dyn_clone::clone_box(&*self.handler_factory);
        // Self-heal: re-materialize Tidepool.Effects if its staging dir was
        // reaped mid-session (macOS purges $TMPDIR / cache). Cheap stat when
        // intact; rewrites only if the file is missing.
        if let Err(e) = write_effects_module_src(&self.effects_source) {
            eprintln!("[tidepool] failed to refresh Tidepool.Effects module: {e}");
        }
        let include_refs: Vec<PathBuf> = self.include.clone();
        let source_for_blocking = Arc::clone(&source);
        let captured = CapturedOutput::new();
        let captured_for_blocking = captured.clone();
        let ask_tag = self.ask_tag;
        let effect_names = self.effect_names.clone();

        // Create channels for Ask effect communication + the pause gate
        let (session_tx, session_rx) = tokio::sync::mpsc::unbounded_channel::<SessionMessage>();
        let (response_tx, response_rx) = std::sync::mpsc::channel::<ResumeMsg>();
        let gate = PauseGate::new();
        let gate_for_thread = Arc::clone(&gate);

        let permit = match self.eval_semaphore.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                // All slots busy — evict oldest suspended eval to free a permit
                self.evict_oldest_continuation();
                // Brief yield to let the evicted thread's permit release propagate
                tokio::task::yield_now().await;
                self.eval_semaphore
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| {
                        McpError::internal_error(
                            "Server busy: too many concurrent evaluations. Please try again in a moment.",
                            None,
                        )
                    })?
            }
        };

        // Spawn eval thread — communicates via channels; joined on timeout or completion
        let thread_session_tx = session_tx;
        let handle = std::thread::Builder::new()
            .name("tidepool-eval".into())
            .stack_size(tidepool_runtime::EVAL_STACK_SIZE)
            .spawn(move || {
                let _permit = permit;
                // Install signal handlers so SIGILL/SIGSEGV from JIT code
                // are caught via sigsetjmp/siglongjmp instead of killing
                // the whole server process.
                tidepool_codegen::signal_safety::install();

                let include_paths: Vec<&Path> = include_refs
                    .iter()
                    .map(std::path::PathBuf::as_path)
                    .collect();
                let mut ask_dispatcher = AskDispatcher {
                    inner: handlers,
                    ask_tag,
                    session_tx: thread_session_tx.clone(),
                    response_rx,
                    gate: gate_for_thread,
                };

                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    tidepool_runtime::compile_and_run(
                        &source_for_blocking,
                        "result",
                        &include_paths,
                        &mut ask_dispatcher,
                        &captured_for_blocking,
                    )
                }));

                match result {
                    Ok(Ok(eval_result)) => {
                        let _ = thread_session_tx.send(SessionMessage::Completed {
                            result: eval_result.to_string_pretty(),
                        });
                    }
                    Ok(Err(e)) => {
                        let diagnostics = tidepool_runtime::drain_diagnostics();
                        let mut error_detail = e.to_string();
                        // Annotate UnhandledEffect with effect names
                        if let Some(tag_str) = error_detail.strip_prefix("Unhandled effect at tag ")
                        {
                            if let Ok(tag) = tag_str.trim().parse::<usize>() {
                                if tag < effect_names.len() {
                                    let effect_name = &effect_names[tag];
                                    error_detail =
                                        format!("{} (effect: {})", error_detail, effect_name);
                                }
                            }
                            let effects_list: String = effect_names
                                .iter()
                                .enumerate()
                                .map(|(i, name)| format!("  {} = {}", i, name))
                                .collect::<Vec<_>>()
                                .join("\n");
                            error_detail
                                .push_str(&format!("\n\nRegistered effects:\n{}", effects_list));
                        }
                        if !diagnostics.is_empty() {
                            error_detail.push_str("\n\n## JIT Diagnostics\n");
                            for d in &diagnostics {
                                error_detail.push_str(d);
                                error_detail.push('\n');
                            }
                        }
                        let _ = thread_session_tx.send(SessionMessage::Error {
                            error: error_detail,
                        });
                    }
                    Err(panic_payload) => {
                        let diagnostics = tidepool_runtime::drain_diagnostics();
                        let mut error_detail = format_panic_payload(panic_payload);
                        if !diagnostics.is_empty() {
                            error_detail.push_str("\n\n## JIT Diagnostics\n");
                            for d in &diagnostics {
                                error_detail.push_str(d);
                                error_detail.push('\n');
                            }
                        }
                        let _ = thread_session_tx.send(SessionMessage::Error {
                            error: error_detail,
                        });
                    }
                }
            })
            .map_err(|e| McpError::internal_error(format!("thread spawn error: {}", e), None))?;

        // Per-eval timeout knob: default to the server window, but let callers
        // extend it (clamped) for deliberately heavy dev evals like `cargo check`.
        let timeout_secs = resolve_eval_timeout_secs(req.timeout_secs);

        // Await first message from the eval thread
        self.handle_session_result_with_timeout(
            "eval",
            session_rx,
            source,
            response_tx,
            captured,
            Some(handle),
            gate,
            timeout_secs,
        )
        .await
    }

    pub(crate) async fn resume(&self, req: ResumeRequest) -> Result<CallToolResult, McpError> {
        tracing::info!(continuation_id = %req.continuation_id, "resume request");

        // Validate-then-consume, all inside ONE lock scope (no awaits): a
        // reply that fails schema validation must NOT consume the one-shot
        // continuation (the caller fixes and retries), and two concurrent
        // resumes must not both pass validation and both send. The session
        // is carried OUT of the scope before any await (Send hygiene).
        enum Consumed {
            Session(EvalSession),
            Reply(CallToolResult),
        }
        let consumed = {
            let mut conts = self.continuations.lock();
            match conts.get(&req.continuation_id) {
                None => {
                    return Err(McpError::invalid_params(
                        format!(
                            "Unknown or expired continuation_id: {}",
                            req.continuation_id
                        ),
                        None,
                    ))
                }
                // Paused eval: nothing is listening on the channel (sending
                // would poison the next ask). Consume the entry, wake the
                // gate, and wait another window; the payload is ignored.
                Some(EvalSession {
                    kind: SessionKind::Paused,
                    ..
                }) => {
                    let session = conts
                        .remove(&req.continuation_id)
                        .expect("session present: checked under the same lock");
                    session.gate.resume_run();
                    Consumed::Session(session)
                }
                Some(EvalSession {
                    kind: SessionKind::AwaitingAnswer { expected_schema },
                    ..
                }) => {
                    let expected_schema = expected_schema.clone();
                    match validate::validate_response(expected_schema.as_ref(), &req.response) {
                        validate::Outcome::Invalid(violations) => {
                            // Anti-starvation: a retrying continuation must
                            // not be the oldest-first eviction victim while
                            // its caller fixes the reply.
                            if let Some(session) = conts.get_mut(&req.continuation_id) {
                                session.created_at = std::time::Instant::now();
                            }
                            let body = serde_json::json!({
                                "validation_failed": true,
                                "violations": violations.iter().map(validate::Violation::to_json).collect::<Vec<_>>(),
                                "schema": expected_schema,
                                "continuation_id": req.continuation_id,
                                "continuation_not_consumed": true,
                            });
                            Consumed::Reply(CallToolResult::error(vec![Content::text(format!(
                                "Response does not match the suspension's schema. Call resume \
                                 again with the same continuation_id and a corrected response \
                                 (or abort).\n{}",
                                body
                            ))]))
                        }
                        validate::Outcome::Valid(canonical) => {
                            let session = conts
                                .remove(&req.continuation_id)
                                .expect("session present: checked under the same lock");
                            // Send the canonical validated value to the
                            // blocked eval thread.
                            match session.response_tx.send(ResumeMsg::Answer(canonical)) {
                                Ok(()) => Consumed::Session(session),
                                Err(_) => {
                                    return Err(McpError::internal_error(
                                        "eval thread is no longer running",
                                        None,
                                    ))
                                }
                            }
                        }
                    }
                }
            }
        };

        let session = match consumed {
            Consumed::Reply(r) => return Ok(r),
            Consumed::Session(s) => s,
        };

        let source = session.source.clone();
        let response_tx = session.response_tx.clone();
        let captured = session.captured_output.clone();
        let gate = Arc::clone(&session.gate);

        // Await the next message from the eval thread
        self.handle_session_result(
            "resume",
            session.session_rx,
            source,
            response_tx,
            captured,
            session.thread,
            gate,
        )
        .await
    }

    pub(crate) async fn abort(&self, req: AbortRequest) -> Result<CallToolResult, McpError> {
        tracing::info!(continuation_id = %req.continuation_id, "abort request");

        let session = {
            let mut conts = self.continuations.lock();
            conts.remove(&req.continuation_id).ok_or_else(|| {
                McpError::invalid_params(
                    format!(
                        "Unknown or expired continuation_id: {}",
                        req.continuation_id
                    ),
                    None,
                )
            })?
        };

        let reason = req
            .reason
            .unwrap_or_else(|| "aborted by caller".to_string());

        match &session.kind {
            // Blocked on an ask: send the abort down the answer channel —
            // the thread wakes from recv and errors cleanly.
            SessionKind::AwaitingAnswer { .. } => {
                session
                    .response_tx
                    .send(ResumeMsg::Abort(reason))
                    .map_err(|_| {
                        McpError::internal_error("eval thread is no longer running", None)
                    })?;
            }
            // Paused at the gate: wake it with the abort — its checkpoint
            // returns Err and the eval terminates as a normal error.
            SessionKind::Paused => {
                session
                    .gate
                    .request_abort(format!("aborted by caller (while paused): {reason}"));
            }
        }

        let source = session.source.clone();
        let response_tx = session.response_tx.clone();
        let captured = session.captured_output.clone();
        let gate = Arc::clone(&session.gate);

        // The eval terminates as a normal error result carrying
        // output-so-far — and its thread + semaphore permit are freed
        // instead of waiting for pressure eviction.
        self.handle_session_result(
            "abort",
            session.session_rx,
            source,
            response_tx,
            captured,
            session.thread,
            gate,
        )
        .await
    }
}

impl ServerHandler for TidepoolMcpServerImpl {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(self.eval_tool_description.clone()),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = request.arguments.unwrap_or_default();
        match request.name.as_ref() {
            "eval" => {
                let req: EvalRequest = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| {
                        McpError::invalid_params(format!("invalid params: {}", e), None)
                    })?;
                self.eval(req).await
            }
            "resume" => {
                let req: ResumeRequest = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| {
                        McpError::invalid_params(format!("invalid params: {}", e), None)
                    })?;
                self.resume(req).await
            }
            "abort" => {
                let req: AbortRequest = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| {
                        McpError::invalid_params(format!("invalid params: {}", e), None)
                    })?;
                self.abort(req).await
            }
            _ => Err(McpError {
                code: ErrorCode::METHOD_NOT_FOUND,
                message: format!("Tool not found: {}", request.name).into(),
                data: None,
            }),
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        fn schema_to_map(
            schema: schemars::Schema,
        ) -> Result<Arc<serde_json::Map<String, serde_json::Value>>, McpError> {
            let json = serde_json::to_value(&schema).map_err(|e| {
                McpError::internal_error(format!("Failed to serialize schema: {}", e), None)
            })?;
            match json {
                serde_json::Value::Object(o) => Ok(Arc::new(o)),
                _ => Ok(Arc::new(serde_json::Map::new())),
            }
        }

        let tools = vec![
            Tool {
                name: "eval".into(),
                title: None,
                description: Some(self.eval_tool_description.clone().into()),
                input_schema: schema_to_map(schemars::schema_for!(EvalRequest))?,
                output_schema: None,
                annotations: None,
                icons: None,
                meta: None,
                execution: None,
            },
            Tool {
                name: "resume".into(),
                title: None,
                description: Some(
                    "Resume a suspended Haskell evaluation. When eval returns \
                     {\"suspended\": true, \"continuation_id\": \"...\", \"prompt\": \"...\"}, \
                     call this tool with the continuation_id and your response to the prompt. \
                     If the suspension carried a \"schema\" field, the response must be JSON \
                     matching it — pass the JSON value directly (string/enum schemas also \
                     accept raw text). A response that fails validation does NOT consume the \
                     continuation: the violations are returned and you can call resume again \
                     with the same continuation_id. If you cannot answer, call abort instead. \
                     If the suspension says \"paused\": true, the eval ran out of its time \
                     window and is parked at an effect boundary (no compute happens while \
                     paused): resume runs it another window (response ignored, may be \
                     omitted); abort kills it."
                        .into(),
                ),
                input_schema: schema_to_map(schemars::schema_for!(ResumeRequest))?,
                output_schema: None,
                annotations: None,
                icons: None,
                meta: None,
                execution: None,
            },
            Tool {
                name: "abort".into(),
                title: None,
                description: Some(
                    "Abort a suspended Haskell evaluation without answering it. Use when you \
                     cannot answer a suspension's question, or to clean up a suspended loop \
                     you are abandoning (a suspended eval pins a thread until evicted). The \
                     computation terminates with an error result (\"ask aborted by caller: \
                     <reason>\") carrying any output produced so far."
                        .into(),
                ),
                input_schema: schema_to_map(schemars::schema_for!(AbortRequest))?,
                output_schema: None,
                annotations: None,
                icons: None,
                meta: None,
                execution: None,
            },
        ];

        Ok(ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        })
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

impl<H> TidepoolMcpServer<H>
where
    H: DispatchEffect<CapturedOutput> + Clone + Send + Sync + 'static + CollectEffectDecls,
{
    /// Create a new server with the given effect handler stack.
    ///
    /// Effect declarations are collected automatically from handlers that
    /// implement `DescribeEffect`.
    pub fn new(handler: H) -> Self {
        let mut decls = H::collect_decls();
        let ask_tag = decls.len() as u64;
        decls.push(ask_decl());
        let effect_names: Vec<String> = decls.iter().map(|d| d.type_name.to_string()).collect();
        // The generated Tidepool.Effects module must be on the include path
        // for every eval (the preamble imports it). Keep its source so the
        // eval path can re-materialize it if the staging dir is reaped mid-
        // session (macOS purges $TMPDIR / cache). Failure is survivable here —
        // evals will fail with a clear missing-module error.
        let effects_source = effects_module_source(&decls);
        let mut include = Vec::new();
        match write_effects_module_src(&effects_source) {
            Ok(dir) => include.push(dir),
            Err(e) => eprintln!("[tidepool] failed to write Tidepool.Effects module: {e}"),
        }
        Self {
            inner: TidepoolMcpServerImpl {
                handler_factory: Arc::new(handler),
                include,
                effects_source,
                haskell_preamble: build_preamble(&decls, false),
                effect_stack_type: build_effect_stack_type(&decls),
                eval_tool_description: build_eval_tool_description(&decls),
                has_user_library: false,
                ask_tag,
                effect_names,
                continuations: Arc::new(Mutex::new(HashMap::new())),
                next_cont_id: Arc::new(AtomicU64::new(1)),
                eval_semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_EVALS)),
                orphaned_threads: Arc::new(AtomicUsize::new(0)),
            },
            _phantom: PhantomData,
        }
    }

    /// Add include paths for Haskell module resolution. Extends the
    /// existing set (which already contains the generated
    /// `Tidepool.Effects` dir).
    pub fn with_include(mut self, paths: Vec<PathBuf>) -> Self {
        self.inner.include.extend(paths);
        self
    }

    /// Add the bundled Tidepool prelude to the include paths.
    ///
    /// Looks for the prelude in this order:
    /// 1. `TIDEPOOL_PRELUDE_DIR` environment variable
    /// 2. The provided fallback path
    ///
    /// The prelude provides source definitions for common Prelude functions
    /// (reverse, splitAt, sort, etc.) whose GHC base library workers lack
    /// unfoldings in .hi files.
    pub fn with_prelude(mut self, fallback: PathBuf) -> Self {
        let prelude_dir = std::env::var_os("TIDEPOOL_PRELUDE_DIR").map_or(fallback, PathBuf::from);
        self.inner.include.push(prelude_dir);

        // Layered verb libraries: project-local first (walk up from CWD for a
        // `.tidepool/`), then user-global (`~/.config/tidepool/lib`, legacy
        // `~/.tidepool/lib`). Both sit AFTER the stdlib on the include path so
        // `Tidepool.*` resolves from the bundle; project is BEFORE global so a
        // project `Library`/module shadows the global one (GHC first-match-wins).
        let mut lib_dirs: Vec<PathBuf> = Vec::new();
        if let Ok(cwd) = std::env::current_dir() {
            if let Some(root) = tidepool_runtime::paths::find_project_root(&cwd) {
                let project_lib = root.join(".tidepool").join("lib");
                if project_lib.is_dir() {
                    lib_dirs.push(project_lib);
                }
            }
        }
        lib_dirs.extend(tidepool_runtime::paths::global_lib_dirs());

        for dir in &lib_dirs {
            self.inner.include.push(dir.clone());
        }

        // The `Library` digest entry-point is the first lib dir that defines it.
        let library_dir = lib_dirs
            .iter()
            .find(|d| d.join("Library.hs").exists())
            .cloned();
        self.inner.has_user_library = library_dir.is_some();
        if let Some(lib_root) = library_dir {
            // Rebuild preamble with the user library import
            let mut decls = H::collect_decls();
            decls.push(ask_decl());
            self.inner.haskell_preamble = build_preamble(&decls, true);
            // Append note + merged vocabulary digest to the tool description
            self.inner.eval_tool_description.push_str(
                "\n\nUser library: `Library` is auto-imported (project or global \
                 `.tidepool/lib/Library.hs`) and re-exports every module below — all names \
                 are in scope bare. Check this vocabulary for an existing combinator with the \
                 right shape (fold/unfold/loop/batch) BEFORE hand-rolling a recursive helper. \
                 New `data` types go in a `<lib>/<Mod>.hs` module (scaffold with \
                 `Explore.defMod`):\n",
            );
            self.inner
                .eval_tool_description
                .push_str(&library_vocab(&lib_dirs));
            self.inner.eval_tool_description.push_str(concat!(
                "\nWith the library:\n",
                "  glob \"**/*.rs\" >>= mapM (\\p -> (,) p <$> getFileSize p) <&> sizeRank 9\n",
                "  seek \"where are case traps emitted?\" 5  -- steered search: suspends to you each round\n",
            ));
            // PATTERNS.md lives beside the active Library dir (at `.tidepool/`).
            let patterns = lib_root.parent().map(|p| p.join("PATTERNS.md"));
            if patterns.as_deref().is_some_and(std::path::Path::exists) {
                self.inner.eval_tool_description.push_str(
                    "\nPattern catalog: read `.tidepool/PATTERNS.md` for composition idioms.\n",
                );
            }
        }

        self
    }

    /// Start the MCP server on stdio transport.
    pub async fn serve_stdio(self) -> Result<(), Box<dyn std::error::Error>> {
        self.inner
            .serve((stdin(), stdout()))
            .await?
            .waiting()
            .await?;
        Ok(())
    }

    /// Start the MCP server on streamable HTTP transport.
    pub async fn serve_http(
        self,
        addr: std::net::SocketAddr,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use rmcp::transport::streamable_http_server::{
            session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
        };
        use std::sync::Arc;

        let template = self.inner;
        let config = StreamableHttpServerConfig::default();
        let cancel = config.cancellation_token.clone();
        let service = StreamableHttpService::new(
            move || Ok(template.clone()),
            Arc::new(LocalSessionManager::default()),
            config,
        );
        async fn health() -> axum::Json<serde_json::Value> {
            axum::Json(serde_json::json!({"status": "ok"}))
        }

        let router = axum::Router::new()
            .route("/health", axum::routing::get(health))
            .nest_service("/mcp", service);
        let listener = tokio::net::TcpListener::bind(addr).await?;
        eprintln!(
            "Tidepool MCP v{} listening on http://{}/mcp",
            env!("CARGO_PKG_VERSION"),
            addr,
        );
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                tokio::signal::ctrl_c().await.ok();
                cancel.cancel();
            })
            .await?;
        Ok(())
    }
}
