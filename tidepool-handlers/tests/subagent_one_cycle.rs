//! The WHOLE one-cycle coupled-spawn vertical, on the real
//! extract/JIT.
//!
//! A Haskell program calls the typed `spawnAgent` (`Tidepool.Agent.Spawn`,
//! which derives the worker's `outputSchema` from the caller's result type and
//! decodes the terminal payload with that type's ordinary `FromJSON`),
//! the Rust saga (`tidepool_agent::spawn::CoupledSpawner`) runs it against
//! [`MockBackend`] and a REAL temporary git repository, and the Haskell
//! caller gets back a `Generic`-decoded `WorkerResult` plus a receipt, or a
//! case-matchable typed `SpawnError` — with rollback proven from DISK state,
//! not from anything held in memory.
//!
//! ## Why this is a standalone driver
//!
//! Same shape as `repo_event_with_handler.rs`'s acceptance
//! harness: it compiles real Haskell through the real extract, builds a
//! real `JitEffectMachine`, and drives it on the PARKED path directly
//! (`run_suspendable_parked`) — not through the resident session or the
//! harness engine. None of these programs ever suspends (`Subagent`'s one
//! verb answers with `cx.respond`, never a park), so [`Session::run`] treats
//! a suspension as a hard failure rather than something to resume.
//!
//! ## The row
//!
//! `frunk::HList!(ConsoleHandler, UnwiredWorktreeRow, SubagentHandler)`.
//! `Subagent`'s generated types reference `WorktreeSpec`/`WorktreeHandle`/
//! `WorktreeError` from `worktree_effect_def!`'s `type_defs`, so the row
//! needs `Worktree` present — but no program here ever calls a `Worktree`
//! verb (worktree work happens INSIDE the Rust saga, not through the
//! resident's own effect dispatch), so [`UnwiredWorktreeRow`] — copied from
//! `repo_event_with_handler.rs` — stands in: it declares the effect and
//! panics if anything ever dispatches through it.
//!
//! ## No live model, ever
//!
//! Every test wires [`MockBackend`] — no live-model turns in tests or
//! automated code. [`RecordingBackend`] wraps it to observe the
//! `CycleSpec` the saga actually hands the backend, without adding an
//! accessor to `SubagentHandler` or `MockBackend` for it.
//!
//! ## One compile, five gates
//!
//! Every gate below drives `PROGRAM_SPAWN_AND_REPORT` — byte-identical
//! Haskell source across all five — against a DIFFERENT backend/fixture. Per
//! `plans/test-time-cut.md` §3/§6 item 1, `compile_gate1_and_program` compiles
//! it ONCE (one `tidepool-extract` spawn) and hands the `CompiledProgram` back
//! so `Session::from_compiled` can build four more independent machines from
//! it — own fixture, own backend, own JIT run per gate, only the compile is
//! shared. Because nextest runs one process per `#[test]` fn, sharing a
//! compile across gates means the gates run inside ONE `#[test]` fn
//! (`subagent_one_cycle_family`); each gate's body still runs to completion
//! and is reported by name via `catch_unwind`, so one gate's assertion
//! failure never hides whether the others still pass — the same visibility
//! five separate `#[test]` fns gave, one spawn instead of five.
//!
//! Needs `TIDEPOOL_EXTRACT` (a built `tidepool-extract-bin`) + GHC on PATH;
//! fails loudly otherwise via [`require_ghc`], never skips-as-pass.

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use tidepool_agent::backend::mock::{MockBackend, MockFailure};
use tidepool_agent::backend::AgentBackend;
use tidepool_agent::seam::{
    AgentBackendError, BackendThreadId, CycleResultPayload, CycleSpec, ThreadSpec, ToolReply,
    TurnEvent,
};
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_codegen::suspension::{ParkedOutcome, RealmId, SuspensionRun};
use tidepool_effect::dispatch::{EffectContext, EffectHandler};
use tidepool_effect::error::EffectError;
use tidepool_effect::EffectRunPolicy;
use tidepool_effect::Response;
use tidepool_handlers::{ConsoleHandler, SubagentHandler, WorktreeReq};
use tidepool_mcp::{CapturedOutput, DescribeEffect, EffectDecl};
use tidepool_repr::{CoreExpr, DataConTable};
use tidepool_worktree::testing::TestRepo;
use tidepool_worktree::{
    Binding, BindingState, BindingTable, GitCli, WorktreeId, WorktreeManager, WorktreeRegistry,
};

// ============================================================================
// The row
// ============================================================================

