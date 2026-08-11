//! The lane-1 live leg: ONE real coupled spawn, ONE real model turn, with
//! receipts. **A human runs this deliberately** — it spends the operator's
//! ChatGPT tokens and is never wired into a suite or a battery tier (standing
//! rule, Inanna 2026-08-09).
//!
//!     cargo run -p tidepool-agent --example live_one_cycle
//!
//! Read `plans/post-restart/agent-lanes/lane1-live-leg.md` BEFORE running it:
//! preconditions, the model policy, which lines to paste back as evidence, the
//! stop-and-hold rule, and the ONE-attempt rule (a token-spending failure is
//! captured and reported, never retried in a loop).
//!
//! What it does, in order:
//!
//! 1. Snapshot the operator's Codex config surface (PRD 18 criterion 11).
//! 2. Build a throwaway git repository plus sibling registry / worktree /
//!    binding roots, all under one temp directory that is KEPT on disk so the
//!    run is checkable after the fact.
//! 3. Run one [`CoupledSpawner::spawn_one_cycle`] against the real
//!    [`CodexAgentBackend`] with a tiny synthetic task and a
//!    `{"result": string}` output schema — no dynamic tools (lane 1).
//! 4. Print the receipt (worktree id, binding ref, thread id, EXACT resolved
//!    model, turn id), the payload, and the binding's on-disk final state.
//! 5. Re-snapshot the config surface and compare, loudly.
//!
//! Exits nonzero on any failure, including an isolation regression. The
//! isolation compare runs even if the spawn panics — a run that spent tokens
//! and then blew up still owes the operator the config verdict, which is why
//! the spawn is caught rather than allowed to unwind out of `main`.

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use tidepool_agent::backend::codex::isolation::{self, ConfigSnapshot};
use tidepool_agent::backend::codex::CodexAgentBackend;
use tidepool_agent::seam::{CycleResultPayload, ModelPolicy, ReasoningEffort};
use tidepool_agent::{CoupledSpawner, SpawnRequest, SpawnWorkspace};
use tidepool_worktree::{GitCli, WorktreeManager, WorktreeRegistry, WorktreeSpec};

/// The word the worker is asked to produce. Distinctive enough that finding it
/// in the payload is evidence the turn actually ran, not that a default
/// matched — and it is NOT the phase-4 passphrase, so a stale transcript
/// cannot be mistaken for this run.
const EXPECTED_WORD: &str = "tidepool-lane1-halite";

