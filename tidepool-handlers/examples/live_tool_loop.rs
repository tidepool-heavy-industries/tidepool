//! THE LIVE ACCEPTANCE — a real Codex child, holding tools compiled from an
//! authored Haskell record, whose calls are answered by the parent's own
//! Haskell handlers.
//!
//! This is the one run in the codex-live lane that spends the operator's real
//! ChatGPT budget. It is an EXAMPLE, not a test: no runner can select it, no
//! battery tier can reach it, and `cargo nextest run` will never execute it. It
//! is double-gated (see [`preflight`]) and run by hand, once, deliberately.
//!
//! ```bash
//! TIDEPOOL_AGENT_LIVE=1 cargo run -p tidepool-handlers --example live_tool_loop
//! ```
//!
//! # What it proves that no mock can
//!
//! Every mechanism below is already mock-proven in the battery tier
//! (`tidepool-handlers/tests/subagent_tool_loop.rs` on the real extract/JIT,
//! and `tidepool-agent`'s replay gates on a recorded transcript). What a
//! scripted child cannot establish is that a REAL model, told only a JSON
//! Schema, *chooses* to call a declared tool, *reads* the parent's answer, and
//! *finishes* on it. Delegation is the thing under test, and delegation needs
//! a delegate.
//!
//! # Model policy is HARD
//!
//! `gpt-5.6-luna` at `ReasoningEffort::Low` — the human's 2026-08-11 grant, and
//! nothing else. [`ModelPolicy::CheapestGpt56`] is a ONE-ENTRY allowlist, so
//! this is enforced by construction rather than by a check that could be
//! skipped: no other slug is reachable, including the cheaper `gpt-5.4-mini`
//! (cheaper is not the same as granted) and including `gpt-5.6-terra`, which is
//! banned outright.
//!
//! # One attempt — and exactly where the free part ends
//!
//! "Retry freely before `turn/start`" is only actionable if you know which
//! failures land on which side of it. In this example, THREE of the four ways
//! the run can fail happen strictly before any turn is sent, and are therefore
//! free to fix and re-run as often as needed:
//!
//! 1. **The authored Haskell does not elaborate** — the extract/JIT compile
//!    below runs before the handler is ever touched.
//! 2. **The model is not on offer** — `start_turn` resolves the model
//!    (`model/list`) BEFORE it builds or sends `turn/start`, so a catalogue
//!    without `gpt-5.6-luna` aborts having spent nothing.
//! 3. **The declarations are refused** — `thread/start` carries the dynamic
//!    tools and completes before the turn exists, so a schema the server will
//!    not accept fails there.
//!
//! Only the fourth — the turn itself — costs anything. Once it starts there is
//! ONE attempt: capture the frame log, report, stop. Do not loop this on
//! failure.
//!
//! # Credentials
//!
//! None are handled here, deliberately. The `codex app-server` reads the
//! operator's own `~/.codex/auth.json` itself; Tidepool never reads, copies, or
//! logs it, and no credential is passed through any argument or environment
//! variable this file sets. That a normal run does not MUTATE that directory is
//! checked, not assumed — see the isolation report below.

use std::path::{Path, PathBuf};

use tidepool_agent::backend::codex::isolation::{self, ConfigSnapshot};
use tidepool_agent::backend::codex::CodexAgentBackend;
use tidepool_agent::seam::{ModelPolicy, ReasoningEffort};
use tidepool_codegen::jit_machine::{JitEffectMachine, ParkedOutcome, RealmId};
use tidepool_effect::dispatch::{EffectContext, EffectHandler};
use tidepool_effect::error::EffectError;
use tidepool_effect::Response;
use tidepool_eval::value::Value as JitValue;
use tidepool_handlers::{ConsoleHandler, SubagentHandler};
use tidepool_mcp::{CapturedOutput, DescribeEffect, EffectDecl};
use tidepool_worktree::testing::TestRepo;

