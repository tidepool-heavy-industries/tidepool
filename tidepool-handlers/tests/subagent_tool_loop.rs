//! codex-live — the WHOLE parent-serves-the-child's-tools loop, on the
//! real extract/JIT.
//!
//! A Haskell program calls `spawnAgentWithTools` (`Tidepool.Agent.Spawn`) with
//! an authored `Tidepool.Agent.Contract` tools record. That record is compiled
//! ONCE into declarations + dispatch, the declarations reach the backend at
//! thread start, and every call the child makes is answered by the PARENT's own
//! Haskell handler — running in the parent's `M`, performing real parent
//! effects, while the child's turn is parked on the far side of the seam.
//!
//! What each gate here catches is stated on the gate. The load-bearing claim of
//! the file is gate `call_answer_from_the_parent_handler_reaches_the_child`:
//! the reply body the backend receives is what the Haskell handler COMPUTED
//! from that call's own arguments — asserted from the backend's recorded
//! replies, never from the handler's own return value.
//!
//! ## Why this is a standalone driver
//!
//! Same shape, same row, and the same real-temporary-git-repository substrate
//! as `subagent_one_cycle.rs` (which owns the no-tools vertical): it compiles
//! real Haskell through the real extract, builds a real `JitEffectMachine`, and
//! drives it on the PARKED path directly (`run_suspendable_parked`). No program
//! here ever suspends — the parent is never suspended while a tool handler
//! runs; each `agentBeginRaw`/`agentResumeRaw` is an ordinary synchronous
//! effect call that returns normally — so a suspension is a hard failure.
//!
//! ## No live model, ever
//!
//! Every gate wires [`MockBackend`], scripted with
//! [`MockStep`]s: arrange-step input ("given exactly these stops, the loop does
//! X"), not a simulation of a model. [`RecordingBackend`] wraps it to observe
//! the `ThreadSpec` and the `ToolReply`s the loop actually produced —
//! `MockBackend` already records both in its own `started`/`replies` fields,
//! but those are unreachable once the backend is boxed inside
//! `SubagentHandler`, exactly as `subagent_one_cycle.rs`'s `RecordingBackend`
//! mirrors `cycles`.
//!
//! ## One compile, six gates
//!
//! `PROGRAM_TOOLS` is byte-identical across every gate below except
//! `a_tools_compile_error_returns_before_anything_is_spawned` (which uses
//! `PROGRAM_BAD_TOOLS` — a genuinely different, deliberately non-compiling
//! source, and stays its own standalone `#[test]`/spawn). Per
//! `plans/test-time-cut.md` §3/§6 item 1, the first PROGRAM_TOOLS gate compiles
//! it ONCE and hands the `CompiledProgram` to the rest via
//! `Session::from_compiled` (own fixture, own backend, own JIT run per gate —
//! only the compile is shared). Because nextest runs one process per `#[test]`
//! fn, the six PROGRAM_TOOLS gates run inside ONE `#[test]` fn
//! (`subagent_tool_loop_family`); each gate's body still runs to completion and
//! is reported by name via `catch_unwind`, so one gate's assertion failure
//! never hides whether the others still pass.
//!
//! Needs `TIDEPOOL_EXTRACT` (a built `tidepool-extract-bin`) + GHC on PATH;
//! fails loudly otherwise via [`require_ghc`], never skips-as-pass.

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use tidepool_agent::backend::mock::{MockBackend, MockStep};
use tidepool_agent::backend::AgentBackend;
use tidepool_agent::seam::{
    AgentBackendError, BackendThreadId, CycleResultPayload, CycleSpec, ThreadSpec, ToolOutcome,
    ToolReply, TurnEvent,
};
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_codegen::suspension::{ParkedOutcome, RealmId, SuspensionRun};
use tidepool_effect::dispatch::{EffectContext, EffectHandler};
use tidepool_effect::error::EffectError;
use tidepool_effect::EffectRunPolicy;
use tidepool_effect::Response;
use tidepool_eval::value::Value as JitValue;
use tidepool_handlers::{ConsoleHandler, SubagentHandler};
use tidepool_mcp::{CapturedOutput, DescribeEffect, EffectDecl};
use tidepool_repr::{CoreExpr, DataConTable};
use tidepool_worktree::testing::TestRepo;
use tidepool_worktree::{GitCli, WorktreeManager, WorktreeRegistry};