/// The `Worktree` row entry, present only so the generated `Tidepool.Effects`
/// carries `WorktreeSpec`/`WorktreeHandle`/`WorktreeError` — `Subagent`'s own
/// types are written against them. It handles nothing: no program here calls
/// a worktree verb (the worktree work happens INSIDE the Rust saga), so
/// reaching this handler would mean a test started exercising a lane it does
/// not own — copied from `repo_event_with_handler.rs`'s identical row entry.
struct UnwiredWorktreeRow;

impl DescribeEffect for UnwiredWorktreeRow {
    fn effect_decl() -> EffectDecl {
        tidepool_mcp::worktree_decl()
    }
}

impl EffectHandler<CapturedOutput> for UnwiredWorktreeRow {
    type Request = WorktreeReq;

    fn handle(
        &mut self,
        _req: WorktreeReq,
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
// The backend under test: MockBackend, wrapped to observe cycles
// ============================================================================

/// Wraps [`MockBackend`] to record every `CycleSpec` the saga hands the
/// backend, in an `Arc<Mutex<..>>` a test can inspect after the run —
/// `MockBackend` already records this in its own `cycles` field, but that
/// field is unreachable once the backend is boxed inside `SubagentHandler`.
/// This is the "simplest" option named in the spec: no new accessor on
/// `SubagentHandler` or `MockBackend`, just a backend that shares its own log.
struct RecordingBackend {
    inner: MockBackend,
    log: Arc<Mutex<Vec<CycleSpec>>>,
}

impl AgentBackend for RecordingBackend {
    fn start_thread(&mut self, spec: &ThreadSpec) -> Result<BackendThreadId, AgentBackendError> {
        self.inner.start_thread(spec)
    }

    fn start_turn(
        &mut self,
        thread: &BackendThreadId,
        spec: &CycleSpec,
    ) -> Result<TurnEvent, AgentBackendError> {
        self.log.lock().push(spec.clone());
        self.inner.start_turn(thread, spec)
    }

    fn resume(&mut self, reply: ToolReply) -> Result<TurnEvent, AgentBackendError> {
        self.inner.resume(reply)
    }
}

// ============================================================================
// The driver
// ============================================================================

/// FAIL LOUDLY when the environment cannot run these gates — see
/// `repo_event_with_handler.rs`'s identical guard for the incident this
/// exists to prevent (a skip spelled as a pass).
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

/// The extra imports every program here needs: the typed wrapper, and the
/// schema class its result type derives. Everything else (`spawnSpec`,
/// `SpawnError`, `fromCurrentRepository`, `renderSpawnError`, …) comes from
/// the generated `Tidepool.Effects`, auto-imported by the preamble.
const IMPORTS: &str = "Tidepool.Agent.Spawn\nTidepool.Aeson.Schema\n";

/// The caller's result type, declared HERE rather than shipped by the stdlib:
/// `spawnAgent @r` needs nothing of `r` but `Generic`-derived `FromJSON` (to
/// read the terminal payload) and `JsonSchema` (to describe it to the worker),
/// so an acceptance fixture is an ordinary user type, not a library one. Its
/// two constructors exercise the tag discriminator, named record fields, and a
/// list leaf in one shape.
const HELPERS: &str = "data WorkerResult = Completed { summary :: Text, caveats :: [Text] }\n\
                       \x20                 | Blocked { blocker :: Text, evidence :: [Text] }\n\
                       \x20 deriving (Show, Eq, Generic, FromJSON, JsonSchema)\n";

/// Core + constructor table for a compiled program, shared across every gate
/// that runs the SAME Haskell source — see the module doc's "One compile,
/// five gates".
struct CompiledProgram {
    expr: CoreExpr,
    table: DataConTable,
}

/// A compiled program plus the machine and handler stack driving it on the
/// parked path — `repo_event_with_handler.rs`'s `Session`, minus ask/answer:
/// no program here ever suspends, so a suspension is a hard failure rather
/// than something to resume.
struct Session {
    machine: JitEffectMachine,
    table: DataConTable,
    stack: Stack,
    captured: CapturedOutput,
}

/// The declarations derived from `Stack`; identical for any handler value
/// with this fixed stack type.
fn stack_decls(stack: &Stack) -> Vec<tidepool_mcp::EffectDecl> {
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
    /// new `tidepool-extract` spawn — for a gate whose Haskell source is
    /// byte-identical to a prior gate's (own machine, own heap, own handler
    /// stack throughout — only the compile is shared).
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
    /// JSON. Panics (naming the request) on an unexpected suspension, and on
    /// a failed turn — every program here is written to complete via `pure`
    /// on every branch it exercises, so either is a bug in the program or in
    /// what is under test, not a case to handle quietly.
    fn run(&mut self) -> serde_json::Value {
        let run = SuspensionRun::main(&self.table, EffectRunPolicy::HandleOrSuspend, RealmId(0));
        let outcome = self
            .machine
            .run_until_suspension(run, &mut self.stack, &self.captured);
        match outcome {
            Ok(ParkedOutcome::CompletedProject { .. } | ParkedOutcome::CompletedRender { .. }) => {
                unreachable!(
                    "this harness parks only ParkKind::Plain turns - Project/Render \
                     completions cannot be produced for them"
                )
            }
            Ok(
                ParkedOutcome::CompletedValue(value)
                | ParkedOutcome::CompletedBinding { value, .. },
            ) => tidepool_runtime::value_to_json(&value, &self.table, 0),
            Ok(ParkedOutcome::Suspended { request, .. }) => panic!(
                "the acceptance program suspended unexpectedly on {:?} — no program in this \
                 file should ever suspend",
                tidepool_runtime::value_to_json(&request, &self.table, 0)
            ),
            Err(e) => panic!("the acceptance program's turn failed: {e}"),
        }
    }

    /// Drop the machine and hand back the `SubagentHandler`, so a test can
    /// inspect its substrate (registry, bindings) after the run without
    /// reopening flocked state.
    fn into_subagent_handler(self) -> SubagentHandler {
        let Session { machine, stack, .. } = self;
        drop(machine); // cycle-scoped: the realm never outlives its machine
        let frunk::hlist_pat![_console, _worktree, handler] = stack;
        handler
    }
}

/// Run `f` on a thread with room for the JIT's own stack usage — same
/// discipline as `repo_event_with_handler.rs`'s `in_test_thread`, generalized
/// to return a value so a whole gate (compile + run + drop) can run on it.
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

    /// Reopen a fresh `WorktreeManager` over this fixture's roots — used by
    /// the rollback gate AFTER the whole session (and the handler's own
    /// flocked `BindingTable`) has been dropped.
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

    /// Reopen a fresh `BindingTable` over this fixture's binding root — only
    /// sound once every prior owner (the session's `SubagentHandler`) has
    /// been dropped, since `BindingTable::open` refuses a second live owner
    /// of the same root.
    fn reopen_bindings(&self) -> BindingTable {
        BindingTable::open(self.binding_root()).expect("reopen the binding table from disk")
    }

    /// The full persisted lease history for `id`, read straight off disk —
    /// `BindingTable::current` only ever answers with an `Active` binding, so
    /// distinguishing "settled Terminal" from "settled Released" (both leave
    /// `current` empty) needs the raw file, not the table's own API. Plain
    /// file reads never contend with the table's exclusive owner lock.
    fn binding_history(&self, id: &WorktreeId) -> Vec<Binding> {
        let path = self.binding_root().join(format!("{}.json", id.as_str()));
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("reading binding history at {path:?}: {e}"));
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|e| panic!("parsing binding history at {path:?}: {e}"))
    }
}