/// Opt-in env var. Its ABSENCE is the default, so an accidental
/// `cargo run --example` spends nothing.
const OPT_IN: &str = "TIDEPOOL_AGENT_LIVE";

/// How many tool-call rounds the parent will serve. Small on purpose: this is
/// a budget, and the task needs two.
const ROUND_CAP: i64 = 4;

/// Where the run's own transcript lands, to become a replay fixture.
const TRANSCRIPT: &str = "tidepool-agent/fixtures/app-server-0.146.0/live-tool-loop.jsonl";

// ============================================================================
// The row — identical to the committed acceptance driver's
// ============================================================================

/// Present only so the generated `Tidepool.Effects` carries the Worktree types
/// `Subagent`'s own types are written against. It handles nothing: the worktree
/// work happens INSIDE the Rust saga, not through the resident's dispatch.
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
        panic!("a Worktree verb was dispatched: this harness owns the Subagent effect only");
    }
}

type Stack = frunk::HList!(ConsoleHandler, UnwiredWorktreeRow, SubagentHandler);

// ============================================================================
// The authored surface the CHILD sees
// ============================================================================

const IMPORTS: &str = "Tidepool.Agent.Spawn\nTidepool.Agent.Contract\nTidepool.Aeson.Schema\n";

/// THREE declared tools, and the third is the point of declaring three.
///
/// `askParent` and `reportProgress` are what the task needs. `readBudget` is
/// declared but NOT needed — so the transcript shows the model SELECTING among
/// tools rather than calling the only thing it was given, which is the
/// difference between evidence of delegation and evidence of a funnel.
///
/// Every handler performs a real parent effect (`send (Print …)`), so the
/// console output is proof the parent ran while the child's turn was parked.
const HELPERS: &str = r#"data Question = Question { questionText :: Text }
  deriving (Show, Eq, Generic, FromJSON, ToJSON, JsonSchema)

data Answer = Answer { passphrase :: Text }
  deriving (Show, Eq, Generic, ToJSON)

data Progress = Progress { progressNote :: Text }
  deriving (Show, Eq, Generic, FromJSON, JsonSchema)

data BudgetQuery = BudgetQuery { budgetTopic :: Text }
  deriving (Show, Eq, Generic, FromJSON, JsonSchema)

data Budget = Budget { remaining :: Int }
  deriving (Show, Eq, Generic, ToJSON)

-- A single-constructor RECORD, not a sum. Proven live on 2026-08-11: a
-- top-level sum renders `{"oneOf": [...]}` and the backend refuses the whole
-- turn at request validation with
-- `invalid_json_schema: In context=(), 'oneOf' is not permitted`.
-- See PROTOCOL-NOTES.md §5.
data WorkerResult = WorkerResult { summary :: Text, secret :: Text }
  deriving (Show, Eq, Generic, FromJSON, JsonSchema)

data WorkerTools mode = WorkerTools
  { askParent :: mode :- Call Question Answer
  , reportProgress :: mode :- Notify Progress
  , readBudget :: mode :- Call BudgetQuery Budget
  }
  deriving (Generic)

workerTools :: WorkerTools (AsServerT M)
workerTools = WorkerTools
  { askParent = tool "Ask the parent for information only the parent has, such as the passphrase." $ \q -> do
      send (Print ("[parent] askParent: " <> questionText q))
      pure (Answer "tidepool-cobalt-7-do-not-reuse")
  , reportProgress = notify "Tell the parent what you are doing. Call this once before you finish." $ \p -> do
      send (Print ("[parent] reportProgress: " <> progressNote p))
      pure ()
  , readBudget = tool "Ask the parent how much budget is left for a topic." $ \b -> do
      send (Print ("[parent] readBudget: " <> budgetTopic b))
      pure (Budget 0)
  }
"#;

