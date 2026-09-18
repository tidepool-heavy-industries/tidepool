# Structured tracing for Shoal: executable design

Survey + instrumentation plan for turning the Shoal host's plain-text log into
structured `tracing` spans with a JSON sink, so a run is reconstructable
without the model provider's own transcript. Decided already (not re-derived
here): `tracing` spans/fields, a third JSON layer on the existing registry
writing `<run_id>.jsonl` via `tracing-appender` non-blocking, content-on by
default (trusted single-user dev box), span durations from span close.

Every claim below carries a `file:line` citation against
`/home/inanna/dev/tidepool-jev` at the surveyed commit (`3103e3adc`).

---

## 1. Existing subscriber setup and where the third layer attaches

`tidepool/src/shoal.rs:1250-1299`:

- `host_pane_filter()` (1250-1252): `EnvFilter::new("warn,tidepool::shoal=info,tidepool::actor_host=info")` — the tmux pane's noise floor.
- `host_tracing_subscriber(detailed_writer, pane_writer, detailed_filter)` (1254-1275): builds exactly two `fmt::layer()`s on one `tracing_subscriber::registry()`:
  - `detailed`: `.with_ansi(false).with_writer(detailed_writer).with_filter(detailed_filter)` — the full-text log file (`<run_id>.log`), filter passed in.
  - `pane`: `.compact().without_time().with_ansi(false).with_target(false).with_writer(pane_writer).with_filter(host_pane_filter())` — the tmux pane (stderr), always `info`-and-above on `tidepool::shoal`/`tidepool::actor_host`, `warn` elsewhere.
  - Returned as `tracing_subscriber::registry().with(detailed).with(pane)` (1274) — a `Layer` stack, not nested subscribers.
- `init_host_tracing(workspace, run_id)` (1277-1299): opens `shoal_log_path(workspace, run_id)` (1238-1242, `.shoal/logs/<run_id>.log`) append-only, calls `host_tracing_subscriber(Mutex::new(file), stderr, tidepool_codegen::debug::tracing_env_filter("info")).try_init()`, then separately calls `tidepool_codegen::debug::init_logging()` (1297).

**Third layer attaches at line 1274**: `tracing_subscriber::registry().with(detailed).with(pane).with(json)` — a third `fmt::layer()` (or a bespoke JSON layer) built the same way as `detailed`/`pane`, pointed at a `tracing_appender::non_blocking` writer over `shoal_trace_path(workspace, run_id)` (new sibling of `shoal_log_path`/`shoal_compiler_log_path`, same `.shoal/logs/` directory, `.jsonl` extension — pattern at 1238-1248). Needs `.json()` on the `fmt::layer()` builder and its own filter (see §6 for what that filter must exclude).

**Cargo feature gap**: `tidepool/Cargo.toml:56` currently enables only `features = ["env-filter"]` on `tracing-subscriber`. The `json` feature must be added (`features = ["env-filter", "json"]`) before `.json()` compiles. `tracing-appender` is not a dependency anywhere in the workspace (`grep` for it in `Cargo.toml` returns nothing) — this is the "new dependency, approved" call site: add to `tidepool/Cargo.toml`.

**The `try_init()` at 1295 sets the global default subscriber once.** `init_logging()` (called right after, 1297) is a *different* mechanism — see the caveat in §1a below; it does not re-init or conflict with the `tracing` dispatcher.

**Guard placement.** `init_host_tracing` returns `Result<PathBuf, _>` (1277-1280) — only the detailed-log path, no guard. Its one call site is `tidepool/src/bin/shoal.rs:313`, inside the `Command::Host` match arm, immediately before `tidepool::shoal::host(options).await` runs for the rest of the process's life (that `host()` call is `run_host` + `settle_host_result`, `tidepool/src/shoal.rs:901-909`; `run_host` is the function that opens the `shoal_host` span, see §2). The daemon (host) process does nothing else after this arm — it *is* the arm.

To add `tracing_appender::non_blocking`, `init_host_tracing`'s signature must change to also return the `tracing_appender::non_blocking::WorkerGuard`:
```rust
pub fn init_host_tracing(workspace: &Path, run_id: &str)
    -> Result<(PathBuf, tracing_appender::non_blocking::WorkerGuard), Box<dyn std::error::Error>>
```
and the call site becomes:
```rust
let (_log_path, _tracing_guard) = tidepool::shoal::init_host_tracing(&workspace, &run_id)?;
```
**`_tracing_guard` must be a named binding**, not `let _ = ...`. A bare `_` drops the guard immediately (end of the statement), which closes the appender's background flush channel before any span is ever written. `_log_path`/`_tracing_guard` (leading underscore, real identifier) live to the end of `main`'s scope, which is what's needed since `main` in `tidepool/src/bin/shoal.rs` is the whole process.