// ============================================================================
// The row
// ============================================================================

/// The `Worktree` row entry, present only so the generated `Tidepool.Effects`
/// carries `WorktreeSpec`/`WorktreeHandle`/`WorktreeError` — `Subagent`'s own
/// types are written against them. It handles nothing: no program here calls
/// a worktree verb (the worktree work happens INSIDE the Rust saga), so
/// reaching this handler would mean a test started exercising a lane it does
/// not own — copied from `subagent_one_cycle.rs`'s identical row entry.
struct UnwiredWorktreeRow;

impl DescribeEffect for UnwiredWorktreeRow {
    fn effect_decl() -> EffectDecl {
        tidepool_mcp::worktree_decl()
    }
}

impl EffectHandler<CapturedOutput> for UnwiredWorktreeRow {
    type Request = JitValue;

    fn handle(
        &mut self,
        _req: JitValue,
        _cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<Response, EffectError> {
        panic!(
            "a Worktree verb was dispatched: this harness owns the Subagent effect only, \
             and the worktree work happens inside the Rust saga, not through the resident's \
             own effect dispatch"
        );
    }
}

type Stack = frunk::HList!(ConsoleHandler, UnwiredWorktreeRow, SubagentHandler);

// ============================================================================
// The backend under test: MockBackend, wrapped to observe the seam traffic
// ============================================================================

/// What a gate reads back after the run: the thread specs the backend was
/// asked to start (which is where the compiled tool DECLARATIONS land) and
/// every reply the parent's loop sent (which is where the compiled tool
/// ANSWERS land). Both mirror `MockBackend`'s own `started`/`replies` fields,
/// which are unreachable once the backend is boxed inside `SubagentHandler`.
#[derive(Clone, Default)]
struct BackendLog {
    started: Arc<Mutex<Vec<ThreadSpec>>>,
    replies: Arc<Mutex<Vec<ToolReply>>>,
}

impl BackendLog {
    fn started(&self) -> Vec<ThreadSpec> {
        self.started.lock().clone()
    }

    fn replies(&self) -> Vec<ToolReply> {
        self.replies.lock().clone()
    }
}

struct RecordingBackend {
    inner: MockBackend,
    log: BackendLog,
}

impl AgentBackend for RecordingBackend {
    fn start_thread(&mut self, spec: &ThreadSpec) -> Result<BackendThreadId, AgentBackendError> {
        self.log.started.lock().push(spec.clone());
        self.inner.start_thread(spec)
    }

    fn start_turn(
        &mut self,
        thread: &BackendThreadId,
        spec: &CycleSpec,
    ) -> Result<TurnEvent, AgentBackendError> {
        self.inner.start_turn(thread, spec)
    }