// ============================================================================
// Haskell programs shared by the gates below
// ============================================================================

/// `spawnAgent @WorkerResult`, matching every branch of its result and
/// rendering whichever one fired to a small JSON envelope the Rust side
/// asserts on. Every branch a given gate does not expect to hit calls
/// `error`, naming what actually happened — turning an unexpected branch into
/// a loud turn failure rather than a JSON shape the assertions would have to
/// notice is wrong.
const PROGRAM_SPAWN_AND_REPORT: &str = r#"let wspec = fromCurrentRepository "reviewer"
result <- spawnAgent @WorkerResult (spawnSpec wspec "reviewer" "summarize the diff")
case result of
  Right (outcome, Completed s cs) ->
    pure (object
      [ "case" .= ("completed" :: Text)
      , "summary" .= s
      , "caveats" .= cs
      , "model" .= outcome.outcomeReceipt.receiptModel
      , "worktree" .= (case outcome.outcomeReceipt.receiptWorktree of WorktreeId t -> t)
      , "bindingRef" .= outcome.outcomeReceipt.receiptBindingRef
      , "thread" .= (case outcome.outcomeReceipt.receiptThread of BackendThreadId t -> t)
      , "turn" .= outcome.outcomeReceipt.receiptTurn
      ])
  Right (_, Blocked b _) -> error ("expected Completed, got Blocked: " <> b)
  Left (SpawnBackendFailed StageThreadAccepted (BackendUnavailable detail)) ->
    pure (object [ "case" .= ("backend_error" :: Text), "detail" .= detail ])
  Left (SpawnResultMalformed msg) ->
    pure (object [ "case" .= ("malformed" :: Text), "message" .= msg ])
  Left other -> error ("unexpected spawn error: " <> renderSpawnError other)