### 1a. Caveat: `init_logging()` is a *separate* logging system, not a tracing layer

`tidepool_codegen::debug::init_logging()` (`tidepool-codegen/src/debug.rs:359-414`) installs an `env_logger::Builder` as the global `log`-crate logger (`builder.try_init()` at 412), routing `tidepool::calls`/`scope`/`heap`/`effects`/`fp` targets (populated from `RUST_LOG` or legacy `TIDEPOOL_TRACE*` env vars) straight to stderr. This is **not** a `tracing_subscriber::Layer` and is not attached to the registry built at `shoal.rs:1274`. Any `log::info!`/`log::debug!` call site in the JIT/codegen path is invisible to the new JSON layer unless a `tracing_log::LogTracer` bridge is installed instead of (or in front of) `env_logger` — the workspace does not currently depend on `tracing-log` (not found in any `Cargo.toml`). **This is fine for the content this task cares about** (cell source, receipts, lookup queries, GHC diagnostic text, child assignment text) because all of that content already exists as owned `String`/struct data at the Rust call sites identified in §2-§3, and gets attached to spans/events directly — none of it needs to be scraped out of `log`-crate macro output. Flagged as an open question only in case a future content item *is* only observable via a `log::` call site.

---

## 2. The span tree: identifiers already in scope vs. needing threading

| Level | Identifier | Type & definition | Already in scope at the natural span site? |
|---|---|---|---|
| run | `run_id` | `String`, e.g. `tidepool/src/shoal.rs:72` (`HostOptions.run_id`), minted at `shoal.rs:465` (`uuid::Uuid::new_v4().to_string()`) | **Yes** — `run_host` (`shoal.rs:911`) takes `options: &HostOptions`; `options.run_id` used at `956-959` and `904/906`. |
| actor | `ActorRef { id: ActorId(u64), incarnation: Incarnation(u64) }` | `tidepool-actor/src/identity.rs:18-31`; `Display` gives `"{id}@{incarnation}"` (`identity.rs:37-44`) | **Yes, everywhere in the actor runtime.** `kernel.identity()` inside every `KernelBehavior` method (`resident_actor.rs:5879-6210` etc.); `state.context.identity` inside `local_actor.rs`'s message loop (`local_actor.rs:816+`, see §3). No separate "path" type exists — every log site in `tidepool/src/actor_host.rs` already Debug-formats a bare `ActorRef` (e.g. `actor = ?self.actor` at `actor_host.rs:233`), confirmed against the existing test assertion `"actor=ActorRef"` (`tidepool/src/actor_host.rs:5782`). |
| tool call | Three distinct candidates — see open question below | — | **Ambiguous; needs a decision, not just threading.** |
| cell | `WorkbenchExecutionId(String)` (opaque `"exec-<32 hex>"`, from a 16-byte digest) | `tidepool-runtime/src/session/workbench.rs:212-230`; minted (non-test) at `tidepool-actor/src/resident_tools.rs:412` | **Yes, but `Option`.** `request.execution_id().cloned()` at `resident_actor.rs:4576` (`execute_workbench`) and `6058` (`workbench` `KernelBehavior` method). It is `None` on some paths (`workbench.rs:948` test: `cell.execution_id().is_none()`) — see open question. |
| input unit | `input_unit_index: usize` | Field of `WorkbenchOperationId` (`workbench.rs:253`); also `WorkbenchItemReceipt.index` (`workbench.rs:320`) | **Yes**, as the loop variable `index` in `resident_actor.rs:4832` (`while index < request.items.len()`), and as `unit.input_unit_index` inside `settle_fragment_effects` (`resident_actor.rs:4236-4242`, field of the `WorkbenchUnitExecution<'_>` argument). |
| effect | `effect_ordinal: usize` | Field of `WorkbenchOperationId` (`workbench.rs:254`) | **Yes**, minted per iteration at `resident_actor.rs:4267-4268` (`let ordinal = effect_ordinal; effect_ordinal += 1;`) inside `settle_fragment_effects`'s effect-boundary loop. |
| compile request | `compile_request` — `blake3::hash(cwd + worker_argv)[..8]` hex-encoded | `tidepool-extract-cmd/src/daemon.rs:548-551` (`compile_request_correlation`), computed **daemon-side only** | **No — not in scope on the client (host/runtime) side at all**, and not returned over the wire. See §5. |