    fn resume(&mut self, reply: ToolReply) -> Result<TurnEvent, AgentBackendError> {
        self.log.replies.lock().push(reply.clone());
        self.inner.resume(reply)
    }
}

// ============================================================================
// The driver
// ============================================================================

/// FAIL LOUDLY when the environment cannot run these gates — see
/// `subagent_one_cycle.rs`'s identical guard for the incident this exists to
/// prevent (a skip spelled as a pass).
fn require_ghc() {
    assert!(
        ghc_available(),
        "TIDEPOOL_EXTRACT is unset or GHC is not on PATH. These gates drive a real \
         extract + JIT against a real temporary git repository and can verify nothing \
         without them — failing loudly rather than passing vacuously. Run through \
         scripts/battery.sh, which derives TIDEPOOL_EXTRACT automatically, or set it \
         to a built tidepool-extract-bin."
    );
}

fn ghc_available() -> bool {
    if std::env::var("TIDEPOOL_EXTRACT").is_err() {
        let bin = repo_root().join("haskell").join("tidepool-extract");
        if bin.exists() {
            std::env::set_var("TIDEPOOL_EXTRACT", &bin);
        }
    }
    std::process::Command::new("ghc")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// The extra imports every program here needs: the typed spawn wrappers, the
/// endpoint algebra the tools record is written in, and the schema class its
/// input/result types derive. Everything else (`spawnSpec`, `SpawnError`,
/// `fromCurrentRepository`, `renderSpawnError`, …) comes from the generated
/// `Tidepool.Effects`, auto-imported by the preamble.
const IMPORTS: &str = "Tidepool.Agent.Spawn\nTidepool.Agent.Contract\nTidepool.Aeson.Schema\n";

/// The authored surface under test, declared HERE rather than shipped by the
/// stdlib: a tools record is an ordinary user type, and so is the result type.
///
/// `WorkerTools` carries one `Call` (whose answer is computed FROM the call's
/// own argument, so a recorded reply proves the handler ran on that argument)
/// and one `Notify` (whose output is `()`). Both handlers perform a REAL parent
/// effect — `send (Print …)` — which is what proves a parent effect can run
/// while the child's turn is parked.
///
/// `BadTools` exists for the compile-error gate: `askParent` and `ask_parent`
/// normalize to the same wire name, which `compileTools` refuses.
const HELPERS: &str = r#"data Question = Question { questionText :: Text }
  deriving (Show, Eq, Generic, FromJSON, ToJSON, JsonSchema)

data Answer = Answer { approved :: Bool, note :: Text }
  deriving (Show, Eq, Generic, ToJSON)

data Progress = Progress { progressNote :: Text }
  deriving (Show, Eq, Generic, FromJSON, JsonSchema)

data WorkerResult = Completed { summary :: Text, caveats :: [Text] }
                  | Blocked { blocker :: Text, evidence :: [Text] }
  deriving (Show, Eq, Generic, FromJSON, JsonSchema)

data WorkerTools mode = WorkerTools
  { askParent :: mode :- Call Question Answer
  , reportProgress :: mode :- Notify Progress
  }
  deriving (Generic)

workerTools :: WorkerTools (AsServerT M)
workerTools = WorkerTools
  { askParent = tool "Ask the resident to resolve a question." $ \q -> do
      send (Print ("parent handled: " <> questionText q))
      pure (Answer (T.length (questionText q) > 5) ("seen: " <> questionText q))
  , reportProgress = notify "Report progress to the resident." $ \p -> do
      send (Print ("parent noted: " <> progressNote p))
      pure ()
  }

data BadTools mode = BadTools
  { askParent :: mode :- Call Question Answer
  , ask_parent :: mode :- Call Question Answer
  }
  deriving (Generic)

badTools :: BadTools (AsServerT M)
badTools = BadTools
  { askParent = tool "Ask the resident." $ \_ -> pure (Answer True "camel")
  , ask_parent = tool "Ask the resident, again." $ \_ -> pure (Answer True "snake")
  }
"#;

/// Core + constructor table for a compiled program, shared across every gate
/// that runs the SAME Haskell source — see the module doc's "One compile,
/// six gates".
struct CompiledProgram {
    expr: CoreExpr,
    table: DataConTable,
}

/// A compiled program plus the machine and handler stack driving it on the
/// parked path — `subagent_one_cycle.rs`'s `Session`, plus an accessor for the
/// console output a parent handler produced mid-loop.
struct Session {
    machine: JitEffectMachine,
    table: DataConTable,
    stack: Stack,
    captured: CapturedOutput,
}

/// The declarations derived from `Stack`; identical for any handler value
/// with this fixed stack type.
fn stack_decls(stack: &Stack) -> Vec<EffectDecl> {
    tidepool_handlers::base_decls(stack)
}

impl Session {
    /// Compile `code` (a bare statement sequence, wrapped in `do` by
    /// `wrap_do`) against a row containing `SubagentHandler`, and build a
    /// session machine for it — ONE `tidepool-extract` spawn. Returns the
    /// compiled program alongside so further gates driving the SAME `code`
    /// can build their own machine via [`Session::from_compiled`] without a
    /// second spawn.
    fn compile(code: &str, handler: SubagentHandler) -> (Self, CompiledProgram) {
        let stack: Stack = frunk::hlist![ConsoleHandler, UnwiredWorktreeRow, handler];
        let decls = stack_decls(&stack);

        let preamble = tidepool_mcp::build_preamble(&decls, false);
        let row = tidepool_mcp::build_effect_stack_type(&decls);
        let source = tidepool_mcp::template_haskell(
            &preamble,
            &row,
            &tidepool_mcp::wrap_do(code),
            IMPORTS,
            HELPERS,
            None,
            None,
        );
        let effects_dir = tidepool_mcp::ensure_effects_module(&decls).expect("effects module");
        let prelude = repo_root().join("haskell").join("lib");
        let include: Vec<&Path> = vec![
            prelude.as_path(),
            effects_dir.core.as_path(),
            effects_dir.shim.as_path(),
        ];

        let compiled = tidepool_runtime::compile_haskell(&source, "result", &include)
            .unwrap_or_else(|e| panic!("compiling the acceptance program failed: {e}"));
        let mut table = compiled.table;
        table.populate_siblings_from_expr(&compiled.expr);
        let machine = JitEffectMachine::compile_session(&compiled.expr, &table, 1 << 20)
            .expect("compile_session");

        let program = CompiledProgram {
            expr: compiled.expr,
            table: table.clone(),
        };
        (
            Self {
                machine,
                table,
                stack,
                captured: CapturedOutput::new(),
            },
            program,
        )
    }

    /// As [`Session::compile`], but reuses an already-compiled program — NO
    /// new `tidepool-extract` spawn.
    fn from_compiled(program: &CompiledProgram, handler: SubagentHandler) -> Self {
        let stack: Stack = frunk::hlist![ConsoleHandler, UnwiredWorktreeRow, handler];
        let _decls = stack_decls(&stack);

        let mut table = program.table.clone();
        table.populate_siblings_from_expr(&program.expr);
        let machine = JitEffectMachine::compile_session(&program.expr, &table, 1 << 20)
            .expect("compile_session");

        Self {
            machine,
            table,
            stack,
            captured: CapturedOutput::new(),
        }
    }

    /// Run to completion and return the program's final `Value`, rendered to
    /// JSON. Panics (naming the request) on an unexpected suspension, and on a
    /// failed turn — an `error` inside a tool dispatch WOULD surface here as a
    /// failed turn, which is exactly what the refusal gates exist to keep from
    /// happening.
    fn run(&mut self) -> serde_json::Value {
        let run = SuspensionRun::main(&self.table, EffectRunPolicy::HandleOrSuspend, RealmId(0));
        let outcome = self
            .machine
            .run_until_suspension(run, &mut self.stack, &self.captured);
        match outcome {
            Ok(
                ParkedOutcome::CompletedValue(value)
                | ParkedOutcome::CompletedBinding { value, .. },
            ) => tidepool_runtime::value_to_json(&value, &self.table, 0),
            Ok(ParkedOutcome::CompletedProject { .. } | ParkedOutcome::CompletedRender { .. }) => {
                unreachable!(
                    "this harness parks only ParkKind::Plain turns — Project/Render \
                     completions cannot be produced for them"
                )
            }
            Ok(ParkedOutcome::Suspended { request, .. }) => panic!(
                "the acceptance program suspended unexpectedly on {:?} — no program in this \
                 file should ever suspend",
                tidepool_runtime::value_to_json(&request, &self.table, 0)
            ),
            Err(e) => panic!("the acceptance program's turn failed: {e}"),
        }
    }

    /// Everything the parent's own handlers printed, in order.
    fn console_lines(&self) -> Vec<String> {
        self.captured.snapshot()
    }
}

/// Run `f` on a thread with room for the JIT's own stack usage — same
/// discipline as `subagent_one_cycle.rs`'s `in_test_thread`.
fn in_test_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

// ============================================================================
// Fixture: a real temp source repository + sibling substrate roots
// ============================================================================

struct Fixture {
    repo: TestRepo,
    roots: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let repo = TestRepo::init().expect("git init the source repository");
        repo.writer()
            .commit_file("README.md", "source\n", "initial commit")
            .expect("seed the source repository with a real commit");
        Self {
            repo,
            roots: tempfile::TempDir::new().expect("create the substrate roots"),
        }
    }

    fn registry_root(&self) -> PathBuf {
        self.roots.path().join("registry")
    }

    fn worktree_root(&self) -> PathBuf {
        self.roots.path().join("worktrees")
    }

    fn binding_root(&self) -> PathBuf {
        self.roots.path().join("bindings")
    }

    fn handler(&self, backend: Box<dyn AgentBackend + Send>) -> SubagentHandler {
        SubagentHandler::new(
            self.registry_root(),
            self.worktree_root(),
            self.binding_root(),
            self.repo.path().to_path_buf(),
            backend,
        )
        .expect("open the subagent handler over the temp substrate")
    }

    /// Reopen a fresh `WorktreeManager` over this fixture's roots — only sound
    /// once the whole session (and the handler's flocked `BindingTable`) has
    /// been dropped.
    fn reopen_manager(&self) -> WorktreeManager {
        let registry =
            WorktreeRegistry::open(self.registry_root()).expect("reopen the registry from disk");
        WorktreeManager::new(
            GitCli::new(),
            registry,
            self.worktree_root(),
            self.repo.path().to_path_buf(),
        )
    }

    /// Every persisted binding-history file (`<worktree-id>.json`). Empty means
    /// no binding was ever taken — what "nothing was allocated" has to mean on
    /// disk. `BindingTable::open` writes its own `.owner.lock` when the HANDLER
    /// is constructed, long before any program runs, so it is not evidence
    /// either way and is excluded.
    fn persisted_binding_files(&self) -> Vec<PathBuf> {
        match std::fs::read_dir(self.binding_root()) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// Build the fixture + a scripted recording backend, and hand back the session
/// and the log a gate asserts on — compiling `program` fresh (its own spawn).
fn session_with(script: Vec<MockStep>, program: &str) -> (Fixture, Session, BackendLog) {
    let fx = Fixture::new();
    let log = BackendLog::default();
    let backend = RecordingBackend {
        inner: MockBackend::scripted(script),
        log: log.clone(),
    };
    let handler = fx.handler(Box::new(backend));
    let (session, _program) = Session::compile(program, handler);
    (fx, session, log)
}

/// As [`session_with`], but reuses an already-compiled program — NO new
/// `tidepool-extract` spawn.
fn session_from_compiled(
    script: Vec<MockStep>,
    program: &CompiledProgram,
) -> (Fixture, Session, BackendLog) {
    let fx = Fixture::new();
    let log = BackendLog::default();
    let backend = RecordingBackend {
        inner: MockBackend::scripted(script),
        log: log.clone(),
    };
    let handler = fx.handler(Box::new(backend));
    let session = Session::from_compiled(program, handler);
    (fx, session, log)
}

// ============================================================================
// Haskell programs
// ============================================================================

/// `spawnAgentWithTools @WorkerTools @WorkerResult` at a round cap of 2 — the
/// cap is what gate `past_the_round_cap…` drives past, and two dispatched
/// rounds is what gate `call_answer…` needs, so ONE program serves both (and
/// so one compiled artifact serves every gate below but the compile-error one).
const PROGRAM_TOOLS: &str = r#"let wspec = fromCurrentRepository "reviewer"
result <- spawnAgentWithTools @WorkerTools @WorkerResult (ToolRounds 2) workerTools (spawnSpec wspec "reviewer" "summarize the diff")
case result of
  Right (outcome, Completed s cs) ->
    pure (object
      [ "case" .= ("completed" :: Text)
      , "summary" .= s
      , "caveats" .= cs
      , "rounds" .= outcome.outcomeReceipt.receiptRounds
      ])
  Right (_, Blocked b _) -> error ("expected Completed, got Blocked: " <> b)
  Left err -> pure (object [ "case" .= ("error" :: Text), "detail" .= renderSpawnError err ])
"#;

/// The same call against a tools record that does not compile. Every branch
/// except the expected typed failure `error`s, so an unexpected one is a loud
/// turn failure rather than a JSON shape the assertions would have to notice
/// is wrong.
const PROGRAM_BAD_TOOLS: &str = r#"let wspec = fromCurrentRepository "reviewer"
result <- spawnAgentWithTools @BadTools @WorkerResult (ToolRounds 2) badTools (spawnSpec wspec "reviewer" "summarize the diff")
case result of
  Left (SpawnDriveFailed StageAllocating detail) ->
    pure (object [ "case" .= ("compile_error" :: Text), "detail" .= detail ])
  Left other -> error ("expected a tools compile error, got: " <> renderSpawnError other)
  Right _ -> error "a tools record that does not compile must never spawn"
"#;

/// The terminal payload every completing script ends with.
fn completed_payload() -> CycleResultPayload {
    CycleResultPayload::Structured(serde_json::json!({
        "tag": "Completed",
        "summary": "done",
        "caveats": ["a"],
    }))
}

fn calls(tool: &str, arguments: serde_json::Value) -> MockStep {
    MockStep::Calls {
        tool: tool.to_string(),
        arguments,
    }
}

fn ask(question: &str) -> MockStep {
    calls(
        "ask_parent",
        serde_json::json!({ "questionText": question }),
    )
}

/// The reply bodies, in order, with refusals distinguished — the whole point of
/// asserting on `replies` rather than on the program's own return value.
fn outcomes(log: &BackendLog) -> Vec<ToolOutcome> {
    log.replies().into_iter().map(|r| r.outcome).collect()
}

/// `jsonSchema (Proxy :: Proxy Question)` / `Proxy Progress` — the schema of
/// the SAME generic encoding `compileTools`' dispatch decodes the argument
/// with. Pinned so a drift in either half is a failure here rather than a
/// mystery at the live leg.
fn pinned_question_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": { "questionText": { "type": "string" } },
        "required": ["questionText"]
    })
}