fn main() -> ExitCode {
    match run() {
        Ok(()) => {
            println!("\nPASS: one-cycle live leg completed with receipts above.");
            ExitCode::SUCCESS
        }
        Err(failure) => {
            eprintln!("\nFAIL: {failure}");
            eprintln!(
                "ONE-ATTEMPT RULE: do not re-run this on a token-spending failure. \
                 Capture the output above and report it."
            );
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    // 1. Config isolation, before anything spawns.
    let codex_home = isolation::codex_home();
    let before = ConfigSnapshot::capture(&codex_home)
        .map_err(|e| format!("could not snapshot {}: {e}", codex_home.display()))?;
    println!("codex home under test: {}", codex_home.display());

    // 2. Throwaway substrate. Kept on disk deliberately: the binding table and
    //    the managed worktree ARE the receipt, and a receipt deleted on exit
    //    is not checkable.
    let scratch = tempfile::TempDir::new()
        .map_err(|e| format!("could not create the scratch directory: {e}"))?
        .keep();
    println!("scratch root (kept on disk): {}", scratch.display());

    let source = scratch.join("source");
    let registry_root = scratch.join("registry");
    let worktree_root = scratch.join("worktrees");
    let binding_root = scratch.join("bindings");
    init_source_repository(&source)?;

    let registry = WorktreeRegistry::open(&registry_root)
        .map_err(|e| format!("could not open the worktree registry: {e}"))?;
    let manager = WorktreeManager::new(GitCli::new(), registry, &worktree_root, &source);
    let mut spawner = CoupledSpawner::open(manager, &binding_root)
        .map_err(|e| format!("could not open the coupled spawner: {e}"))?;

    let mut backend =
        CodexAgentBackend::new().map_err(|e| format!("could not build the backend: {e}"))?;
    println!("turn timeout: {:?}", backend.turn_timeout());

    let request = SpawnRequest {
        workspace: SpawnWorkspace::New(WorktreeSpec::from_current_repository(
            "lane1-live-one-cycle",
        )),
        agent_label: "lane1-live".to_string(),
        // Blunt on purpose: the cheap-plumbing tier is a weaker model than the
        // one the phase-4 transcript was recorded on, and the point of this
        // task is to exercise the plumbing, not the model.
        task: format!(
            "Do not run any commands and do not edit any files. Respond with exactly \
             {{\"result\": \"{EXPECTED_WORD}\"}} and nothing else."
        ),
        output_schema: Some(serde_json::json!({
            "type": "object",
            "properties": {"result": {"type": "string"}},
            "required": ["result"],
            "additionalProperties": false
        })),
        // Lane 1's shape: no tools, so the child has nothing to call and the
        // no-tools combinator drives the turn straight through.
        tools: Vec::new(),
        model: ModelPolicy::CheapPlumbing,
        effort: ReasoningEffort::Low,
    };

    // 3. The one spawn. Caught rather than unwinding, so step 5 still runs.
    println!("\n--- spawning (one attempt, no retries) ---");
    let spawn_result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        spawner.spawn_one_cycle(&mut backend, &request)
    }));

    if let Err(e) = backend.shutdown() {
        eprintln!("WARNING: backend shutdown reported {e}");
    }

    // 4. Receipts, whatever happened.
    let outcome = match &spawn_result {
        Ok(Ok(run)) => {
            println!("\n--- receipt (every field checkable against disk or the backend) ---");
            println!("agent id:        {}", run.receipt.agent.0);
            println!("worktree id:     {}", run.receipt.worktree.as_str());
            println!("worktree cwd:    {}", run.run.worktree.cwd().display());
            println!("binding ref:     {}", run.receipt.binding_ref);
            println!("thread id:       {}", run.receipt.thread.0);
            println!("RESOLVED MODEL:  {}", run.receipt.resolved_model);
            println!("turn id:         {}", run.receipt.turn.0);

            println!("\n--- payload ---");
            match &run.payload {
                CycleResultPayload::Structured(value) => {
                    println!("Structured: {value}");
                    match value.get("result").and_then(|r| r.as_str()) {
                        Some(word) if word == EXPECTED_WORD => {
                            println!("result matches the requested word exactly");
                        }
                        Some(word) => println!(
                            "NOTE: result is {word:?}, not the requested {EXPECTED_WORD:?} — \
                             the plumbing worked; the model did not follow the instruction"
                        ),
                        None => println!(
                            "NOTE: payload has no string `result` field — schema-conforming \
                             output is the model's job, the plumbing's job is done"
                        ),
                    }
                }
                CycleResultPayload::Unstructured(text) => {
                    println!("Unstructured (not JSON): {text}");
                }
                CycleResultPayload::Absent => {
                    println!("Absent — the turn completed with no agent message");
                }
            }

            println!("\n--- activity (lane 1 projects commands + file changes only) ---");
            if run.activity.is_empty() {
                println!("none reported (expected for a message-only plumbing turn)");
            } else {
                for entry in &run.activity {
                    println!("{entry:?}");
                }
            }

            print_binding_state(&binding_root, Some(run.receipt.worktree.as_str()));
            Some(())
        }
        Ok(Err(error)) => {
            println!("\n--- spawn FAILED (typed) ---");
            println!("{error}");
            println!("{error:?}");
            print_binding_state(&binding_root, None);
            None
        }
        Err(_) => {
            println!("\n--- spawn PANICKED ---");
            println!(
                "(the saga is supposed to return a TYPED SpawnError, never unwind — \
                 a panic here is a bug in the saga, not a backend failure)"
            );
            print_binding_state(&binding_root, None);
            None
        }
    };

    // 5. Config isolation, after. Reported before any early return: the
    //    verdict is owed regardless of how the spawn went.
    println!("\n--- config isolation (PRD 18 acceptance criterion 11) ---");
    let after = ConfigSnapshot::capture(&codex_home)
        .map_err(|e| format!("could not re-snapshot {}: {e}", codex_home.display()))?;
    let report = before.compare(&after);
    print!("{}", report.render());
    if report.passed() {
        println!("isolation PASS: config.toml, auth.json and installation_id all identical");
    } else {
        println!("isolation FAIL: the operator's Codex config CHANGED across this run");
        return Err(
            "config isolation violated — STOP AND HOLD, report the lines above before \
             running anything else"
                .to_string(),
        );
    }

    match spawn_result {
        Ok(Ok(_)) => {
            let _ = outcome;
            Ok(())
        }
        Ok(Err(error)) => Err(format!("coupled spawn failed: {error}")),
        Err(_) => Err("coupled spawn panicked (see above)".to_string()),
    }
}

/// `git init` a repository with one commit, with identity pinned so the commit
/// does not depend on whose machine ran this.
fn init_source_repository(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path)
        .map_err(|e| format!("could not create {}: {e}", path.display()))?;
    let git = GitCli::new();
    let steps: [&[&str]; 5] = [
        &["init", "--initial-branch=main", "-q"],
        &["config", "user.name", "Tidepool Lane 1"],
        &["config", "user.email", "lane1@example.invalid"],
        &["config", "commit.gpgsign", "false"],
        &["config", "gc.auto", "0"],
    ];
    for args in steps {
        git.try_run(path, args)
            .map_err(|e| format!("git {args:?} failed in {}: {e}", path.display()))?;
    }
    std::fs::write(
        path.join("README.md"),
        "lane-1 live one-cycle source repo\n",
    )
    .map_err(|e| format!("could not write README.md: {e}"))?;
    git.try_run(path, &["add", "--", "README.md"])
        .map_err(|e| format!("git add failed: {e}"))?;
    git.try_run(path, &["commit", "-q", "-m", "seed"])
        .map_err(|e| format!("git commit failed: {e}"))?;
    println!("source repository: {} (1 commit on main)", path.display());
    Ok(())
}

/// The binding table's final state read straight off disk — one JSON file per
/// worktree id, holding that worktree's full lease history. Read as bytes
/// rather than through `BindingTable::open`, which would contend with the
/// spawner's lifetime flock.
fn print_binding_state(binding_root: &Path, worktree_id: Option<&str>) {
    println!(
        "\n--- binding table on disk ({}) ---",
        binding_root.display()
    );
    if let Some(id) = worktree_id {
        let path = binding_root.join(format!("{id}.json"));
        match std::fs::read_to_string(&path) {
            Ok(contents) => {
                println!("{}:", path.display());
                println!("{contents}");
                return;
            }
            Err(e) => println!("could not read {}: {e}", path.display()),
        }
    }
    match list_json_files(binding_root) {
        Ok(files) if files.is_empty() => println!("(no binding files — nothing was ever bound)"),
        Ok(files) => {
            for path in files {
                match std::fs::read_to_string(&path) {
                    Ok(contents) => println!("{}:\n{contents}", path.display()),
                    Err(e) => println!("could not read {}: {e}", path.display()),
                }
            }
        }
        Err(e) => println!("could not list {}: {e}", binding_root.display()),
    }
}

fn list_json_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}