/// The task. Deliberately blunt: `luna` at LOW effort is a weak model, and this
/// run tests the PLUMBING, not the model's ingenuity. It needs exactly one
/// `askParent` and one `reportProgress`, and it cannot be completed by guessing
/// — the passphrase exists only in the parent's handler.
///
/// `WorkerResult` is a single-constructor RECORD for a reason the first live
/// attempt found: see the note beside its declaration above.
const PROGRAM: &str = r#"let wspec = fromCurrentRepository "live-tool-loop"
let task = "You have tools. To finish you need a secret passphrase that ONLY the parent knows; you cannot guess it and it is not in any file. Call askParent to get it. Also call reportProgress exactly once to say what you are doing. Do not run commands and do not edit files. Then finish with your result."
result <- spawnAgentWithTools @WorkerTools @WorkerResult (ToolRounds 4) workerTools (spawnSpec wspec "live-tool-loop" task)
case result of
  Left err -> pure (object ["ok" .= False, "error" .= renderSpawnError err])
  Right (outcome, WorkerResult s sec) -> pure (object
    [ "ok" .= True
    , "summary" .= s
    , "secret" .= sec
    , "model" .= receiptModel (outcomeReceipt outcome)
    , "turn" .= receiptTurn (outcomeReceipt outcome)
    , "rounds" .= receiptRounds (outcomeReceipt outcome)
    , "worktree" .= show (receiptWorktree (outcomeReceipt outcome))
    , "binding" .= receiptBindingRef (outcomeReceipt outcome)
    , "activity" .= show (outcomeActivity outcome)
    , "usage" .= show (receiptUsage (outcomeReceipt outcome))
    ])
"#;

// ============================================================================
// Preflight — the run costs nothing until both gates pass
// ============================================================================

/// Both gates, checked before anything is built. Returns the reason to abort,
/// or `None` to proceed.
fn preflight() -> Option<String> {
    if std::env::var(OPT_IN).ok().as_deref() != Some("1") {
        return Some(format!(
            "{OPT_IN} is not set to 1. This example spends the operator's real ChatGPT \
             budget, so it does nothing unless asked explicitly."
        ));
    }
    let auth = isolation::codex_home().join("auth.json");
    if !auth.exists() {
        return Some(format!(
            "no Codex credential at {} — this example runs against the operator's own \
             ~/.codex on purpose and never copies credentials anywhere else.",
            auth.display()
        ));
    }
    None
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-handlers has a parent directory")
        .to_path_buf()
}

fn main() {
    if let Some(reason) = preflight() {
        eprintln!("SKIPPED: {reason}");
        std::process::exit(0);
    }
    // The JIT wants more stack than a default thread has.
    let handle = std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(run)
        .expect("spawn the runner thread");
    // A panicked runner is a failed run, not a silent success.
    let code = handle.join().unwrap_or(1);
    std::process::exit(code);
}