fn pinned_progress_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": { "progressNote": { "type": "string" } },
        "required": ["progressNote"]
    })
}

/// Run `body` (a gate) and collect its name into `failures` on panic, instead
/// of aborting the whole family — so every gate below still runs and reports
/// by name, the way six separate `#[test]` fns would.
fn run_gate(name: &str, failures: &mut Vec<String>, body: impl FnOnce()) {
    if let Err(e) = std::panic::catch_unwind(AssertUnwindSafe(body)) {
        let msg = e
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "panic (non-string payload)".to_string());
        failures.push(format!("{name}: {msg}"));
    }
}

// ============================================================================
// Gates a-e, g — one compile (`PROGRAM_TOOLS`), six independent runs
// ============================================================================

#[test]
fn subagent_tool_loop_family() {
    require_ghc();
    in_test_thread(|| {
        let mut failures: Vec<String> = Vec::new();

        // Gate a also performs the ONE compile every other PROGRAM_TOOLS gate
        // reuses.
        let fx_a = Fixture::new();
        let log_a = BackendLog::default();
        let backend_a = RecordingBackend {
            inner: MockBackend::scripted(vec![
                ask("should we ship this quarter?"),
                ask("no"),
                MockStep::Completes(completed_payload()),
            ]),
            log: log_a.clone(),
        };
        let handler_a = fx_a.handler(Box::new(backend_a));
        let (session_a, program) = Session::compile(PROGRAM_TOOLS, handler_a);

        run_gate(
            "call_answer_from_the_parent_handler_reaches_the_child",
            &mut failures,
            move || {
                let mut session = session_a;
                let out = session.run();
                assert_eq!(out["case"], "completed", "unexpected program branch: {out}");
                assert_eq!(out["summary"], "done");

                assert_eq!(
                    outcomes(&log_a),
                    vec![
                        ToolOutcome::Answered(serde_json::json!({
                            "approved": true,
                            "note": "seen: should we ship this quarter?",
                        })),
                        ToolOutcome::Answered(serde_json::json!({
                            "approved": false,
                            "note": "seen: no",
                        })),
                    ],
                    "each reply must be what the parent's Haskell handler computed from that \
                     call's own arguments"
                );
                assert_eq!(
                    out["rounds"], 2,
                    "the receipt counts the rounds the child actually took"
                );
            },
        );

        // Gate b — a Notify endpoint dispatches, and answers with the unit
        // encoding.
        run_gate(
            "notify_dispatches_and_answers_with_the_unit_encoding",
            &mut failures,
            || {
                let (_fx, mut session, log) = session_from_compiled(
                    vec![
                        calls(
                            "report_progress",
                            serde_json::json!({ "progressNote": "halfway" }),
                        ),
                        MockStep::Completes(completed_payload()),
                    ],
                    &program,
                );

                let out = session.run();
                assert_eq!(out["case"], "completed", "unexpected program branch: {out}");

                assert_eq!(
                    outcomes(&log),
                    vec![ToolOutcome::Answered(serde_json::Value::Null)],
                    "a Notify's reply is the unit encoding, and it is an ANSWER, not a refusal"
                );
                assert!(
                    session
                        .console_lines()
                        .contains(&"parent noted: halfway".to_string()),
                    "the Notify handler's own effect must have run: {:?}",
                    session.console_lines()
                );
            },
        );

        // Gate c — a parent handler's REAL effect runs while the child is
        // parked.
        run_gate(
            "parent_effect_runs_while_the_child_is_parked",
            &mut failures,
            || {
                let (_fx, mut session, _log) = session_from_compiled(
                    vec![
                        ask("does the parent still have effects?"),
                        MockStep::Completes(completed_payload()),
                    ],
                    &program,
                );

                let out = session.run();
                assert_eq!(out["case"], "completed", "unexpected program branch: {out}");

                assert_eq!(
                    session.console_lines(),
                    vec!["parent handled: does the parent still have effects?".to_string()],
                    "the parent's own Console effect must have run mid-loop, with the child's \
                 turn parked"
                );
            },
        );

        // Gate d — an undeclared tool name is REFUSED, and the loop continues.
        run_gate(
            "undeclared_tool_is_refused_and_the_loop_continues",
            &mut failures,
            || {
                let (_fx, mut session, log) = session_from_compiled(
                    vec![
                        calls("frobnicate", serde_json::json!({ "whatever": 1 })),
                        ask("still alive?"),
                        MockStep::Completes(completed_payload()),
                    ],
                    &program,
                );

                let out = session.run();
                assert_eq!(
                    out["case"], "completed",
                    "the turn must finish normally after a refusal: {out}"
                );

                assert_eq!(
                    outcomes(&log),
                    vec![
                        ToolOutcome::Refused("no such tool: frobnicate".to_string()),
                        ToolOutcome::Answered(serde_json::json!({
                            "approved": true,
                            "note": "seen: still alive?",
                        })),
                    ],
                    "an undeclared tool is refused (never dropped, never an abort) and the next \
                     declared call still dispatches"
                );
            },
        );

        // Gate e — past the ToolRounds cap, calls are refused and the turn
        // completes.
        run_gate(
            "past_the_round_cap_calls_are_refused_and_the_turn_still_completes",
            &mut failures,
            || {
                let (_fx, mut session, log) = session_from_compiled(
                    vec![
                        ask("first, within budget"),
                        ask("second, within budget"),
                        ask("third, over budget"),
                        MockStep::Completes(completed_payload()),
                    ],
                    &program,
                );

                let out = session.run();
                assert_eq!(
                    out["case"], "completed",
                    "past the cap the child finishes its turn normally: {out}"
                );

                assert_eq!(
                    outcomes(&log),
                    vec![
                        ToolOutcome::Answered(serde_json::json!({
                            "approved": true,
                            "note": "seen: first, within budget",
                        })),
                        ToolOutcome::Answered(serde_json::json!({
                            "approved": true,
                            "note": "seen: second, within budget",
                        })),
                        ToolOutcome::Refused("tool-call round cap reached (2)".to_string()),
                    ],
                    "the third call is refused with text naming the cap, not dispatched"
                );
                assert_eq!(
                    session.console_lines().len(),
                    2,
                    "the refused round must not have run a handler: {:?}",
                    session.console_lines()
                );
            },
        );

        // Gate g — the declarations reaching the backend are the compiled
        // ones.
        run_gate(
            "declared_tools_reach_the_backend_with_wire_names_and_schemas",
            &mut failures,
            || {
                let (_fx, mut session, log) =
                    session_from_compiled(vec![MockStep::Completes(completed_payload())], &program);

                let out = session.run();
                assert_eq!(out["case"], "completed", "unexpected program branch: {out}");

                let started = log.started();
                assert_eq!(started.len(), 1, "one thread per spawn: {started:?}");
                let spec = &started[0];
                assert!(spec.ephemeral, "a coupled spawn's thread is ephemeral");

                let declared: Vec<(&str, &str, &serde_json::Value)> = spec
                    .dynamic_tools
                    .iter()
                    .map(|d| (d.name.as_str(), d.description.as_str(), &d.input_schema))
                    .collect();
                assert_eq!(
                    declared,
                    vec![
                        (
                            "ask_parent",
                            "Ask the resident to resolve a question.",
                            &pinned_question_schema()
                        ),
                        (
                            "report_progress",
                            "Report progress to the resident.",
                            &pinned_progress_schema()
                        ),
                    ],
                    "the declarations must be the compiled snake_case names, the authored \
                     descriptions, and the JsonSchema of each input type — in record-field order"
                );

                assert!(
                    outcomes(&log).is_empty(),
                    "a turn that made no calls answers none"
                );
            },
        );

        assert!(
            failures.is_empty(),
            "{} gate(s) failed:\n{}",
            failures.len(),
            failures.join("\n")
        );
    });
}