### Open question: which "tool call" identifier is *the* tool-call level

Three distinct, non-interchangeable candidates exist in the codebase today, and the survey brief's background text ("provider call id") does not disambiguate them:

1. **`ToolInvocationContext { context_call_id, thread_id, turn_id, call_id, namespace }`** — `tidepool-tool/src/lib.rs:88-95`. In scope at `KernelBehavior::tool` (`resident_actor.rs:5931`, argument `invocation: tidepool_tool::ToolInvocation`, whose `.context: Option<ToolInvocationContext>` carries it). This is a *typed backend* actor-to-actor tool invocation (child actor calling a parent's exposed tool surface), not the interactive-agent's own MCP tool call.
2. **`WorkbenchRequest::tool_call()`** — used at `resident_actor.rs:4577-4588` and `4863-4870` inside `execute_workbench`; the value returned only exposes `.name`/`.arguments` in every call site read during this survey — no id field was found on it. This is the path that actually drives a *cell* dispatched as a tool call from the interactive coding agent (Shoal's normal case: the model calls `session_run`/`eval` as an MCP tool, which arrives here).
3. **`ConversationTurn.turn: String`** ("the provider's own turn identifier", `tidepool-model/src/lib.rs:277-293`) surfaced via `ProviderObservation.turn: Option<ProviderTurnObservation>` (`tidepool-model/src/lib.rs:243-252`) and referenced at `tidepool/src/actor_host.rs:2292` (`turn = %turn.turn`). This is read out of the interactive agent's own transcript/observation, not minted by Rust at dispatch time — it is the actual model-provider call id, but it arrives asynchronously relative to cell execution, not in the same call stack.

**Recommendation to resolve, not decided here**: candidate 2 (`tool_call()`'s `.name`, threaded with a locally-generated span id if no id exists on the type) is the natural in-band "tool call" span parent for the cell/input-unit/effect subtree, since it wraps `execute_workbench` directly. Candidate 3 (`ConversationTurn.turn`) is the right field to *record on* that span (or the actor span) when/if it becomes available, to join against the provider's own side. Candidate 1 is a different, narrower mechanism (actor-to-actor typed calls) and should get its own span only if that path is separately instrumented. This needs a decision before implementation — flagged, not guessed.

---

## 3. Instrumentation checklist

All are currently uninstrumented except the one noted `shoal_host` span. Each row: file:line, signature, async/spawn flag, what it gives the tree.

| # | Site | Signature (abridged) | Async? | Gives |
|---|---|---|---|---|
| 0 (done) | `tidepool/src/shoal.rs:911`, span opened `956-959` | `async fn run_host(options: &HostOptions) -> Result<(), _>` | async, `.instrument()` already used | **run** span (`"shoal_host"`, `run_id`) — already correct, no change needed. |
| 1 | `tidepool-actor/src/local_actor.rs:816` | `async fn handle(&self, myself, message: KernelMessage, state: &mut ...) -> Result<(), ActorProcessingErr>` (ractor `Actor::handle`) | async (ractor-invoked, not manually spawned) | **actor** span, entered once per inbound message; `actor = %state.context.identity` in scope at entry. Wrap the body: `async move { ... }.instrument(tracing::info_span!("actor_dispatch", actor = %state.context.identity))`. This subsumes Cast/Call/Tool/Workbench/Source arms (`1049-1115`+) as one span per dispatch — cheaper and simpler than instrumenting each `KernelBehavior` method separately. |
| 2 | `tidepool-actor/src/resident_actor.rs:5931` `fn tool<'a>(...) -> BoxFuture<'a, ...>` | `KernelBehavior::tool` | **async, `Box::pin(async move {...})`** — needs `.instrument()` around the async block *before* `Box::pin`, not a guard | **tool call** span (candidate 1, see open question), `invocation.context` fields if present. |
| 3 | `tidepool-actor/src/resident_actor.rs:4570` `async fn execute_workbench(&mut self, kernel, context, mut request: WorkbenchRequest) -> Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure>` | plain `async fn`, not boxed | **cell** span. `#[tracing::instrument(skip(self, kernel, context, request), fields(execution = request.execution_id().map(...)))]` works directly here — no manual `.instrument()` needed since it's not hand-boxed. Also the entry point called from `workbench<'a>` (`resident_actor.rs:6034`, itself boxed — instrument that one too per row 2's pattern if the outer `KernelBehavior::workbench` dispatch boundary itself needs to be visible, e.g. to capture the replay/lookup short-circuit at `6058-6089`). |
| 4 | `tidepool-actor/src/resident_actor.rs:4229` `async fn settle_fragment_effects(&mut self, kernel, context, workbench, fragment, outcome, unit: WorkbenchUnitExecution<'_>) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError>` | plain `async fn` | **input unit** span. `unit.input_unit_index`/`unit.total` already parameters — `#[tracing::instrument(skip(...), fields(input_unit_index = unit.input_unit_index, total = unit.total))]`. **Caveat**: this only covers effect settlement (from `4239` onward); it does not cover the dispatch call that produces the first `fragment`/`outcome` (`begin_tool`/`begin_prepared_cell_item`, `resident_actor.rs:4863-4898`, called from the `while` loop at `4832` *before* `settle_fragment_effects` is invoked). Open question: whether the input-unit span should instead be opened one level up, around the loop body at `4832-4930` in `execute_workbench`, to include that dispatch call too — that loop body currently interleaves sync setup with two `.await` points, so wrapping it means extracting the per-iteration body into its own `async fn` and `.instrument()`-ing it (see row 5's note on the anti-pattern). |
| 5 | Effect boundary, inside `settle_fragment_effects`'s loop, `resident_actor.rs:4239-4268`+ (ordinal minted at `4267-4268`) | loop body, not a function | **N/A directly** — **do not** enter a span with `.entered()` and hold the guard across the loop's two `.await` points (`workbench.settle_item(...).await` at `4247`, `self.environment.runner.capture_boundary(...).await` at `4257`); that is exactly the `clippy::await_holding_span_guard` anti-pattern the tracing docs warn against. Extract one effect-boundary iteration into its own `async fn` (e.g. `settle_one_effect_boundary`) and wrap *that* future with `.instrument(tracing::info_span!("effect", input_unit_index, effect_ordinal = ordinal, effect = %effect))` — same shape as row 2, not a bare guard. This is the one site in the whole tree most likely to be implemented wrong; call it out explicitly in review. |
| 6 | `tidepool-runtime/src/session/turn.rs:1877` `pub fn check_cell(req: CellCheckRequest<'_>) -> Result<CellCheck, CellCheckFailure>` | **sync** (`endpoint.execute(&cmd)` at `1896` blocks) | **compile request** (whole-cell check), child of **cell**. Called once per cell from `prepare_cell_in_session` (`tidepool-actor/src/resident_workbench.rs:6029`, via `prepare_cell` at `2143-2147`). Plain `tracing::info_span!(...).in_scope(\|\| ...)` or `.entered()` — no await inside, safe to guard. |
| 7 | `tidepool-runtime/src/session/turn.rs:2032`/`2077` `pub fn run_turn` / `run_turn_pinned` → `run_turn_with_pin` | **sync**, `endpoint.execute(&cmd)` at `2137` | **compile request** (per-item turn compile), child of **input unit**. Called from `compile_block_in_view` (`tidepool-actor/src/resident_workbench.rs:6252`, call sites `6370-6371`) — this is the per-input-unit compile that actually produces the `ReadyBlock` consumed by `begin_ready_block` (`resident_workbench.rs:2611`). Also called from `inspect_rendered_value` (`resident_workbench.rs:2961`, call at `2994`) for value-display compiles — a secondary path, lower priority. |
| 8 | `tidepool-extract-cmd/src/daemon.rs:436-501` (accept-loop body, one iteration per request) | **sync**, single-threaded worker loop | **compile request, daemon side.** Currently hand-timed (`started = Instant::now()` at `437`, `elapsed_ms` computed at `441` and logged as a field at `444-450`/`455-461`). Replace with `tracing::info_span!("compile_request", run_id, compile_request = %compile_request).in_scope(\|\| worker.request(&cwd, &worker_argv))` so duration comes from span close, matching the decided design; keep `compile_request_correlation` (`548-551`) as the id-minting call, unchanged. Also carries the worker-rotation fields already computed at `477-501` (`served`, `rotate_after`, `worker_rss`, `rss_ceiling_mb`) — see §8. |
| 9 | `tidepool-handlers/src/handlers/agent.rs:1142` (`SpawnRequest { .. }` construction) | context not fully surveyed here — locate enclosing `async fn` before instrumenting | **effect** span for the child-spawn effect specifically; `SpawnRequest.task: String` (`tidepool-agent/src/spawn.rs:202`) is the "child assignment text" content item named in the background. Not yet located precisely enough to cite an enclosing function signature — treat the exact wrap point as an implementation-time lookup, not guessed here. |

Nine concrete sites (rows 1-9; row 0 already done) rather than 10-20 — the tree has six levels plus compile-request children, and several levels collapse onto one function each. Padding this list with the ~40 other `tracing::*!` call sites already in `actor_host.rs`/`resident_actor.rs` (enumerated during the survey but not level-bearing) would not add tree structure, only noise; they are candidates for ordinary log *events* inside the spans above, not new span levels.

---

## 4. Fields to record on close

`tidepool-runtime/src/session/workbench.rs:257-269` and `:292-299`:

```rust
pub enum WorkbenchOperationDisposition { Prepared, Committed, Rejected, Unknown }  // 257-269
pub enum WorkbenchItemStatus { Committed, Stopped, Diagnostic, Rejected, NotRun }  // 292-299
```

Neither derives `Display` (only `Debug, Clone[, Copy], PartialEq, Eq, Serialize, JsonSchema`). Recording them as tracing fields needs either `?status`/`?disposition` (Debug — fine for the JSON layer, renders as a quoted Rust-Debug string, e.g. `"Committed"`) or a small `as_str(&self) -> &'static str` added to each for a cleaner JSON value — not present today, a one-line addition if wanted.

**Byte sizes**: `WorkbenchItemReceipt.output: String` (`workbench.rs:330`) — `.len()` at span close for the input-unit span. `WorkbenchOperationReceipt` (`workbench.rs:272-280`) carries `effect: String` (name only, not a payload) and `disposition`, no separate byte count — if a byte size is wanted per effect, it has to be measured at the call site (e.g. serialized receipt/query size), not read off an existing field.

**Failure layer**: no existing enum named "failure layer" was found. The closest concept is `ResidentActorWorkbenchError`'s variants (referenced at `resident_actor.rs:511-538`, `disposition_for_non_command_failure`) which classify failures into `Delivered(_)` (→ `Committed`) vs. everything else (→ `Unknown`) — this is a disposition classifier, not a labeled "layer" field. If "failure layer" means "which crate/boundary produced the failure" (actor vs. workbench vs. compile vs. effect handler), no such field exists yet; it would have to be synthesized at each span's `record()`/close call from which `Err` arm was hit — **open question**, not found as an existing type.

---

## 5. Cross-process join (host log ↔ compiler log)

**Already correlated today, at the run level.** The compiler daemon is spawned once per Shoal run, as its own tmux window, with the *same* `run_id` string as the host:
- Host launch: `tidepool/src/shoal.rs:579-580` (`"--run-id".into(), run_id.clone()`), inside the args built at `573-591` for `Command::Host`.
- Compiler daemon launch: `tidepool/src/shoal.rs:706-707` (`"--run-id".into(), run_id.into()`), inside `compiler_daemon_launch` (`690-714`).
- Daemon reads it back at `tidepool-extract-cmd/src/frontend.rs:217` (`DaemonConfig.run_id: Option<String>`) and `daemon.rs:348` (`let run_id = config.run_id.as_deref().unwrap_or("standalone");`), then stamps every compiler-daemon log line with it (`daemon.rs:398,405,413,438,439,445,456,482,543`).

So a JSON layer added to *both* processes, filtered on `run_id`, already joins at the run granularity with zero new plumbing.

**Not correlated today: per-request.** `compile_request_correlation(cwd, worker_argv)` (`daemon.rs:548-551`) is computed **inside the daemon**, from data the daemon already has (it decoded `cwd`/`argv` off the wire at `daemon.rs:402-417`). The wire response back to the client is only `i32-LE(exit_code) frame(stdout) frame(stderr)` (module doc, `daemon.rs:14`, and `write_response` at `daemon.rs:475`) — **the digest is never returned to the caller**, so a client-side compile-request span (rows 6/7 in §3) cannot cite the same id the daemon logs unless one of:
  - (a) the wire protocol is extended to echo the digest back, or
  - (b) the client independently recomputes the same digest, since `compile_request_correlation`/`encode_request` are pure functions of `(cwd, worker_argv)` which the client already has before sending (`tidepool-extract-cmd/src/daemon.rs:265`, `encode_request`, is `pub(crate)` — visible within the `tidepool_extract_cmd` crate, so `lib.rs`'s client path *could* call `compile_request_correlation` directly if that function's visibility were widened from private to `pub(crate)` — it is currently a bare `fn`, not exported at all, even within the crate).

Recommend (b): make `compile_request_correlation` `pub(crate)`, call it from the client side (wherever `ExtractCmd`'s request-building happens, before `endpoint.execute(&cmd)`) to get the exact same digest, and use it as the `compile_request` field on the client-side span (rows 6/7). This requires no wire-format change and reuses the existing hash. **Not yet verified**: whether the client-side call site has access to the exact same `worker_argv` shape the daemon normalizes at `daemon.rs:410` (`normalize_worker_argv`) — if client and daemon build the argv frame differently before it's normalized, the two hashes could still diverge. Flagged as an implementation-time check, not assumed.

---

## 6. Privacy constraint that must keep holding

`tidepool/src/actor_host.rs:5769-5790` (test, inside `dispatch_haskell_script`-driven update-presentation flow):

```rust
let subscriber = tracing_subscriber::fmt()
    .with_ansi(false)
    .with_writer(std::sync::Mutex::new(log.reopen().unwrap()))
    .finish();
let error = tidepool_agent::UpdatePresentationError::NotSubmitted(
    "connecting update proxy: controlled transport failure".into(),
);
tracing::subscriber::with_default(subscriber, || {
    presentation.not_presented(error.to_string())
});
let logged = std::fs::read_to_string(log.path()).unwrap();
for expected in [
    "request update not presented",
    "actor=ActorRef",
    "request=RequestId",
    "update=1",
    &key,
    "connecting update proxy",
] {
    assert!(logged.contains(expected), "missing {expected}: {logged}");
}
assert!(!logged.contains("Private baseline clarification"));
```

The asserted-absent string (`"Private baseline clarification"`) is the *user-authored clarification answer text* passed to `updateRequest` earlier in the same test (`actor_host.rs:5748`) — i.e., request-update payload content must never reach any log, ever, regardless of the content-logging default.

**What keeps this true when content logging is on-by-default**: this string never flows through the span/field machinery described in §2-§3 at all — it is scoped to a completely different subsystem (`tidepool_agent::UpdatePresentationError` / the request-update proxy path), and the test's `error.to_string()` (the only thing actually logged, via `tracing::error!`/`warn!` somewhere inside `not_presented`) intentionally does not embed the raw answer text — only the *failure reason* ("controlled transport failure"). The new JSON content layer changes **what gets logged**, not **what data reaches the logging call sites**: this specific string is kept out by the *caller* (whatever code path builds the log message for `not_presented`) never passing it in, not by any target/filter arrangement downstream. Concretely: the JSON layer must not blanket-capture *all* fields on *all* spans/events without checking what call sites feed it — the design should keep this boundary the same way it's kept today (the request-update proxy failure path constructs its own bounded error string, and that's what's public), and the new instrumentation sites in §3 (cell/input-unit/effect/compile-request) are a *different* code path from this one and do not touch request-update payload content. **No target/filter change is needed to preserve this test** — it passes or fails based on whether `not_presented`'s call site is changed to include the answer text, which is out of scope for this work. Flag this explicitly to whoever implements §3: do not thread `WorkbenchRequest`'s raw cell text or tool arguments through the request-update proxy's error path, only through the cell/effect spans it's actually scoped to.

---

## 7. Test approach

`tidepool-extract-cmd/src/daemon.rs:919-947` (`CapturedWriter`/`CapturedGuard`, implementing `tracing_subscriber::fmt::MakeWriter<'writer>`) captures raw bytes written by a `fmt::layer()` into an in-memory `Arc<Mutex<Vec<u8>>>`, read back via `.text()` (`943-947`). Existing daemon tests (`950+`) use it only for non-tracing socket-lifecycle assertions in this excerpt, but the type is generic over any `fmt::layer()`.

**To assert on span fields instead of message text**: point a `.json()`-formatted `fmt::layer()` at a `CapturedWriter`, run the code under `tracing::subscriber::with_default(...)` (same pattern as `actor_host.rs:5776`), then parse `.text()` line-by-line as JSON (`tracing_subscriber`'s JSON formatter emits one JSON object per line, with a `"span"`/`"spans"` object holding the current span's fields and a `"fields"` object for the event). Assert `parsed["span"]["input_unit_index"] == 0`, etc., rather than `logged.contains("input_unit_index=0")` — immune to formatting/reordering changes. A `CapturedWriter` clone (it's `#[derive(Clone, Default)]`, `daemon.rs:919`, backed by one shared `Arc`) can be reused across a whole test to collect every line, then filtered by `"target"` or `"span"]["name"]`.

**Tests worth writing** (4-6, per the survey ask):

1. **Span nesting shape** — run one cell with two input units and one effect boundary each; assert the JSON lines' `"spans"` array shows `run → actor → cell → input_unit → effect` in the right parent order (proves the `.instrument()` wiring in §3 rows 1-5 actually nests, not just co-occurs).
2. **`run_id`/`compile_request` join** — run a compile through `check_cell`/`run_turn` (§3 rows 6-7) with a `CapturedWriter` on both the client-side and (via `daemon.rs`'s own test harness) daemon-side subscriber; assert the same `compile_request` value appears on both sides once §5's recommendation (b) is implemented. This is the one test that would catch the divergence risk flagged in §5.
3. **Disposition/status field values close correctly** — drive one committed and one rejected input unit; assert the input-unit span's closed fields include `status: "Committed"` / `status: "Rejected"` (exercises `WorkbenchItemStatus`, §4) via Debug-string or `as_str()` depending on which is chosen.
4. **Privacy regression** — port the exact assertion at `actor_host.rs:5780-5790` to also run under the new JSON layer (not just `tracing_subscriber::fmt()`), confirming content-logging-on-by-default does not resurrect the private string through a different layer than the one the existing test already covers.
5. **Worker-rotation field presence** — drive the daemon past `rotate_after` (`daemon.rs:58`, default 256; a test can pass a small override) and assert the "replacing compiler worker" span/event (§3 row 8, §8) carries `served`, `rotate_after`, `worker_rss_mb`, `rss_ceiling_mb` as structured fields, and that the *next* `compile_request` span's duration is measured from span close (not the removed `Instant::now()` hand timer).
6. **Guard-drop / appender flush** — a smoke test that `init_host_tracing`'s returned `WorkerGuard` (§1) is not dropped before the last write: write a span, drop the guard, and assert the `.jsonl` file on disk contains that span's line (catches the `let _ = ...` mistake specifically, since it would pass in-process but lose the write).

---

## 8. The two measurements: which spans/fields isolate each

**First two small cells took ~75s.** The span tree isolates this by elimination: with rows 0-7 wired, a trace of those two cells shows, per cell, the **cell** span's total duration (row 3) versus the sum of its child **compile-request** spans (rows 6-7, whole-cell check + per-item turn compiles) and **effect** spans (row 5). If the compile-request children account for most of the 75s, the cause is GHC/extract-cmd latency (cross-check against §5's `compile_request` join to see if it lines up with a cold daemon or worker rotation, §3 row 8). If the effect spans dominate instead (e.g. a `lookup` tool call or an external command effect), the cause is elsewhere in the effect handler, not compilation. If neither accounts for the bulk of 75s, the gap is in the **actor**-level span (row 1) minus the sum of its **cell**-span children — i.e., time spent in the interactive agent itself (candidate 3 from §2, `ConversationTurn.turn`, if/when threaded) or in actor-dispatch overhead not yet spanned. The tree as designed is sufficient to distinguish these three buckets; it does not, on its own, explain *which* sub-cause inside "compile" (cold cache vs. daemon queueing vs. GHC itself) without also looking at the `tidepool-timing`/`tidepool-compile-summary` lines already parsed at `log_compile_timing` (`daemon.rs:534-546`) — those remain plain debug-level log lines under this design, not span fields, unless promoted (open question: worth turning into child spans of row 8's `compile_request` span, not decided here).

**26.5s compile immediately after a worker rotation at 256 requests (RSS 3693MB / 6144MB ceiling).** Directly explained by wiring row 8 as designed: the daemon already computes and logs (as of `daemon.rs:477-501`) `served`, `rotate_after`, `worker_rss_mb`, `rss_ceiling_mb` at the moment it decides to replace the worker — turning `"replacing compiler worker"` into a span event (or a field set on the *next* `compile_request` span, e.g. `followed_rotation: true`) lets a query directly correlate "this compile_request's span duration is an outlier" with "the immediately preceding request triggered a rotation." The rotation replaces the worker and, per the comment at `daemon.rs:479-480`, discards its module memo — so the very next compile recompiles every library module cold, which is the expected mechanism for a 26.5s outlier immediately after rotation. **Sufficient as designed**, provided the "followed a rotation" fact is attached to the next request's span (not just logged as a separate prior event that a human has to line up by timestamp) — recommend adding that boolean/counter field explicitly rather than relying on event ordering in the `.jsonl` file.

---

## Open questions (not guessed)

1. **Which "tool call" identifier is the span-tree level** — three non-interchangeable candidates exist (§2); needs a decision before rows 2/3 in §3 are implemented as a coherent parent/child pair.
2. **`WorkbenchExecutionId` is `Option`** — some workbench calls have no cell execution id (`workbench.rs:948`). The cell span needs a fallback identity (synthetic counter? omit the span entirely and let input-unit spans attach directly to the tool-call span?) for that case.
3. **"Failure layer" field (§4)** — no existing type carries this concept; would need to be synthesized per span from which error arm fired, not read off `ResidentActorWorkbenchError` or the disposition enums directly.
4. **Effect byte sizes (§4)** — no existing field on `WorkbenchOperationReceipt`; would need to be measured at each effect call site if wanted, not read off existing data.
5. **`tidepool-handlers/src/handlers/agent.rs:1142`'s enclosing function** — not identified precisely enough during this survey to cite a signature for the "child assignment" effect span (§3 row 9); needs a direct look at that file before implementation.
6. **Client/daemon argv-hash parity (§5)** — recommendation (b) (client recomputes `compile_request_correlation` itself) assumes the client can reconstruct the exact same `worker_argv` bytes the daemon normalizes at `daemon.rs:410`; not verified byte-for-byte in this survey.
7. **Whether `tidepool-timing`/`tidepool-compile-summary`/`tidepool-memo-miss` lines (`daemon.rs:534-546`) should become child spans/fields of the `compile_request` span** rather than remaining plain debug log lines — would sharpen §8's first measurement but is additional scope beyond the decided design.

## Resolutions

Decisions on the open questions above, made after the survey. Where a question is
answered here, implement the answer rather than re-opening it.

**1. Tool-call identity is the provider's call id, and the span opens in the host
tool layer.** The join that matters is to the model provider's own transcript,
which keys every tool call by an id such as `call_vTAtgwrR7TAGootopFBJMJ9F`; that
id is what let us recover the two failing cells of the 2026-09-17 run from the
rollout at all. `WorkbenchToolCall` (`workbench.rs:144-147`) carries only `name`
and `arguments` — payload, not identity — so it is not the answer. The provider
id lives as `call_id` in `tidepool/src/host_dynamic_tools.rs` (see `call_id` and
`context_call_id`, around lines 356-370 and 497), which is also where the existing
dispatch-failure event already logs it. Open the tool-call span there, carrying
`call_id`, `context_call_id` and the tool name. `ConversationTurn.turn` is the
coarser turn level and may be a field on that span, never its identity.

**2. No cell execution id means no cell span.** Let the input-unit spans attach
directly to the tool-call span. A synthetic counter would be a second identifier
issuer for something that already has one, which the repository rules forbid, and
it would join to nothing.

**3. The failure layer needs its type built first.** The survey is right that
nothing carries the concept. It is a planned Wave 1 item that was not built: the
receipt-attribution fix landed, but the compile / effect / observation distinction
as data did not. Build the enum on the item receipt first, then have the span read
it. Do NOT synthesize it per span from which error arm fired — that would make the
trace and the receipt two independent opinions about the same event, which is the
correlated-wrongness failure we are trying to avoid elsewhere.

**4. Effect byte sizes are optional; drop them from the first cut.** They are not
on `WorkbenchOperationReceipt` and measuring at each call site is scope we have not
justified. The sizes that matter today are already reachable: cell output length
and the observation budget's own accounting.

**6 and 7. Leave both.** Recomputing the compile-request hash client-side is
unverified and run-level correlation already works, which is enough to answer the
two measurements in §8. Turning the daemon's timing lines into spans is real scope
for a later pass.

**5 is a lookup, not a decision** — read the file during implementation.

### One finding worth carrying forward

The survey found compilation happens twice per cell at different granularities: a
whole-cell check, then one per input unit. That explains the 2026-09-17 run's 233
compiles against 33 cells without anything being pathological, and it means any
future claim about compile cost must say which granularity it is talking about.