fn run() -> i32 {
    println!("=== codex-live: LIVE tool-dispatch acceptance ===");
    println!("model policy:  CheapestGpt56 (one-entry allowlist: gpt-5.6-luna)");
    println!("effort:        Low");
    println!("round cap:     {ROUND_CAP} (authored policy)");

    // 1. Isolation baseline. A run that succeeded but mutated the operator's
    //    Codex config is a FAILURE, so the evidence is captured before anything
    //    connects.
    let home = isolation::codex_home();
    let before = ConfigSnapshot::capture(&home).expect("capture the pre-run config snapshot");
    let config_before = std::fs::read_to_string(home.join("config.toml")).unwrap_or_default();

    // 2. A real temporary source repository, and substrate roots OUTSIDE it
    //    (never dirty the source).
    let repo = TestRepo::init().expect("git init the source repository");
    repo.writer()
        .commit_file("README.md", "live tool loop\n", "initial commit")
        .expect("seed the source repository");
    let roots = tempfile::TempDir::new().expect("substrate roots");

    let backend = match CodexAgentBackend::new() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("FAILED to build the backend: {e}");
            return 1;
        }
    };
    println!("turn timeout:  {:?}", backend.turn_timeout());

    let handler = SubagentHandler::new(
        roots.path().join("registry"),
        roots.path().join("worktrees"),
        roots.path().join("bindings"),
        repo.path().to_path_buf(),
        Box::new(backend),
    )
    .expect("open the subagent handler")
    .with_model_policy(ModelPolicy::CheapestGpt56, ReasoningEffort::Low);

    // 3. Compile the Haskell. Free — no turn has started, so a compile failure
    //    here costs nothing and may be fixed and retried freely.
    println!("\n--- compiling the authored program (free; retry as needed) ---");
    let stack: Stack = frunk::hlist![ConsoleHandler, UnwiredWorktreeRow, handler];
    let (decls, ask_tag) = tidepool_handlers::base_decls_with_ask(&stack);
    // DERIVED from the same value that built the stack — the parking contract's
    // rule, never restated at the park site.
    let handled_prefix: Vec<String> = decls[..ask_tag as usize]
        .iter()
        .map(|d| d.type_name.to_string())
        .collect();

    let preamble = tidepool_mcp::build_preamble(&decls, false);
    let row = tidepool_mcp::build_effect_stack_type(&decls);
    let source = tidepool_mcp::template_haskell(
        &preamble,
        &row,
        &tidepool_mcp::wrap_do(PROGRAM),
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

    let compiled = match tidepool_runtime::compile_haskell(&source, "result", &include) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FAILED to compile the program (no tokens spent): {e}");
            return 1;
        }
    };
    let mut table = compiled.table;
    table.populate_siblings_from_expr(&compiled.expr);
    let mut machine = JitEffectMachine::compile_session(&compiled.expr, &table, 1 << 20)
        .expect("compile_session");
    println!("compiled.");

    // 4. THE ONE LIVE RUN. Everything past this point may spend tokens.
    println!("\n--- running (ONE attempt; do not retry past this line) ---");
    let started = std::time::Instant::now();
    let captured = CapturedOutput::new();
    let mut stack = stack;
    let outcome = machine.run_suspendable_parked(
        &table,
        &mut stack,
        &captured,
        ask_tag,
        RealmId(0),
        &handled_prefix,
    );
    let elapsed = started.elapsed();

    // 5. Receipts FIRST, whatever happened — a failed run still has to leave
    //    its evidence behind, and a panic here would take the transcript with
    //    it.
    println!("\n--- parent handlers that ran while the child was parked ---");
    let lines = captured.snapshot();
    if lines.is_empty() {
        println!("(none — no parent handler ran)");
    }
    for line in &lines {
        println!("  {line}");
    }

    // The handler owns the backend, so the transcript is reached through the
    // stack the run just used.
    let frames = {
        let handler: &SubagentHandler = stack.get();
        handler.backend_transcript_jsonl()
    };
    let transcript = repo_root().join(TRANSCRIPT);
    match write_jsonl(&frames, &transcript) {
        Ok(()) => println!(
            "\nwrote {} frames to {} (this becomes the replay fixture)",
            frames.len(),
            transcript.display()
        ),
        Err(e) => eprintln!("\nWARNING: could not write the transcript: {e}"),
    }

    // Worktree resolution: what the coupled spawn actually LEFT BEHIND.
    //
    // Retain-first is locked (PRD 19), so "rolling back" never deletes
    // anything — the question a receipt has to answer is what STATE the
    // worktree and its binding were left in, and whether the child moved the
    // tree at all. A settled binding plus a retained, rebindable worktree is
    // the successful end state; the git head and dirtiness say whether there
    // is anything to roll forward.
    println!("\n--- worktree resolution ---");
    {
        let handler: &SubagentHandler = stack.get();
        let spawner = handler.spawner();
        match spawner.manager().list() {
            Ok(trees) if trees.is_empty() => println!("(no worktree was registered)"),
            Ok(trees) => {
                for summary in &trees {
                    let id = &summary.receipt.worktree_id;
                    // `current` reports only ACTIVE bindings, so ABSENCE here
                    // is the success condition, not a missing record: the
                    // binding was settled and the worktree is rebindable. An
                    // Active binding after a completed run is the orphan the
                    // saga exists to prevent, so it is spelled as the alarming
                    // case rather than as the neutral one.
                    let binding = spawner
                        .bindings()
                        .current(id)
                        .map(|b| {
                            format!(
                                "STILL ACTIVE ({:?}, agent {}) — settle did not happen",
                                b.state(),
                                b.agent().as_str()
                            )
                        })
                        .unwrap_or_else(|| {
                            "settled — no active binding remains, worktree rebindable".to_string()
                        });
                    println!("worktree:  {}", id.as_str());
                    println!("binding:   {binding}");
                    match spawner.manager().lookup(id) {
                        Ok(Some(handle)) => {
                            let cwd = handle.cwd();
                            println!("retained:  yes, at {}", cwd.display());
                            println!("head:      {}", git_line(cwd, &["rev-parse", "HEAD"]));
                            let dirty = git_line(cwd, &["status", "--porcelain"]);
                            if dirty.is_empty() {
                                println!("changes:   none — nothing to roll forward");
                            } else {
                                println!("changes:   the child modified the tree:\n{dirty}");
                            }
                        }
                        Ok(None) => println!("retained:  NO — the registry lost it"),
                        Err(e) => println!("retained:  lookup failed: {e}"),
                    }
                }
            }
            Err(e) => println!("(could not list the registry: {e})"),
        }
    }

    println!("\n--- isolation (a mutated ~/.codex is a FAILURE even on success) ---");
    let after = ConfigSnapshot::capture(&home).expect("capture the post-run config snapshot");
    let report = before.compare(&after);
    println!("{}", report.render());
    let config_after = std::fs::read_to_string(home.join("config.toml")).unwrap_or_default();
    let config_unchanged = config_before == config_after;
    println!("config.toml text unchanged: {config_unchanged}");

    println!("\n--- result ---");
    println!("wall clock: {elapsed:?}");
    let program_ok = match outcome {
        Ok(ParkedOutcome::CompletedProject { .. } | ParkedOutcome::CompletedRender { .. }) => {
            unreachable!(
                "this harness parks only ParkKind::Plain turns - Project/Render \
                     completions cannot be produced for them"
            )
        }
        Ok(
            ParkedOutcome::CompletedValue(value) | ParkedOutcome::CompletedBinding { value, .. },
        ) => {
            let json = tidepool_runtime::value_to_json(&value, &table, 0);
            println!(
                "{}",
                serde_json::to_string_pretty(&json).unwrap_or_default()
            );
            json.get("ok").and_then(|v| v.as_bool()).unwrap_or(false)
        }
        Ok(ParkedOutcome::Suspended { request, .. }) => {
            eprintln!(
                "the program SUSPENDED unexpectedly on {:?}",
                tidepool_runtime::value_to_json(&request, &table, 0)
            );
            false
        }
        Err(e) => {
            eprintln!("the turn FAILED: {e}");
            false
        }
    };

    let isolation_ok = report.passed() && config_unchanged;
    if !isolation_ok {
        eprintln!("\nISOLATION VIOLATED — this is a failure regardless of the turn's outcome.");
    }
    if program_ok && isolation_ok {
        println!("\nPASS");
        0
    } else {
        eprintln!("\nFAIL");
        1
    }
}

/// One line of `git` output from `cwd`, or empty. Read-only inspection for the
/// receipt — this example never writes to the child's tree.
fn git_line(cwd: &Path, args: &[&str]) -> String {
    std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// The backend's recorded lines, one per line — the format
/// [`replay`](tidepool_agent::backend::codex::replay) reads back.
fn write_jsonl(lines: &[String], path: &Path) -> std::io::Result<()> {
    use std::io::Write;
    let mut out = String::new();
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    std::fs::File::create(path)?.write_all(out.as_bytes())
}