// ============================================================================
// Gate f — a tools compile error returns BEFORE anything is spawned
// ============================================================================

/// A `ToolCompileError` is a typed `SpawnError` returned before
/// `agentBeginRaw` is ever called: nothing allocated, nothing bound, no
/// process started. Proven from three independent places — the typed error the
/// program matched, the backend's (empty) thread log, and disk.
#[test]
fn a_tools_compile_error_returns_before_anything_is_spawned() {
    require_ghc();
    in_test_thread(|| {
        // An empty script: reaching the backend at all would exhaust it and
        // fail loudly, which is a second way this gate catches a spawn that
        // should never have happened.
        let (fx, mut session, log) = session_with(Vec::new(), PROGRAM_BAD_TOOLS);

        let out = session.run();
        assert_eq!(
            out["case"], "compile_error",
            "unexpected program branch: {out}"
        );
        assert_eq!(
            out["detail"],
            "BadTools: selectors askParent, ask_parent all normalize to the wire name \
             \"ask_parent\". Rename one of the selectors so their snake_case forms differ.",
            "the typed failure carries renderToolCompileError's own text, naming the \
             record, both selectors, and the fix"
        );

        assert!(
            log.started().is_empty(),
            "no thread may be started for a tools record that does not compile: {:?}",
            log.started()
        );
        assert!(
            fx.persisted_binding_files().is_empty(),
            "no binding may be taken: {:?}",
            fx.persisted_binding_files()
        );

        drop(session);
        assert!(
            fx.reopen_manager()
                .list()
                .expect("list the registry from disk")
                .is_empty(),
            "no worktree may be allocated for a spawn that never began"
        );
    });
}
