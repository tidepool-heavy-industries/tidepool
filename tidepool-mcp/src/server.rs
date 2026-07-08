//! The MCP server implementation: the [`TidepoolMcpServer`] public wrapper, its
//! non-generic [`TidepoolMcpServerImpl`] core, and the [`ServerHandler`] glue
//! that dispatches the `eval`/`resume`/`abort` tools.
//!
//! The session-driving machinery (timeout-as-yield, continuation registry,
//! the `Ask`-effect suspend/resume state machine) lives in
//! `tidepool_runtime::session::SessionEngine` — this module only does eval
//! PREP (import rejection, module templating, lib fault-isolation) and wire
//! RENDERING (`render_outcome` maps a `TurnOutcome` to the tool's
//! `CallToolResult`, byte-identical to the pre-engine wire shape). Builder
//! methods on `TidepoolMcpServer<H>` assemble the preamble, effect-stack
//! type, and tool description (from [`crate::preamble`] / [`crate::eval_prep`])
//! and start the stdio / HTTP transports.

use crate::*;
use dyn_clone::{clone_trait_object, DynClone};
use rmcp::{
    model::*, service::RequestContext, ErrorData as McpError, RoleServer, ServerHandler, ServiceExt,
};
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::Arc;
use tidepool_runtime::session::{
    AbortOutcome, EngineConfig, RenderPolicy, ResumeOutcome, Retention, SessionEngine, StartError,
    StartTurn, TurnOutcome,
};
use tidepool_runtime::DispatchEffect;
use tokio::io::{stdin, stdout};

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
    /// Generated `Tidepool/Orchestrate.hs` source, co-located with the effects
    /// module in the same content-addressed staging dir (re-materialized
    /// together on self-heal).
    pub(crate) orchestrate_source: String,
    pub(crate) haskell_preamble: String,
    pub(crate) effect_stack_type: String,
    pub(crate) eval_tool_description: String,
    // User library support
    pub(crate) has_user_library: bool,
    // Ask effect support
    pub(crate) ask_tag: u64,
    // Effect names for error annotation (indexed by tag)
    pub(crate) effect_names: Vec<String>,
    // MCP resource backing: the raw sources rendered on demand by `read_resource`
    // (per-effect detail, live library vocab, patterns, stdlib module sources).
    pub(crate) effect_decls: Vec<EffectDecl>,
    pub(crate) lib_dirs: Vec<PathBuf>,
    pub(crate) patterns_path: Option<PathBuf>,
    pub(crate) stdlib_dir: Option<PathBuf>,
    // Advertise the `help` TOOL (reference content via tools/call). OFF by
    // default — only worth it for clients that can't read MCP resources, where
    // it would otherwise be redundant tool clutter. Toggled by `--help-tool`.
    pub(crate) help_tool: bool,
    /// The session-turn substrate: owns the eval thread, timeout→park→detach
    /// orchestration, the parked-continuation registry, and the concurrency
    /// pool. See `tidepool_runtime::session::engine` for the full contract.
    pub(crate) engine: Arc<SessionEngine<CapturedOutput>>,
}