"#;

/// `jsonSchema (Proxy :: Proxy WorkerResult)`'s exact rendering — aeson's
/// `TaggedObject` shape, which is what the `FromJSON` instance on the other
/// side of this same payload actually reads. `oneOf` order follows
/// constructor declaration order (`Completed` then `Blocked`); each
/// constructor's `required` array is `["tag", ...]` in the same order, since
/// JSON arrays (unlike the sorted `Object` map) preserve declaration order.
fn pinned_worker_result_schema() -> serde_json::Value {
    serde_json::json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "tag": { "type": "string", "enum": ["Completed"] },
                    "summary": { "type": "string" },
                    "caveats": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["tag", "summary", "caveats"]
            },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "tag": { "type": "string", "enum": ["Blocked"] },
                    "blocker": { "type": "string" },
                    "evidence": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["tag", "blocker", "evidence"]
            }
        ]
    })
}

/// Run `body` (a gate) and collect its name into `failures` on panic, instead
/// of aborting the whole family — so every gate below still runs and reports
/// by name, the way five separate `#[test]` fns would.
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
// Gates 1-5 — one compile (`PROGRAM_SPAWN_AND_REPORT`), five independent runs
// ============================================================================

#[test]
fn subagent_one_cycle_family() {
    require_ghc();
    in_test_thread(|| {
        let mut failures: Vec<String> = Vec::new();

        // Gate 1 also performs the ONE compile every other gate reuses.
        let fx1 = Fixture::new();
        let payload = serde_json::json!({
            "tag": "Completed",
            "summary": "done",
            "caveats": ["a", "b"],
        });
        let backend = MockBackend::completing(CycleResultPayload::Structured(payload));
        let handler = fx1.handler(Box::new(backend));
        let (session1, program) = Session::compile(PROGRAM_SPAWN_AND_REPORT, handler);

        run_gate(
            "typed_result_round_trips_through_the_real_jit",
            &mut failures,
            move || {
                let mut session1 = session1;
                let out = session1.run();

                assert_eq!(out["case"], "completed", "unexpected program branch: {out}");
                assert_eq!(out["summary"], "done");
                assert_eq!(out["caveats"], serde_json::json!(["a", "b"]));
                assert_eq!(
                    out["model"],
                    MockBackend::MODEL,
                    "the receipt must record the EXACT resolved model, never a tier name"
                );
                let worktree_str = out["worktree"].as_str().expect("worktree is a string");
                assert!(!worktree_str.is_empty());
                assert!(
                    !out["bindingRef"].as_str().unwrap().is_empty(),
                    "the binding ref is the string the binding was taken under"
                );
                assert!(
                    !out["thread"].as_str().unwrap().is_empty(),
                    "the backend thread id is the mock's own, echoed"
                );
                assert!(
                    !out["turn"].as_str().unwrap().is_empty(),
                    "the turn id is the backend's own, echoed"
                );

                let worktree_id = WorktreeId::from_raw(worktree_str);

                // Success settles the binding Terminal — distinct from rollback's
                // Released (finished vs stopped-waiting).
                let history = fx1.binding_history(&worktree_id);
                let last = history
                    .last()
                    .expect("at least one binding row for the worktree that was bound");
                assert_eq!(
                    last.state(),
                    BindingState::Terminal,
                    "a completed one-cycle agent settles Terminal, not Released or Active"
                );
                assert_eq!(last.agent().as_str(), out["bindingRef"].as_str().unwrap());

                // The worktree the run actually got is retained and present on disk —
                // read through the SAME still-open handler the run used (its
                // BindingTable/registry flock is still held; a fresh reopen here
                // would contend with it).
                let handler = session1.into_subagent_handler();
                let found = handler
                    .spawner()
                    .manager()
                    .lookup(&worktree_id)
                    .expect("lookup the worktree the run reported")
                    .expect("the worktree must be registered");
                assert!(
                    found.cwd().exists(),
                    "the worktree handle's cwd must exist on disk: {:?}",
                    found.cwd()
                );
                // Release the flock before the fixture drops at the end of scope.
                drop(handler);
            },
        );

        // Gate 2 — a backend failure surfaces as a typed SpawnError, with
        // rollback proven from DISK state.
        run_gate(
            "backend_failure_surfaces_as_typed_spawn_error_and_rolls_back",
            &mut failures,
            || {
                let fx = Fixture::new();
                let backend = MockBackend::failing(MockFailure::AtThreadStart(
                    AgentBackendError::BackendUnavailable {
                        detail: "codex app-server not running".to_string(),
                    },
                ));
                let handler = fx.handler(Box::new(backend));

                let mut session = Session::from_compiled(&program, handler);
                let out = session.run();

                assert_eq!(
                    out["case"], "backend_error",
                    "unexpected program branch: {out}"
                );
                assert_eq!(out["detail"], "codex app-server not running");

                // Drop the WHOLE session — including the handler's flocked
                // BindingTable — before reopening fresh state from disk.
                drop(session);

                let manager = fx.reopen_manager();
                let summaries = manager
                    .list()
                    .expect("list the registry after the whole session was dropped");
                assert_eq!(
                    summaries.len(),
                    1,
                    "exactly one worktree was ever created in this fixture"
                );
                let worktree_id = summaries[0].receipt.worktree_id.clone();

                let bindings = fx.reopen_bindings();
                assert!(
                    bindings.current(&worktree_id).is_none(),
                    "no Active binding must remain after a rolled-back spawn"
                );

                let found = manager
                    .lookup(&worktree_id)
                    .expect("lookup must not fail: the worktree is retained, never deleted")
                    .expect("the worktree must still be registered");
                assert!(
                    found.cwd().exists(),
                    "the worktree is RETAINED — rollback settles the binding, it never deletes: {:?}",
                    found.cwd()
                );

                let history = fx.binding_history(&worktree_id);
                assert_eq!(
                    history.last().expect("at least one binding row").state(),
                    BindingState::Released,
                    "a post-Bound failure settles Released — unbound and rebindable, never left Active"
                );
            },
        );

        // Gate 3 — a malformed structured payload is a typed decode failure,
        // never a success.
        run_gate(
            "malformed_payload_is_a_typed_decode_failure_never_a_success",
            &mut failures,
            || {
                let fx = Fixture::new();
                // Missing `caveats` — the schema `spawnAgent` derived from
                // `WorkerResult` requires it.
                let payload = serde_json::json!({ "tag": "Completed", "summary": "done" });
                let backend = MockBackend::completing(CycleResultPayload::Structured(payload));
                let handler = fx.handler(Box::new(backend));

                let mut session = Session::from_compiled(&program, handler);
                let out = session.run();

                assert_eq!(out["case"], "malformed", "unexpected program branch: {out}");
                assert_eq!(
                    out["message"], "key \"caveats\" not present",
                    "the plain vendored decode error, naming the missing field — there is no \
                     codec on this boundary and no path-carrying error machinery"
                );
            },
        );

        // Gate 4 — an unstructured (non-JSON) terminal message is typed
        // malformed too.
        run_gate(
            "unstructured_payload_is_typed_malformed",
            &mut failures,
            || {
                let fx = Fixture::new();
                let backend =
                    MockBackend::completing(CycleResultPayload::Unstructured("prose".to_string()));
                let handler = fx.handler(Box::new(backend));

                let mut session = Session::from_compiled(&program, handler);
                let out = session.run();

                assert_eq!(out["case"], "malformed", "unexpected program branch: {out}");
                assert_eq!(
                    out["message"], "terminal message was not JSON: prose",
                    "Spawn.hs's PayloadUnstructured branch names the message verbatim"
                );
            },
        );

        // Gate 5 — the schema that reaches the backend is the NAMED-FIELD
        // shape.
        run_gate(
            "schema_reaches_the_backend_named_field_shape",
            &mut failures,
            || {
                let fx = Fixture::new();
                let payload = serde_json::json!({
                    "tag": "Completed",
                    "summary": "done",
                    "caveats": ["a", "b"],
                });
                let log: Arc<Mutex<Vec<CycleSpec>>> = Arc::new(Mutex::new(Vec::new()));
                let backend = RecordingBackend {
                    inner: MockBackend::completing(CycleResultPayload::Structured(payload)),
                    log: log.clone(),
                };
                let handler = fx.handler(Box::new(backend));

                let mut session = Session::from_compiled(&program, handler);
                let out = session.run();
                assert_eq!(out["case"], "completed", "unexpected program branch: {out}");

                let cycles = log.lock();
                assert_eq!(
                    cycles.len(),
                    1,
                    "lane 1 is exactly one cycle: {:?}",
                    *cycles
                );
                let schema = cycles[0].output_schema.as_ref().expect(
                    "spawnAgent must derive an output_schema from WorkerResult, never None",
                );
                assert_eq!(
                    *schema,
                    pinned_worker_result_schema(),
                    "the schema reaching the backend must be the NAMED-FIELD shape the caller's \
                     own FromJSON reads — not a positional {{tag, fields:[..]}} shape"
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