impl TidepoolMcpServerImpl {
    /// Borrow the raw sources that back the MCP resources (per-effect detail,
    /// library vocab, patterns, stdlib module sources).
    fn resource_ctx(&self) -> crate::resources::ResourceCtx<'_> {
        crate::resources::ResourceCtx {
            effects: &self.effect_decls,
            lib_dirs: &self.lib_dirs,
            patterns_path: self.patterns_path.as_deref(),
            stdlib_dir: self.stdlib_dir.as_deref(),
        }
    }

    pub(crate) async fn eval(&self, req: EvalRequest) -> Result<CallToolResult, McpError> {
        tracing::info!(len = req.code.len(), "eval request");

        // Shed load before doing any prep work: the engine re-checks this at
        // admission, but reading it here first skips the (non-trivial)
        // source-assembly cost when the server is already overloaded.
        if self.engine.orphaned_count() >= MAX_ORPHANED_EVALS {
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
        // (~+385ms); no-splice evals keep an
        // import-identical (and cache-identical) module source. The
        // QuasiQuotes/ViewPatterns PRAGMAS are always-on in build_preamble
        // (root decision — see the comment there for the latency FIXME).
        if uses_qq(&req.code) || uses_qq(&req.helpers) {
            all_imports.push_str("Tidepool.QQ (fmt, j, patch, uri)\n");
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
        // Self-heal: re-materialize Tidepool.Effects + Tidepool.Orchestrate if
        // their staging dir was reaped mid-session (macOS purges $TMPDIR /
        // cache). Cheap stats when intact; rewrites only missing files.
        if let Err(e) = write_generated_modules(&self.effects_source, &self.orchestrate_source) {
            eprintln!("[tidepool] failed to refresh generated Tidepool modules: {e}");
        }
        let captured = CapturedOutput::new();

        // Fault-isolate the verb-library layer (issue #322): if a `.tidepool/lib`
        // module is broken, `import Library` fails for EVERY eval, including the
        // `writeFile` that would repair it. Probe the facade; on breakage, prepend
        // a sanitized `Library.hs` (re-exporting only the modules that compile) so
        // healthy verbs still work and the note names the culprit. The probe is
        // blocking (shells out to extract) and memoized on a lib-snapshot hash, so
        // the steady state is cheap — but run it off the async executor anyway.
        let include_refs: Vec<PathBuf> = if self.has_user_library {
            let lib_dirs = self.lib_dirs.clone();
            let base_include = self.include.clone();
            let layer = tokio::task::spawn_blocking(move || {
                crate::isolate_lib_layer(&lib_dirs, &base_include)
            })
            .await
            .unwrap_or_default();
            if let Some(note) = layer.brick_note {
                captured.push(note);
            }
            layer
                .prepend_include
                .into_iter()
                .chain(self.include.iter().cloned())
                .collect()
        } else {
            self.include.clone()
        };

        // Per-eval timeout knob: default to the server window, but let callers
        // extend it (clamped) for deliberately heavy dev evals like `cargo check`.
        let timeout_secs = resolve_eval_timeout_secs(req.timeout_secs);

        let outcome = self
            .engine
            .start_turn(StartTurn {
                source,
                include: include_refs,
                handlers,
                ask_tag: self.ask_tag,
                effect_names: self.effect_names.clone(),
                captured,
                nursery_size: tidepool_runtime::DEFAULT_NURSERY_SIZE,
                timeout_secs,
            })
            .await;

        match outcome {
            Ok(outcome) => Ok(self.render_outcome("eval", outcome).await),
            Err(StartError::Overloaded) => Ok(CallToolResult::error(vec![Content::text(
                "Server overloaded: too many timed-out evaluations still running. Please wait.",
            )])),
            Err(StartError::Busy) => Err(McpError::internal_error(
                "Server busy: too many concurrent evaluations. Please try again in a moment.",
                None,
            )),
        }
    }

    pub(crate) async fn resume(&self, req: ResumeRequest) -> Result<CallToolResult, McpError> {
        tracing::info!(continuation_id = %req.continuation_id, "resume request");

        // The validator runs UNDER the engine's registry lock (validate-then-
        // consume is atomic); its error payload carries the violations AND the
        // continuation's expected_schema (the engine doesn't hand that back on
        // `Invalid`, so the closure smuggles it out itself) so the caller can
        // render the same validation-failed body as before.
        let outcome = self
            .engine
            .resume(
                &req.continuation_id,
                |schema| match validate::validate_response(schema, &req.response) {
                    validate::Outcome::Valid(canonical) => Ok(canonical),
                    validate::Outcome::Invalid(violations) => Err((violations, schema.cloned())),
                },
            )
            .await;

        match outcome {
            ResumeOutcome::NotFound => Err(McpError::invalid_params(
                format!(
                    "Unknown or expired continuation_id: {}",
                    req.continuation_id
                ),
                None,
            )),
            ResumeOutcome::ThreadGone => Err(McpError::internal_error(
                "eval thread is no longer running",
                None,
            )),
            ResumeOutcome::Invalid((violations, schema)) => {
                let body_text = crate::server_common::validation_failed_body(
                    "resume",
                    "abort",
                    &violations,
                    schema.as_ref(),
                    &req.continuation_id,
                );
                Ok(CallToolResult::error(vec![Content::text(body_text)]))
            }
            ResumeOutcome::Driven(outcome) => Ok(self.render_outcome("resume", outcome).await),
        }
    }

    pub(crate) async fn abort(&self, req: AbortRequest) -> Result<CallToolResult, McpError> {
        tracing::info!(continuation_id = %req.continuation_id, "abort request");

        let reason = req
            .reason
            .unwrap_or_else(|| "aborted by caller".to_string());

        match self.engine.abort(&req.continuation_id, reason).await {
            AbortOutcome::NotFound => Err(McpError::invalid_params(
                format!(
                    "Unknown or expired continuation_id: {}",
                    req.continuation_id
                ),
                None,
            )),
            AbortOutcome::ThreadGone => Err(McpError::internal_error(
                "eval thread is no longer running",
                None,
            )),
            AbortOutcome::Driven(outcome) => Ok(self.render_outcome("abort", outcome).await),
        }
    }

    /// Map a driven turn's classified outcome to its tool-call wire shape.
    /// Every variant reproduces the pre-engine `handle_session_result_with_timeout`
    /// rendering byte-for-byte — the engine carries only classified data, never
    /// wire text, so this is the ONE place per server that owns the mapping.
    async fn render_outcome(&self, op: &str, outcome: TurnOutcome) -> CallToolResult {
        match outcome {
            TurnOutcome::Completed { output, result } => {
                tracing::info!("{} completed", op);
                let response = crate::server_common::format_with_output(&output, &result);
                CallToolResult::success(vec![Content::text(response)])
            }
            TurnOutcome::SuspendedAsk {
                cont_id,
                prompt,
                meta,
                output,
            } => {
                tracing::info!(prompt = %prompt, "{} suspended on Ask", op);
                let (mut json_obj, _expected_schema) =
                    crate::server_common::build_suspension_envelope(&cont_id, &prompt, meta);
                if !output.is_empty() {
                    if let Some(obj) = json_obj.as_object_mut() {
                        obj.insert("output".into(), serde_json::Value::from(output));
                    }
                }
                CallToolResult::success(vec![Content::text(json_obj.to_string())])
            }
            TurnOutcome::Paused {
                cont_id,
                output,
                timeout_secs,
            } => {
                tracing::info!(
                    "{} paused after {}s — parked as continuation",
                    op,
                    timeout_secs
                );
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
                CallToolResult::success(vec![Content::text(json_obj.to_string())])
            }
            TurnOutcome::Error {
                class,
                phase,
                detail,
                output,
                source,
                diagnostics,
            } => {
                let mut error_msg = format_error_with_source(
                    class,
                    phase,
                    "Error",
                    &detail,
                    diagnostics.as_deref(),
                    &source,
                );
                if !output.is_empty() {
                    error_msg.push_str("\n\n## Output So Far\n");
                    for line in &output {
                        error_msg.push_str(line);
                        error_msg.push('\n');
                    }
                }
                tracing::error!("{} failed: {}", op, detail);
                CallToolResult::error(vec![Content::text(error_msg)])
            }
            TurnOutcome::TimedOut {
                class,
                phase,
                compiling,
                timeout_secs,
                output,
                source,
            } => {
                tracing::error!(
                    "{} reached no yield point within grace after {}s — detaching",
                    op,
                    timeout_secs
                );
                let mut detail = if compiling {
                    format!(
                        "{} timed out after {}s during COMPILATION — the first \
                         eval of a new expression pays a GHC compile (~2-6s cold; \
                         the result is cached). Not a fault in your code: retry, \
                         or raise timeout_secs.",
                        op, timeout_secs
                    )
                } else {
                    format!(
                        "{} timed out after {}s WITHOUT reaching an effect boundary — \
                         likely a pure infinite loop or unbounded pure recursion. The \
                         thread was detached.",
                        op, timeout_secs
                    )
                };
                if !output.is_empty() {
                    detail.push_str("\n\n## Output Before Timeout\n");
                    for line in &output {
                        detail.push_str(line);
                        detail.push('\n');
                    }
                }
                let error_msg =
                    format_error_with_source(class, phase, "Timeout", &detail, None, &source);
                CallToolResult::error(vec![Content::text(error_msg)])
            }
            TurnOutcome::Crashed {
                output,
                thread_panic,
                source,
            } => {
                tracing::error!("{} thread crashed", op);
                let mut crash_info = String::new();

                // The program's last words are the cheapest forensics there
                // are — surface anything it printed before the signal.
                if !output.is_empty() {
                    crash_info.push_str("\n\n## Output Before Crash\n");
                    for line in &output {
                        crash_info.push_str(line);
                        crash_info.push('\n');
                    }
                }

                // If we have the panic payload (the thread was joinable), surface it.
                if let Some(panic) = thread_panic {
                    crash_info.push_str("\n\n## Thread Panic\n");
                    crash_info.push_str(&panic);
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
                    FailureClass::Runtime,
                    Phase::Run,
                    "Crash",
                    &format!(
                        "{} thread crashed (likely SIGILL from exhausted case branch or SIGSEGV from invalid memory access). Set RUST_LOG=debug for JIT diagnostics on stderr.{}",
                        op, crash_info
                    ),
                    None,
                    &source,
                );
                CallToolResult::error(vec![Content::text(error_msg)])
            }
        }
    }
}

impl ServerHandler for TidepoolMcpServerImpl {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(self.eval_tool_description.clone()),
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
            ..Default::default()
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let ctx = self.resource_ctx();
        let resources = crate::resources::list(&ctx)
            .into_iter()
            .map(|d| {
                RawResource {
                    uri: d.uri,
                    name: d.name,
                    title: None,
                    description: Some(d.description),
                    mime_type: Some(d.mime.to_string()),
                    size: None,
                    icons: None,
                    meta: None,
                }
                .no_annotation()
            })
            .collect();
        Ok(ListResourcesResult {
            resources,
            next_cursor: None,
            meta: None,
        })
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        let resource_templates = crate::resources::templates()
            .into_iter()
            .map(|t| {
                RawResourceTemplate {
                    uri_template: t.uri_template.to_string(),
                    name: t.name.to_string(),
                    title: None,
                    description: Some(t.description.to_string()),
                    mime_type: Some(t.mime.to_string()),
                    icons: None,
                }
                .no_annotation()
            })
            .collect();
        Ok(ListResourceTemplatesResult {
            resource_templates,
            next_cursor: None,
            meta: None,
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let ctx = self.resource_ctx();
        match crate::resources::read(&ctx, &request.uri) {
            Some(b) => Ok(ReadResourceResult {
                contents: vec![ResourceContents::TextResourceContents {
                    uri: request.uri,
                    mime_type: Some(b.mime.to_string()),
                    text: b.text,
                    meta: None,
                }],
            }),
            None => Err(McpError::resource_not_found(
                format!("Unknown resource: {}", request.uri),
                None,
            )),
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
            "help" if self.help_tool => {
                let req: HelpRequest = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| {
                        McpError::invalid_params(format!("invalid params: {}", e), None)
                    })?;
                let ctx = self.resource_ctx();
                let text = crate::resources::help(&ctx, req.topic.as_deref().unwrap_or(""));
                Ok(CallToolResult::success(vec![Content::text(text)]))
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
        let mut tools = vec![
            crate::server_common::make_tool(
                "eval",
                &self.eval_tool_description,
                crate::eval_request_input_schema()
                    .map_err(|e| McpError::internal_error(e, None))?,
            ),
            crate::server_common::make_tool(
                "resume",
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
                 omitted); abort kills it.",
                crate::server_common::schema_to_map(schemars::schema_for!(ResumeRequest))
                    .map_err(|e| McpError::internal_error(e, None))?,
            ),
            crate::server_common::make_tool(
                "abort",
                "Abort a suspended Haskell evaluation without answering it. Use when you \
                 cannot answer a suspension's question, or to clean up a suspended loop \
                 you are abandoning (a suspended eval pins a thread until evicted). The \
                 computation terminates with an error result (\"ask aborted by caller: \
                 <reason>\") carrying any output produced so far.",
                crate::server_common::schema_to_map(schemars::schema_for!(AbortRequest))
                    .map_err(|e| McpError::internal_error(e, None))?,
            ),
        ];

        // The `help` tool is only advertised when enabled (`--help-tool`) — for
        // clients without MCP `resources` support, where it's the only way to
        // reach the depth. Resource-capable clients get it via resources/read.
        if self.help_tool {
            tools.push(crate::server_common::make_tool(
                "help",
                "Fetch reference content on demand — the same depth as the tidepool:// \
                 resources, via a plain tool call so ANY client can reach it (no \
                 resources/read support needed). topic: `guide` (how to write eval code), \
                 `schema` (Schema + ask/llm), `edits` (editing verbs), `vocab` (every verb \
                 signature in scope), `patterns` (worked examples), `effect <Name>` (e.g. \
                 `effect Fs` — one effect's constructors + helpers), or `stdlib <Module>` \
                 (e.g. `stdlib Tidepool.Prelude` — vendored source). Omit topic to list topics.",
                crate::server_common::schema_to_map(schemars::schema_for!(HelpRequest))
                    .map_err(|e| McpError::internal_error(e, None))?,
            ));
        }

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
        let orchestrate_source = orchestrate_module_source(&decls);
        let mut include = Vec::new();
        match write_generated_modules(&effects_source, &orchestrate_source) {
            Ok(dir) => include.push(dir),
            Err(e) => eprintln!("[tidepool] failed to write generated Tidepool modules: {e}"),
        }
        Self {
            inner: TidepoolMcpServerImpl {
                handler_factory: Arc::new(handler),
                include,
                effects_source,
                orchestrate_source,
                haskell_preamble: build_preamble(&decls, false),
                effect_stack_type: build_effect_stack_type(&decls),
                eval_tool_description: build_eval_tool_description(&decls),
                has_user_library: false,
                ask_tag,
                effect_names,
                effect_decls: decls.clone(),
                lib_dirs: Vec::new(),
                patterns_path: None,
                stdlib_dir: None,
                help_tool: false,
                engine: Arc::new(SessionEngine::new(EngineConfig {
                    max_concurrent: MAX_CONCURRENT_EVALS,
                    max_orphaned: MAX_ORPHANED_EVALS,
                    cont_prefix: "cont".to_string(),
                    default_timeout_secs: EVAL_TIMEOUT_SECS,
                    render: RenderPolicy::Json,
                    retention: Retention::DropAfterDone,
                })),
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

    /// Advertise the `help` tool (reference content via a plain tool call).
    /// Enable for clients that don't support MCP `resources`. When on, the eval
    /// description gains a pointer to `help` (the resource pointers aren't
    /// actionable for a client that can't read resources).
    pub fn with_help_tool(mut self, enabled: bool) -> Self {
        self.inner.help_tool = enabled;
        if enabled {
            self.inner.eval_tool_description.push_str(
                "\nNo MCP `resources` support in your client? Call the `help` tool for the same \
                 depth — topics: guide, schema, edits, vocab, patterns, `effect <Name>`, \
                 `stdlib <Module>`.\n",
            );
        }
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
        // The prelude dir holds the vendored `Tidepool/*.hs` stdlib — back the
        // `tidepool://stdlib/{module}` resources with it.
        self.inner.stdlib_dir = Some(prelude_dir.clone());
        self.inner.include.push(prelude_dir);

        // Layered verb libraries: project-local first (walk up from CWD for a
        // `.tidepool/`), then user-global (`~/.config/tidepool/lib`, legacy
        // `~/.tidepool/lib`). Both sit AFTER the stdlib on the include path so
        // `Tidepool.*` resolves from the bundle; project is BEFORE global so a
        // project `Library`/module shadows the global one (GHC first-match-wins).
        let project_root = std::env::current_dir()
            .ok()
            .and_then(|cwd| tidepool_runtime::paths::find_project_root(&cwd));
        let lib_dirs = crate::server_common::resolve_lib_dirs(project_root.as_deref());

        for dir in &lib_dirs {
            self.inner.include.push(dir.clone());
        }
        self.inner.lib_dirs = lib_dirs.clone();

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
            // The full vocabulary digest now lives in `tidepool://vocab` (pulled on
            // demand); the description keeps a short pointer instead of inlining it.
            self.inner.eval_tool_description.push_str(concat!(
                "\nUser library: `Library` is auto-imported (project/global ",
                "`.tidepool/lib/Library.hs`); all its verbs are in scope bare. Read ",
                "`tidepool://vocab` for the signatures (or call `vocab` at runtime) and ",
                "check there for an existing combinator BEFORE hand-rolling a recursive helper.\n",
            ));
            // PATTERNS.md lives beside the active Library dir (at `.tidepool/`),
            // surfaced as the `tidepool://patterns` resource.
            let patterns = lib_root.parent().map(|p| p.join("PATTERNS.md"));
            if patterns.as_deref().is_some_and(std::path::Path::exists) {
                self.inner.patterns_path = patterns;
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
