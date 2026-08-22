//! PRD 21 lane C3 — the SCRIPTED acceptance tier for the recursive companion
//! (`plans/self-iterating-harness/21-c3-recursive-companion-slice.md` §9).
//!
//! **It drives the REAL harness**, `harness-dogfooding/recursive-companion/`,
//! through the production entry point (`SelfHarnessDriver::run_one_cycle`) —
//! not a fixture copy. A copied fixture would keep passing while the shipped
//! harness rotted, and the shipped harness IS the deliverable;
//! `dogfood_harness_typecheck.rs` already reaches that directory from this
//! crate, so the precedent (and the include-root shape) is established.
//!
//! **No live model, no operator, no repository.** Every cognition window is
//! served by `support::scripted_provider::KeyedProvider` (this file's own
//! proving ground — see that module's doc for the mechanism, now shared with
//! `delegate_positive_path.rs`/`delegate_merge_fold.rs`) and every gate
//! presentation by [`ScriptedGate`]; the only substrate that runs for real is
//! the JIT, the driver, and GHC.
//!
//! # How a scenario is scripted
//!
//! Two knobs, and between them they cover this file's kept §9 rows:
//!
//! - **The provider table.** Every window's prompt embeds its node's
//!   `NodePath` (`NODE <path> — DISCOVER` / `NODE <path> — FOLD`) as its own
//!   FIRST line — a structural echo of the label `bulkLayerWindow` stamps on
//!   the `runLLMTurnBranchFanout` wire payload itself (`Harness.hs`'s
//!   `bulkLayerWindow` doc: "the label rides the wire structurally, never
//!   parsed back out of the prompt" — true of the DRIVER's own routing; this
//!   test file has no access to that wire field, since `ModelProvider::complete`
//!   only ever sees the assembled `TurnRequest`, so it reads the one
//!   HEADER LINE the harness renders in every window's own voice, via
//!   [`parse_window`] — the SAME single parse point [`Run`]'s own indexing
//!   already used before this rewrite). [`KeyedProvider`] keys a reply on the
//!   parsed `(path, phase)` pair — an [`Script::path`] of [`PathKey::Exact`]
//!   when this file authored the branch's title itself (so its slug is known
//!   ahead of time) or [`PathKey::Prefix`] when the slug is model-derived (a
//!   hostile title, an operator-added branch) and only the branch's POSITION
//!   is known in advance — never a substring match against arbitrary prose
//!   inside the message. A window re-visited more than once (a starved
//!   window's repeated re-prompts; a corrective retry) is served
//!   [`Script::replies`] front to back, one per call, the last one sticking —
//!   the round-ordinal case, resolved by call COUNT against one key rather
//!   than by matching a marker string like "did not validate" inside the
//!   retry prompt. `ReplayProvider` cannot serve any of this: its queue is
//!   strictly FIFO, and the order in which windows reach the provider is
//!   itself under test.
//! - **The seeded checkpoint.** Scenario config (`maxDepth`/`maxNodes`/
//!   `maxFanOut`/`gatePolicy`) has to vary per scenario and `initialState` is
//!   fixed, so each scenario writes a durable `persistence::Checkpoint`
//!   carrying its own `State` JSON and boots through
//!   `SelfHarnessDriver::restore` — the production restart path, not a
//!   test-only argument. `State` derives `ToJSON`/`FromJSON`, so the config
//!   crosses as ordinary state.
//!
//! # Family-bundle discipline, and what actually costs a compile
//!
//! One SCENARIO CONFIG (which the driver splices into the compile alongside
//! the rest of the restored `State`) is one compile shape: two runs sharing
//! a config share that compile (the memo makes the second free), and two
//! runs differing only in their provider table or gate script cost nothing
//! extra. Each kept scenario below asserts as many §9 rows as its config can
//! carry; the three gate runs (prune, amend, add) share ONE config, and the
//! fold-lineage rewrite's own new-behavior pins share the "tree" scenario's.
//! Answerer-side compiles are shared the same way: every leaf reuses ONE
//! `ProposeFinish` reply and every fold reuses ONE `FoldDecision` reply
//! wherever a scenario does not need a node-specific override, so those
//! blocks compile once for the whole file regardless of how many scenarios
//! reuse them.
//!
//! Four configs is the floor, not a preference: the depth cap, the node cap
//! and the fan-out cap are three DIFFERENT `Config` values, and a run cannot
//! hold two of them without confounding which cap fired.
//!
//! GHC-heavy: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH
//! (`--ignore-default-filter` to run). What remains warm across runs is only
//! the `tidepool-extract` compile memo (one entry per distinct scenario
//! config); per-window JIT compilation is never memoized, so wall time still
//! scales with the number of windows a scenario's tree actually opens.
//!
//! # What this file pins, and what it does not
//!
//! This suite used to script every window by matching a SET of substrings
//! against the request's raw prompt text (a "needle" table) and to pin
//! several tests' worth of exact prompt WORDING (a coalgebra prompt's full
//! literal shape; a marker string's exact occurrence count in the assembled
//! framing). Both are gone: the provider now keys structurally (above), and
//! every surviving test asserts BEHAVIOR the driver actually produces — a
//! branch's own window reached, a failure folded as data at its own
//! position, declared-order reassembly, a node id's containment safety —
//! never the literal prose of a prompt this file itself is free to reword.
//! A test that existed only to pin prompt wording (or the one-off teaching
//! block's exact placement) is deleted outright rather than re-keyed; see
//! `submit_branch`'s receipts for the kept/deleted accounting.

mod support;

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;

use serde_json::{json, Value as Json};

use tidepool_handlers::{load_journal, ConsoleHandler, JournalEntry, JournalHandler, SegmentPath};
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Event as LogEvent, LogHeader, LogReader, LogWriter};
use tidepool_harness::provider::DynModelProvider;
use tidepool_harness::selfharness::operator::FormShape;
use tidepool_harness::selfharness::persistence;
use tidepool_harness::tree::NodeId;
use tidepool_harness::{
    answerer_decls, load_harness_source, Event as DriverEvent, Harness, Observer, OperatorGate,
    SelfHarnessDriver,
};

use support::scripted_provider::{parse_window, script, KeyedProvider, PathKey, Phase, Script};

// ---------------------------------------------------------------------------
// Where the real harness lives
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

/// The SHIPPED harness, not a fixture copy — see this file's module doc.
fn companion_dir() -> PathBuf {
    repo_root().join("harness-dogfooding/recursive-companion")
}

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "companion-recursive-slice-{}-{label}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn header(label: &str) -> LogHeader {
    LogHeader {
        prelude_hash: format!("companion-recursive-{label}"),
        extract_fingerprint: format!("companion-recursive-{label}"),
        harness_version: "test".into(),
    }
}

// ---------------------------------------------------------------------------
// The scripted provider — keyed structurally on (path, phase[, ordinal]),
// never on prompt-text substrings. `PathKey`/`Script`/`script`/`KeyedProvider`/
// `Phase`/`parse_window` now live in `support::scripted_provider` — this file
// was their original proving ground (commit b135d174), and
// `delegate_positive_path.rs`/`delegate_merge_fold.rs` now share this same
// copy instead of each re-deriving their own needle-set matcher.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// The scripted operator gate
// ---------------------------------------------------------------------------

/// An [`OperatorGate`] that answers each `askUser @LayerApproval` from a
/// queue, recording every presentation. An empty queue submits `{}` — a
/// malformed submission `Tidepool.Form.askUser` re-presents rather than fails
/// — so a scenario that expects NO gate at all (§9 row 9) is asserted by
/// `presented().is_empty()`, not by a panic inside `block_in_place`.
#[derive(Default)]
struct ScriptedGate {
    submissions: Mutex<Vec<Json>>,
    presented: Mutex<Vec<FormShape>>,
}

impl ScriptedGate {
    fn new(submissions: Vec<Json>) -> Self {
        ScriptedGate {
            submissions: Mutex::new(submissions),
            presented: Mutex::new(Vec::new()),
        }
    }

    fn presentations(&self) -> usize {
        self.presented.lock().len()
    }
}

impl OperatorGate for ScriptedGate {
    fn present_form(&self, shape: &FormShape) -> Json {
        self.presented.lock().push(shape.clone());
        let mut queue = self.submissions.lock();
        if queue.is_empty() {
            json!({})
        } else {
            queue.remove(0)
        }
    }
}

/// One `LayerApproval` submission, as the PLAIN JSON the generic `FromJSON`
/// decode reads: a record is an object of its fields, and a nullary-sum field
/// (`gateVerdict`, `gateRole`) is the chosen constructor as a bare string.
fn verdict(kind: &str, target: &str, text: &str) -> Json {
    json!({
        "gateVerdict": kind,
        "gateTarget": target,
        "gateTitle": "",
        "gateRole": "Primary",
        "gateText": text,
        "gateNote": "scripted",
    })
}

/// An `Add` submission — the one verdict `verdict()` above cannot express,
/// since it always submits an empty `gateTitle` (fine for the verdicts that
/// ignore it, but `Add` refuses a blank title).
fn add_verdict(title: &str, role: &str, text: &str) -> Json {
    json!({
        "gateVerdict": "Add",
        "gateTarget": "",
        "gateTitle": title,
        "gateRole": role,
        "gateText": text,
        "gateNote": "scripted",
    })
}

// ---------------------------------------------------------------------------
// The observer — what pairs a window's PROMPT with the NODE that served it
// ---------------------------------------------------------------------------

/// Records the (prompt, node) pair for every cognition window, plus every
/// operator form.
///
/// The pairing is the driver's own emission order: `service_outer_branch` /
/// `service_outer_fanout` / `service_outer_branch_fanout` emit
/// `RunLLMTurnHole{prompt}` and then `TurnStart{node}` for the window(s)
/// they mint. A single window (`runLLMTurnBranch`, a single-child
/// `runLLMTurnFork`) is sequential by construction — one hole immediately
/// followed by its own `TurnStart`. A BULK window
/// (`runLLMTurnBranchFanout`/`runLLMTurnFanout`) is not: every sibling's
/// `RunLLMTurnHole` is emitted upfront, in DECLARED order, before any of
/// them is driven — sibling windows are ALWAYS driven concurrently
/// (operator decision), so their `TurnStart`s land in COMPLETION order,
/// not declaration order. A QUEUE (not a single slot) is what lets this
/// still pair correctly: every hole is enqueued, and every `TurnStart`
/// dequeues the OLDEST still-pending prompt — exact for this suite's
/// deterministic scripted providers (no artificial per-child delay ever
/// separates a group's completion order from its declaration order here;
/// `outer_fanout.rs` is where completion-order insensitivity itself is
/// pinned, via a provider that deliberately forces one).
///
/// This is what lets a `BranchInvocation` receipt — which carries a `NodeId`
/// and no path — be attributed to the node whose window it belongs to,
/// without inferring anything from event ORDER.
#[derive(Default)]
struct WindowObserver {
    pending: Mutex<VecDeque<String>>,
    windows: Mutex<Vec<(String, NodeId)>>,
    forms: Mutex<Vec<Json>>,
}

impl Observer for WindowObserver {
    fn on_event(&self, event: &DriverEvent) {
        match event {
            DriverEvent::RunLLMTurnHole { prompt, .. } => {
                self.pending.lock().push_back(prompt.clone());
            }
            DriverEvent::TurnStart { node } => {
                if let Some(prompt) = self.pending.lock().pop_front() {
                    self.windows.lock().push((prompt, *node));
                }
            }
            DriverEvent::FormSubmitted { submission, .. } => {
                self.forms.lock().push(submission.clone());
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// One scenario run
// ---------------------------------------------------------------------------

/// Everything one cycle left behind: the harness's own durable `State`, the
/// runtime's per-node event log, the authored journal, the provider's record
/// of what it was asked, and the operator forms raised.
struct Run {
    state: Json,
    /// (prompt, node) per cognition window, in order.
    windows: Vec<(String, NodeId)>,
    /// Every request's last message, as the provider saw it.
    requests: Vec<String>,
    log: Vec<LogEvent>,
    journal: Vec<JournalEntry>,
    gate_presentations: usize,
    gate_submissions: Vec<Json>,
}

impl Run {
    fn windows_in(&self, phase: Phase) -> Vec<(String, NodeId)> {
        self.windows
            .iter()
            .filter_map(|(prompt, node)| match parse_window(prompt) {
                Some((path, p)) if p == phase => Some((path, *node)),
                _ => None,
            })
            .collect()
    }

    /// The node paths whose `phase` window actually RAN, in order.
    fn paths_in(&self, phase: Phase) -> Vec<String> {
        self.windows_in(phase).into_iter().map(|(p, _)| p).collect()
    }

    /// The one path starting with `prefix` whose coalgebra window ran — how a
    /// test names a node whose id the harness derived from a model-produced
    /// title (§3's slug) without hardcoding the slug.
    fn discovered_under(&self, prefix: &str) -> String {
        let hits: Vec<String> = self
            .paths_in(Phase::Discover)
            .into_iter()
            .filter(|p| p.starts_with(prefix))
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "expected exactly one discovered node under {prefix:?}, got {hits:?}"
        );
        hits.into_iter().next().unwrap()
    }

    fn window_node(&self, path: &str, phase: Phase) -> NodeId {
        self.windows_in(phase)
            .into_iter()
            .find(|(p, _)| p == path)
            .unwrap_or_else(|| panic!("no {phase:?} window ran for {path}"))
            .1
    }

    /// The prompt the window for `path`/`phase` was actually given.
    fn window_prompt(&self, path: &str, phase: Phase) -> String {
        self.windows
            .iter()
            .find(|(prompt, _)| parse_window(prompt) == Some((path.to_string(), phase)))
            .unwrap_or_else(|| panic!("no {phase:?} window ran for {path}"))
            .0
            .clone()
    }

    /// The request (hole card) the provider was handed for `path`/`phase`.
    fn request_for(&self, path: &str, phase: Phase) -> String {
        let needle = format!(
            "NODE {path} — {}",
            match phase {
                Phase::Discover => "DISCOVER",
                Phase::Fold => "FOLD",
            }
        );
        let hits: Vec<&String> = self
            .requests
            .iter()
            .filter(|r| r.contains(&needle))
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "expected exactly one provider request for {needle}, got {}",
            hits.len()
        );
        hits[0].clone()
    }

    /// `(node, digest, prefix_bytes)` for every `SnapshotFrozen` receipt.
    fn frozen(&self) -> Vec<(NodeId, String, u64)> {
        self.log
            .iter()
            .filter_map(|e| match e {
                LogEvent::SnapshotFrozen {
                    node,
                    digest,
                    prefix_bytes,
                    ..
                } => Some((*node, digest.as_str().to_string(), *prefix_bytes)),
                _ => None,
            })
            .collect()
    }

    /// `(node, snapshot, shared_prefix_bytes, branch_suffix_bytes)` for every
    /// `BranchInvocation` receipt — written ONLY for a node minted through
    /// `fork_from_snapshot`, and only after the harness's own
    /// re-digest-and-compare check passed.
    fn branch_invocations(&self) -> Vec<(NodeId, String, u64, u64)> {
        self.log
            .iter()
            .filter_map(|e| match e {
                LogEvent::BranchInvocation {
                    node,
                    snapshot,
                    shared_prefix_bytes,
                    branch_suffix_bytes,
                    ..
                } => Some((
                    *node,
                    snapshot.as_str().to_string(),
                    *shared_prefix_bytes,
                    *branch_suffix_bytes,
                )),
                _ => None,
            })
            .collect()
    }

    fn branch_invocation_of(&self, node: NodeId) -> (String, u64, u64) {
        self.branch_invocations()
            .into_iter()
            .find(|(n, ..)| *n == node)
            .map(|(_, d, s, b)| (d, s, b))
            .unwrap_or_else(|| panic!("no BranchInvocation receipt for {node:?}"))
    }

    fn frozen_of(&self, node: NodeId) -> (String, u64) {
        let hits: Vec<(String, u64)> = self
            .frozen()
            .into_iter()
            .filter(|(n, ..)| *n == node)
            .map(|(_, d, b)| (d, b))
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "expected exactly one SnapshotFrozen receipt for {node:?}, got {hits:?}"
        );
        hits.into_iter().next().unwrap()
    }

    fn run_summary(&self) -> &Json {
        self.state
            .get("lastRun")
            .filter(|v| !v.is_null())
            .unwrap_or_else(|| panic!("the cycle recorded no lastRun: {}", self.state))
    }

    fn counter(&self, field: &str) -> i64 {
        self.run_summary()
            .get(field)
            .and_then(|v| v.as_i64())
            .unwrap_or_else(|| panic!("lastRun.{field} must be an Int: {}", self.run_summary()))
    }

    /// The render's tree — one line per node, `render`'s own output.
    fn tree(&self) -> Vec<String> {
        self.run_summary()
            .get("runTree")
            .and_then(|v| v.as_array())
            .expect("lastRun.runTree is an array")
            .iter()
            .map(|v| v.as_str().expect("a tree line is a string").to_string())
            .collect()
    }

    /// The one tree line for `path` (matched on the rendered path token, so an
    /// indent or a badge cannot make it miss).
    fn tree_line(&self, path: &str) -> String {
        let hits: Vec<String> = self
            .tree()
            .into_iter()
            .filter(|l| l.split_whitespace().next() == Some(path))
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "expected exactly one tree line for {path}, got {:?}",
            self.tree()
        );
        hits[0].clone()
    }

    /// One branch's block out of an algebra prompt's rendered layer —
    /// `--- branch <path>: <title> (<role>) [<posture>]` and the synthesis
    /// under it, up to the next branch.
    ///
    /// This is the channel a child's ANSWER actually reaches its parent by
    /// (the render's tree line carries identity and badges, never the
    /// synthesis), so it is what "the sibling's answer still arrived" has to
    /// be read off.
    fn branch_summary(&self, parent: &str, path: &str) -> String {
        let prompt = self.window_prompt(parent, Phase::Fold);
        let head = format!("--- branch {path}:");
        let start = prompt
            .find(&head)
            .unwrap_or_else(|| panic!("{parent}'s fold saw no branch {path}:\n{prompt}"));
        let rest = &prompt[start..];
        let end = ["--- branch ", "\n\nFold this realized layer"]
            .iter()
            .filter_map(|marker| rest[head.len()..].find(marker).map(|i| i + head.len()))
            .min()
            .unwrap_or(rest.len());
        rest[..end].to_string()
    }

    /// Every `(kind, key)` the authored journal recorded, in order.
    fn journal_pairs(&self) -> Vec<(String, String)> {
        self.journal
            .iter()
            .map(|e| (e.kind.clone(), e.key.clone()))
            .collect()
    }

    fn journal_kind(&self, kind: &str) -> Vec<&JournalEntry> {
        self.journal.iter().filter(|e| e.kind == kind).collect()
    }
}

/// One scenario's `State`, as the durable JSON a checkpoint carries.
///
/// `gate_policy` is the wire shape the vendored generic `FromJSON` reads for a
/// sum with a payload arm: aeson's default `TaggedObject`
/// (`{"tag":"GateWiderThan","gateWidth":3}`); an all-nullary sum would be a
/// bare string, which is why `gateVerdict` above is one and this is not.
fn state_json(max_depth: i64, max_nodes: i64, max_fan_out: i64, gate_policy: Json) -> Json {
    json!({
        "question": "SCENARIO: drive the recursive companion on scripted windows.",
        "config": {
            "maxDepth": max_depth,
            "maxNodes": max_nodes,
            "maxFanOut": max_fan_out,
            "gatePolicy": gate_policy,
            "gateMaxRounds": 8,
        },
        "turnCount": 0,
        "lastRun": Json::Null,
    })
}

/// As [`state_json`], with an EXTRA, no-longer-declared `draft` key spliced
/// in — the fold-lineage rewrite deleted `State.draft` (the checked-edits
/// in-heap draft; the worktree/delegate channel is the file-edit mechanism
/// now), and the vendored generic `FromJSON` decode reads a record by
/// looking up each of ITS OWN fields by name (`o .: fieldName`) rather than
/// requiring the object's key set to match exactly — so an old checkpoint
/// carrying a stale `draft` key must still decode clean, the key simply
/// never looked up. `companion_old_checkpoint_with_stale_draft_key_still_boots`
/// is the one caller.
fn state_json_with_stale_draft_key(
    max_depth: i64,
    max_nodes: i64,
    max_fan_out: i64,
    gate_policy: Json,
) -> Json {
    let mut v = state_json(max_depth, max_nodes, max_fan_out, gate_policy);
    v["draft"] = json!("a pre-fold-lineage checkpoint's stale in-heap draft");
    v
}

/// Drive ONE cycle of the real recursive-companion harness against `scripted`
/// and `gate`, booting from a durable checkpoint carrying `state`.
///
/// The boot is the production restart path — `persistence::save_checkpoint`
/// then `SelfHarnessDriver::restore` — rather than handing `run_one_cycle` a
/// state directly, so the scenario's config crosses the same seam a real
/// restarted process's would.
async fn run_scenario(
    label: &str,
    state: Json,
    scripted: Vec<Script>,
    gate: Arc<ScriptedGate>,
) -> Run {
    support::require_extract();

    let dir = scratch(label);
    let checkpoint_path = dir.join("checkpoint.json");
    let journal_path = dir.join("journal.jsonl");
    let log_path = dir.join("log.jsonl");

    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        repo_root().join("haskell/lib"),
        Some(companion_dir()),
    )
    .expect("answerer engine config over the recursive-companion harness dir");

    let provider = Arc::new(KeyedProvider::new(scripted));
    let dyn_provider: Arc<dyn DynModelProvider> = provider.clone();
    let writer = LogWriter::create(&log_path, &header(label)).expect("log writer");
    let agent =
        Arc::new(Harness::new(writer, agent_cfg, dyn_provider).expect("agent harness boots"));

    let observer = Arc::new(WindowObserver::default());
    let mut driver = SelfHarnessDriver::new(agent, observer.clone());
    driver.set_checkpoint_path(checkpoint_path.clone());
    driver.set_console_handler(ConsoleHandler);
    driver.set_journal_handler(JournalHandler::new(
        SegmentPath::create_exclusive(journal_path.clone())
            .expect("this scenario's journal segment is fresh in its own tempdir"),
    ));
    driver.set_gate(gate.clone());
    // Every scripted window finalizes on its FIRST round, so lowering the
    // round caps changes nothing for them — and it makes a deliberately
    // STARVED window (row 4b) cost four instant provider calls with no
    // compiles instead of thirty-four. Same lever `outer_fanout.rs`'s
    // round-exhaustion check pulls, for the same reason.
    driver.set_answerer_round_caps(1, 2);

    let source = load_harness_source(&companion_dir().join("Harness.hs"))
        .expect("the SHIPPED recursive-companion harness loads");

    persistence::save_checkpoint(
        &checkpoint_path,
        &persistence::Checkpoint::committed(
            None,
            state,
            None,
            // The CURRENT source's fingerprint: this checkpoint is seeding a
            // scenario config, not simulating a since-edited harness, so the
            // carry-forward path must not fire.
            source.fingerprint.clone(),
            persistence::LoopIteration::new(0),
        ),
    )
    .expect("seed the scenario's durable checkpoint");

    let restored = driver
        .restore(&source)
        .await
        .expect("restore the seeded checkpoint")
        .expect("the seeded checkpoint is on disk");

    let outcome = driver
        .run_one_cycle(&source, Some(&restored))
        .await
        .expect("one render -> loop -> thoughtHylo -> render cycle");

    let (_header, events) = LogReader::open(&log_path).expect("log opens");
    let log: Vec<LogEvent> = events.map(|r| r.expect("event parses").event).collect();
    let journal = if journal_path.exists() {
        load_journal(&journal_path).expect("journal loads")
    } else {
        Vec::new()
    };

    let windows = observer.windows.lock().clone();
    let gate_submissions = observer.forms.lock().clone();
    let requests = provider.seen();
    Run {
        state: outcome.state_json,
        windows,
        requests,
        log,
        journal,
        gate_presentations: gate.presentations(),
        gate_submissions,
    }
}

// ---------------------------------------------------------------------------
// Scripted replies — shared across scenarios so they compile ONCE
// ---------------------------------------------------------------------------

fn haskell(block: &str) -> String {
    format!("```haskell\n{block}\n```")
}

/// `finalize @LayerProposal (ProposeSplit …)` — `branches` is
/// `(title, role, instruction)` in declared order.
fn split_reply(posture: &str, focus: &str, branches: &[(&str, &str, &str)]) -> String {
    let rendered: Vec<String> = branches
        .iter()
        .map(|(title, role, instruction)| {
            format!(
                "ProposedBranch {{ branchTitle = \"{title}\", branchRole = {role}, \
                 branchInstruction = \"{instruction}\" }}"
            )
        })
        .collect();
    haskell(&format!(
        "finalize @LayerProposal (ProposeSplit {{ splitPosture = {posture}, splitFocus = \
         \"{focus}\", splitBranches = [{}] }})",
        rendered.join(", ")
    ))
}

/// The instruction `split_two`'s first branch carries — named, because §9 row
/// 8b turns on whether an operator's `Amend` replaced it in the SEED the
/// child's own window is prompted from.
const ALPHA_INSTRUCTION: &str = "the instruction the model proposed for alpha";

/// The TWO-branch split every scenario that needs one reuses, verbatim.
///
/// Reused rather than re-worded per scenario for a reason that is the whole
/// point of the family bundle: an answerer compile is keyed by the block's
/// SOURCE, so one shared reply text is one compile for the entire file, while
/// several near-identical ones would each pay their own. The [`Script`] KEY
/// (its `path`/`phase`) is what varies per scenario; the reply does not have
/// to.
fn split_two() -> String {
    split_reply(
        "Explore",
        "which reading of this node holds",
        &[
            ("Alpha", "Primary", ALPHA_INSTRUCTION),
            (
                "Beta",
                "Alternative",
                "the instruction the model proposed for beta",
            ),
        ],
    )
}

/// The THREE-branch split, reused the same way.
fn split_three() -> String {
    split_reply(
        "Explore",
        "which reading of this node holds",
        &[
            ("Alpha", "Primary", ALPHA_INSTRUCTION),
            (
                "Beta",
                "Alternative",
                "the instruction the model proposed for beta",
            ),
            (
                "Gamma",
                "Critic",
                "the instruction the model proposed for gamma",
            ),
        ],
    )
}

/// A ONE-branch split — the cheapest way to turn an otherwise-childless node
/// into an INTERIOR one. Under mechanical-leaf-fold semantics a childless
/// non-root node runs no fold window at all, so a scenario whose whole point
/// is that node's OWN fold (proposing or selecting an edit) needs it to have
/// at least one child; this mints that one child without adding any shape of
/// its own for the scenario to account for. Reused verbatim everywhere it's
/// needed, one compile for the whole file.
fn split_one() -> String {
    split_reply(
        "Explore",
        "one more layer, to keep this node interior",
        &[("Solo", "Primary", "the only child of this interior node")],
    )
}

/// The text `finish_reply()` finalizes with. Under mechanical-leaf-fold
/// semantics (commit 8afe890b) a childless non-root node's fold window never
/// runs at all, so its synthesis — what a parent's fold sees at that branch's
/// position — is this text verbatim (`leafAnswerText`/`Harness.hs`), never
/// the scripted fold marker `"FOLDED"`. Named so assertions can check for it
/// without re-typing the literal.
const LEAF_FINISH_TEXT: &str = "this node answers locally";

/// The ONE leaf reply every scenario's leaves share — one answerer compile for
/// the whole file.
fn finish_reply() -> String {
    haskell(&format!(
        "finalize @LayerProposal (ProposeFinish {{ localAnswer = \"{LEAF_FINISH_TEXT}\" }})"
    ))
}

/// A structurally UNUSABLE layer: a split declaring no branches. §2's
/// `layerFromProposal` turns it into `Finish (Draft _ (InvocationFailed _))`,
/// which the algebra folds as ordinary data.
fn empty_split_reply() -> String {
    split_reply("Explore", "nothing usable", &[])
}

/// The ONE fold reply every node's algebra window shares: a `FoldDecision`
/// of just a synthesis and one tension — the whole wire shape, after the
/// fold-lineage rewrite retired the checked-edits fields.
fn fold_reply() -> String {
    haskell(
        "finalize @FoldDecision (FoldDecision { foldSynthesis = \"FOLDED\", \
         foldTensions = [\"one unresolved tension\"] })",
    )
}

/// The catch-all fold entry, matched last (any path, phase `Fold`).
fn fold_script() -> Script {
    script(PathKey::Prefix(""), Phase::Fold, fold_reply())
}

/// A window that never answers: prose with no fenced block, so every one of
/// its rounds is a `NoBlock` re-prompt and its round budget is spent without
/// it ever running anything. The cheapest honest way to reach
/// `InvocationExit::RoundsExhausted` — no compiles at all on this branch.
fn starved_reply() -> String {
    "Still weighing this branch; nothing to run yet.".to_string()
}

// ---------------------------------------------------------------------------
// Shared named checks
// ---------------------------------------------------------------------------

/// Every segment of a node id is `<index>-<slug>` with the slug drawn from
/// `[a-z0-9-]` and at most 32 characters (§3). Hand-rolled rather than a
/// regex dependency; the shape is small enough to read.
fn is_node_segment(segment: &str) -> bool {
    let Some(dash) = segment.find('-') else {
        return false;
    };
    let (index, rest) = segment.split_at(dash);
    let slug = &rest[1..];
    !index.is_empty()
        && index.chars().all(|c| c.is_ascii_digit())
        && !slug.is_empty()
        && slug.len() <= 32
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// §9 row 11 — node ids are containment-safe.
///
/// `tidepool-web`'s loopback trust model rests on "`node_id` is always a
/// substrate identifier a caller passed to `register_node`, never
/// model-produced text", and a branch title IS model-produced text. Every id
/// the run EMITTED (the journal's keys are `renderPath` of the node each entry
/// is about — the harness's own identity channel) is checked segment by
/// segment, and the punctuation/markup/unicode the title carried must appear
/// in none of them.
fn row_11_node_ids_are_containment_safe(run: &Run, raw_title_fragments: &[&str]) {
    let keys: Vec<String> = run.journal.iter().map(|e| e.key.clone()).collect();
    assert!(!keys.is_empty(), "the run journaled nothing to check");
    for key in &keys {
        let mut segments = key.split('/');
        assert_eq!(
            segments.next(),
            Some("root"),
            "every node id is root-relative: {key}"
        );
        for segment in segments {
            assert!(
                is_node_segment(segment),
                "node id segment {segment:?} (in {key:?}) must be <index>-<slug> with the \
                 slug drawn from [a-z0-9-] and at most 32 chars — this is the containment \
                 invariant tidepool-web's no-injection-surface story rests on, not cosmetics"
            );
        }
        for fragment in raw_title_fragments {
            assert!(
                !key.contains(fragment),
                "the model-produced title fragment {fragment:?} reached a node id: {key:?}"
            );
        }
    }
}

/// §9 row 10 — the turn is journaled per node event, keyed by `NodePath`.
fn row_10_journal_is_keyed_by_node_path(run: &Run, expected: &[(&str, &str)]) {
    let actual = run.journal_pairs();
    let expected: Vec<(String, String)> = expected
        .iter()
        .map(|(k, p)| ((*k).to_string(), (*p).to_string()))
        .collect();
    // Compare as multisets: the ORDER of journal kinds across a tree is the
    // traversal's business (and pinned by the tree assertions elsewhere);
    // what row 10 claims is one entry per node event, keyed by the node's path.
    let bag = |pairs: &[(String, String)]| {
        let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
        for pair in pairs {
            *counts.entry(pair.clone()).or_default() += 1;
        }
        counts
    };
    assert_eq!(
        bag(&actual),
        bag(&expected),
        "the journal must carry exactly one entry per node event, keyed by NodePath\n\
         got: {actual:#?}"
    );
}

// ---------------------------------------------------------------------------
// Scenario A — the tree: §9 rows 1, 2, 3, 4, 9, 10, 11
// ---------------------------------------------------------------------------

/// A branch title carrying punctuation, markup and non-ASCII — model-produced
/// text, which is exactly what §3's slug exists to contain (row 11).
const HOSTILE_TITLE: &str = "Beta!! <b>risk</b> ünïcode";

/// The fragments of [`HOSTILE_TITLE`] that must never reach a node id.
const HOSTILE_FRAGMENTS: [&str; 5] = ["!", "<", ">", "/b", "ü"];

/// §9 rows 1, 2, 3, 4, 4b, 9, 10 and 11, off ONE compile shape.
///
/// The tree (five branches at the root, one of them splitting again, one
/// finalizing an unusable layer, one whose coalgebra window never answers,
/// and one interior node whose ALGEBRA window never answers) is chosen so a
/// single run carries every row a `GateOff`, generously-budgeted config can
/// carry — family-bundle discipline: the scenarios that follow each exist
/// only because they need a DIFFERENT `Config`, which is a different compile.
/// Three of §8's four failure shapes therefore sit side by side in ONE tree,
/// which is also the strongest form of the claim: none of them erases another.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn companion_tree_recurses_folds_and_contains_its_node_ids() {
    let _cache_guard = support::isolate_cache();

    let run = run_scenario(
        "tree",
        state_json(3, 40, 5, json!({"tag": "GateOff"})),
        vec![
            script(
                PathKey::Exact("root"),
                Phase::Discover,
                split_reply(
                    "Compare",
                    "what the slice must show",
                    &[
                        ("Alpha", "Primary", "work the alpha angle"),
                        (HOSTILE_TITLE, "Critic", "attack the beta claim"),
                        ("Gamma", "Alternative", "the gamma alternative"),
                        ("Delta", "Primary", "the delta option"),
                        ("Epsilon", "Critic", "the branch whose window never answers"),
                    ],
                ),
            ),
            script(PathKey::Exact("root/1-alpha"), Phase::Discover, split_two()),
            script(
                PathKey::Prefix("root/1-alpha/1-"),
                Phase::Discover,
                finish_reply(),
            ),
            script(
                PathKey::Prefix("root/1-alpha/2-"),
                Phase::Discover,
                finish_reply(),
            ),
            // The hostile-titled branch is ALSO the unusable-layer branch
            // (row 4): a node whose own layer fails still has a node id, so
            // one branch carries both properties.
            script(
                PathKey::Prefix("root/2-"),
                Phase::Discover,
                empty_split_reply(),
            ),
            script(PathKey::Prefix("root/3-"), Phase::Discover, finish_reply()),
            script(PathKey::Prefix("root/4-"), Phase::Discover, finish_reply()),
            // Row 4b: this window is STARVED — it burns its round budget
            // without ever running a block, so its coalgebra comes back as a
            // typed `Left InvocationExit`.
            script(PathKey::Prefix("root/5-"), Phase::Discover, starved_reply()),
            // The ALGEBRA side of the same contract: this node's own FOLD
            // window is starved, so the exit replaces what that node owed and
            // must leave its two children's finished answers alone.
            script(PathKey::Exact("root/1-alpha"), Phase::Fold, starved_reply()),
            fold_script(),
        ],
        Arc::new(ScriptedGate::default()),
    )
    .await;

    let beta = run.discovered_under("root/2-");
    let alpha_one = run.discovered_under("root/1-alpha/1-");
    let alpha_two = run.discovered_under("root/1-alpha/2-");
    let gamma = run.discovered_under("root/3-");
    let delta = run.discovered_under("root/4-");
    let epsilon = run.discovered_under("root/5-");

    // --- row 1: the root is never asked for descendant shape ---------------
    let root_discoveries: Vec<&String> = run
        .requests
        .iter()
        .filter(|r| r.contains("NODE root — DISCOVER"))
        .collect();
    assert_eq!(
        root_discoveries.len(),
        1,
        "the root's coalgebra window must be served exactly ONE reply — the whole \
         tree below it comes from its DESCENDANTS' own windows, never from a deeper \
         answer the root was asked for"
    );
    // The type-level half of row 1, read off the REAL compiled `DataConTable`:
    // the hole card renders `LayerProposal`'s own declaration plus every user
    // type transitively reachable through its field types
    // (`synopsis::type_document`). `LayerProposal` occurring exactly once in
    // that whole document — as its own `data` head, never as a field type of
    // itself or of anything it reaches — IS "no recursive arm".
    let card = run.request_for("root", Phase::Discover);
    let shape = fenced_haskell(&card).expect("the hole card renders the answer type's shape");
    assert!(
        shape.contains("ProposeFinish") && shape.contains("ProposeSplit"),
        "the coalgebra's answer type must be rendered from the compiled table, got:\n{shape}"
    );
    assert!(
        shape.contains("ProposedBranch"),
        "the shape document must expand through field types (or the recursion check \
         below would be vacuous), got:\n{shape}"
    );
    assert_eq!(
        shape.matches("LayerProposal").count(),
        1,
        "`LayerProposal` must occur ONCE in the shape document — as its own data head. \
         A second occurrence would be a recursive arm, i.e. a coalgebra window able to \
         describe more than its own layer. Got:\n{shape}"
    );

    // --- row 2: a grandchild's window is FORKED from its parent's prefix ----
    // `Event::BranchInvocation` is written ONLY for a node minted through
    // `fork_from_snapshot`, and only after the harness's own
    // re-digest-and-compare check passes — an empty-root child (what every
    // pre-GAP-1 fork/fanout child was) produces none at all. Its presence per
    // coalgebra window IS the forked-prefix proof; nothing here reads prompt
    // text for it.
    //
    // Since the fold-lineage rewrite, a NON-mechanical fold window is ALSO a
    // real branch off the node's own ref (never a fresh fork), so it ALSO
    // gets one — `root/1-alpha`'s own fold window opens one even though it
    // then starves before finalizing (row 4b below): the receipt is written
    // at the branch's FIRST turn, independent of whether it ever finalizes.
    // A mechanical leaf's fold never opens a window at all, so it
    // contributes none — the expected total is therefore every DISCOVER
    // window plus every FOLD window that actually ran, read back off the
    // harness's own window record rather than hand-counted.
    let discovered = run.paths_in(Phase::Discover);
    let folded = run.paths_in(Phase::Fold);
    let branches = run.branch_invocations();
    assert_eq!(
        branches.len(),
        discovered.len() + folded.len(),
        "every coalgebra window, and every non-mechanical fold window (both are \
         branches off a frozen prefix now), must produce one BranchInvocation each. \
         Discovered: {discovered:?}, folded: {folded:?}"
    );

    let alpha_node = run.window_node("root/1-alpha", Phase::Discover);
    let (alpha_digest, alpha_prefix_bytes) = run.frozen_of(alpha_node);
    for grandchild in [&alpha_one, &alpha_two] {
        let node = run.window_node(grandchild, Phase::Discover);
        let (snapshot, shared, suffix) = run.branch_invocation_of(node);
        assert_eq!(
            snapshot, alpha_digest,
            "{grandchild}'s window must branch off the prefix its PARENT's window froze \
             — a genuinely inherited transcript, not a rendered ancestry line"
        );
        assert_eq!(
            shared, alpha_prefix_bytes,
            "{grandchild}'s shared-prefix byte count must equal its parent's frozen \
             prefix's own; the harness re-derives this itself before writing the \
             receipt, so equality here is the byte-stability proof"
        );
        assert!(
            suffix > 0,
            "{grandchild} must carry its own divergent suffix past the shared prefix"
        );
    }
    let sibling_digests: Vec<String> = [&alpha_one, &alpha_two]
        .iter()
        .map(|p| {
            run.branch_invocation_of(run.window_node(p, Phase::Discover))
                .0
        })
        .collect();
    assert_eq!(
        sibling_digests[0], sibling_digests[1],
        "sibling branches at one node must name ONE shared frozen digest"
    );
    // Depth 3 really was reached: a grandchild is two segments below the root.
    assert_eq!(
        alpha_one.split('/').count(),
        3,
        "row 2 needs a genuine depth-3 tree, got {alpha_one}"
    );

    // --- row 3: branch-order delivery, never completion order --------------
    // The descent is sequential by construction today (`traverseLayer` is an
    // order-preserving traversal that ignores its `Strategy`), so what this
    // asserts is the DECLARED order reaching the algebra — the property that
    // must survive when green threads make the descent concurrent.
    let root_fold = run.window_prompt("root", Phase::Fold);
    let positions: Vec<usize> = ["root/1-alpha", &beta, &gamma, &delta, &epsilon]
        .iter()
        .map(|path| {
            root_fold
                .find(&format!("--- branch {path}:"))
                .unwrap_or_else(|| panic!("the root's fold must see branch {path}:\n{root_fold}"))
        })
        .collect();
    assert!(
        positions.windows(2).all(|w| w[0] < w[1]),
        "the algebra's rendered layer must list branches in DECLARED order, got \
         positions {positions:?} in:\n{root_fold}"
    );

    // --- row 4: failure accumulates as data (an unusable layer) -------------
    let beta_line = run.tree_line(&beta);
    assert!(
        beta_line.contains("failed:"),
        "a window that finalized a split with no branches must fold as an \
         InvocationFailed finish, got: {beta_line}"
    );
    assert_eq!(
        run.counter("runNodes"),
        8,
        "the failed branches are still nodes, and their siblings still folded"
    );
    assert!(
        run.branch_summary("root", &beta)
            .contains("[finish(failed: the window proposed a split with no branches)]"),
        "the failure is ordinary DATA in the parent's realized layer, at its own \
         branch position: {}",
        run.branch_summary("root", &beta)
    );
    for sibling in [&gamma, &delta] {
        assert!(
            !run.tree_line(sibling).contains("failed"),
            "branch {sibling} must still fold its real answer beside the failed ones"
        );
        assert!(
            run.branch_summary("root", sibling)
                .contains(LEAF_FINISH_TEXT),
            "branch {sibling}'s own answer must still reach its parent's fold — a \
             childless non-root node folds MECHANICALLY (no fold window at all), so \
             its synthesis IS its coalgebra's finish text, never a scripted \"FOLDED\": {}",
            run.branch_summary("root", sibling)
        );
    }

    // --- row 4b: failure accumulates as data (an abnormal exit) ------------
    // A window that burns its round budget without finalizing comes back as
    // `Left InvocationExit`, and `discover` makes that node a leaf whose
    // ORIGIN says why. The turn COMPLETED (this test is reading its state),
    // which is the headline: before the typed-exit verb, one branch's
    // exhausted window failed the whole outer turn and erased every sibling
    // result already produced.
    let epsilon_line = run.tree_line(&epsilon);
    assert!(
        epsilon_line.contains("failed: round exhaustion:"),
        "a window that never finalized must arrive as a TYPED exit at its own \
         branch position, rendered by `renderInvocationExit`: {epsilon_line}"
    );
    assert!(
        run.branch_summary("root", &epsilon)
            .contains("[finish(failed: round exhaustion:"),
        "and it reaches its parent's fold as ordinary data in the realized layer: {}",
        run.branch_summary("root", &epsilon)
    );
    assert_eq!(
        run.counter("runFailed"),
        3,
        "three failures — an unusable layer, a starved coalgebra, and a starved \
         ALGEBRA — and none of them aborted anything"
    );

    // The ALGEBRA side of the same decision, and the half that matters most:
    // `root/1-alpha`'s own FOLD window exited, so its synthesis is replaced —
    // but its two children ALREADY ran and ALREADY folded, and discarding
    // their answers, their tree lines or their accounting here would erase
    // completed sibling work one level up.
    let alpha_line = run.tree_line("root/1-alpha");
    assert!(
        alpha_line.contains("fold failed"),
        "a node whose fold window exited must say so on its own line: {alpha_line}"
    );
    for child in [&alpha_one, &alpha_two] {
        assert!(
            run.branch_summary("root/1-alpha", child)
                .contains(LEAF_FINISH_TEXT),
            "the children HAD answered when their parent's fold window died — and, being \
             childless non-root nodes, they fold MECHANICALLY: {}",
            run.branch_summary("root/1-alpha", child)
        );
        assert!(
            run.tree_line(child).contains("finish(voluntary)"),
            "and their tree lines roll up UNTOUCHED past the failed fold: {}",
            run.tree_line(child)
        );
    }
    assert_eq!(
        run.counter("runWindows"),
        10,
        "and their accounting too: root and root/1-alpha each spend two windows \
         (coalgebra + algebra — a failed algebra window still SPENT one, and a \
         failed fold discards neither its children's nodes nor what they cost), \
         while the six childless non-root nodes each spend only their coalgebra — \
         a mechanical leaf fold runs no window at all"
    );
    let failures: Vec<(&str, &str, &str)> = run
        .journal_kind("failed")
        .iter()
        .map(|e| {
            (
                e.key.as_str(),
                e.payload
                    .get("window")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<none>"),
                e.payload
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<none>"),
            )
        })
        .collect();
    assert_eq!(
        failures.len(),
        3,
        "each failure is recorded at ITS OWN branch position, got {failures:?}"
    );
    assert!(
        failures
            .iter()
            .any(|(key, window, reason)| *key == "root/1-alpha"
                && *window == "algebra"
                && reason.starts_with("round exhaustion:")),
        "a failed FOLD is journaled distinctly from a failed coalgebra — a node can \
         carry both, and which window failed is the difference between 'this node \
         decided nothing' and 'this node could not fold what its children decided': \
         {failures:?}"
    );
    assert!(
        failures.iter().any(|(key, window, reason)| *key == beta
            && *window == "coalgebra"
            && reason.contains("no branches")),
        "the unusable-layer failure, tagged with the window that produced it: {failures:?}"
    );
    assert!(
        failures.iter().any(|(key, window, reason)| *key == epsilon
            && *window == "coalgebra"
            && reason.starts_with("round exhaustion:")
            && reason.contains("without finalizing")),
        "the abnormal-exit failure, tagged with the window that produced it: {failures:?}"
    );

    // --- row 9: GateOff raises no askUser suspension at all -----------------
    assert_eq!(
        run.gate_presentations, 0,
        "under GateOff no layer-approval form may be presented — an unattended \
         companion turn is a real configuration, not a suppressed prompt"
    );
    assert!(
        run.journal_kind("gate").is_empty(),
        "and nothing may be journaled as a gate that never happened"
    );

    // --- row 10: one journal entry per node event, keyed by NodePath -------
    row_10_journal_is_keyed_by_node_path(
        &run,
        &[
            ("turn", "root"),
            ("turn", "root"),
            // `proposed` is what each WINDOW said; `split`/`finish` is what
            // the driver did with it. Every node that reaches `discover`
            // emits one, so a policy that later refuses or reshapes a layer
            // cannot erase the model's own work from the record.
            ("proposed", "root"),
            ("proposed", "root/1-alpha"),
            ("proposed", &alpha_one),
            ("proposed", &alpha_two),
            ("proposed", &beta),
            ("proposed", &gamma),
            ("proposed", &delta),
            ("proposed", &epsilon),
            ("split", "root"),
            ("split", "root/1-alpha"),
            ("finish", &alpha_one),
            ("finish", &alpha_two),
            ("finish", &beta),
            ("finish", &gamma),
            ("finish", &delta),
            ("finish", &epsilon),
            ("fold", "root"),
            ("fold", "root/1-alpha"),
            ("fold", &alpha_one),
            ("fold", &alpha_two),
            ("fold", &beta),
            ("fold", &gamma),
            ("fold", &delta),
            ("fold", &epsilon),
            ("failed", &beta),
            ("failed", &epsilon),
            ("failed", "root/1-alpha"),
        ],
    );

    // --- row 11: node ids are containment-safe -----------------------------
    row_11_node_ids_are_containment_safe(&run, &HOSTILE_FRAGMENTS);
}

/// The first fenced Haskell block in `text` — how the hole card's rendered
/// answer-type shape is read back out of a recorded request.
fn fenced_haskell(text: &str) -> Option<&str> {
    const OPEN: &str = "```haskell\n";
    let start = text.find(OPEN)? + OPEN.len();
    let rest = &text[start..];
    let end = rest.find("```")?;
    Some(&rest[..end])
}

// ---------------------------------------------------------------------------
// Scenario B — the depth cap: §9 row 5
// ---------------------------------------------------------------------------

/// §9 row 5 — budget-forced finish (depth).
///
/// Its own compile because `maxDepth` is part of the `Config` the driver
/// splices into the cycle's fused `render`+`loop` compile: a different cap is
/// a different compile, and no amount of bundling avoids that.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn companion_depth_cap_forces_a_stamped_finish() {
    let _cache_guard = support::isolate_cache();

    let run = run_scenario(
        "depth",
        state_json(2, 40, 4, json!({"tag": "GateOff"})),
        vec![
            // The SAME two-branch reply at all three nodes that split — one
            // answerer compile, three windows.
            script(PathKey::Exact("root"), Phase::Discover, split_two()),
            script(PathKey::Exact("root/1-alpha"), Phase::Discover, split_two()),
            script(PathKey::Exact("root/2-beta"), Phase::Discover, split_two()),
            fold_script(),
        ],
        Arc::new(ScriptedGate::default()),
    )
    .await;

    let capped = [
        "root/1-alpha/1-alpha",
        "root/1-alpha/2-beta",
        "root/2-beta/1-alpha",
        "root/2-beta/2-beta",
    ];
    let discovered = run.paths_in(Phase::Discover);
    assert_eq!(
        discovered,
        vec!["root", "root/1-alpha", "root/2-beta"],
        "the depth cap refuses BEFORE the window runs — a capped node must cost no \
         coalgebra window at all"
    );

    // Every child brief states its own numeric budget — depth against the
    // cap, its own remaining allowance, and the fan-out cap — so a window
    // can see its own room to fork instead of it staying implicit.
    let root_request = run.request_for("root", Phase::Discover);
    assert!(
        root_request.contains("Budget: depth 0 of 2 max, node allowance 40, fan-out cap 4."),
        "the root's own brief must state its numeric budget in full: {root_request}"
    );
    let alpha_request = run.request_for("root/1-alpha", Phase::Discover);
    assert!(
        alpha_request.contains("Budget: depth 1 of 2 max,")
            && alpha_request.contains("fan-out cap 4."),
        "a descendant's brief must state its OWN depth against the same cap: {alpha_request}"
    );

    for path in capped {
        let line = run.tree_line(path);
        assert!(
            line.contains("forced: depth cap 2/2 reached"),
            "a depth-capped node's finish must be STAMPED as budget-forced, NAMING \
             the depth cap and the governing numbers, not left indistinguishable \
             from a model's own choice: {line}"
        );
    }
    assert_eq!(
        run.counter("runForced"),
        4,
        "the receipt counts every budget-forced finish"
    );
    assert_eq!(run.counter("runNodes"), 7);
    assert_eq!(
        run.counter("runWindows"),
        6,
        "three nodes (root and the two splitting children) spend two windows each \
         (coalgebra + algebra); the four depth-capped nodes are childless non-root \
         leaves that never even reach discover, and — under mechanical-leaf-fold \
         semantics — never run a fold window either, so they cost none at all"
    );
    // A budget-refused node never reaches `discover`, so it has no window to
    // have SAID anything — no `proposed` entry. But the driver still decided
    // something for it, so it does journal a `finish`: that is exactly the
    // split the two kinds exist to make. Reading `proposed` tells you which
    // nodes cost a model call; reading `finish` tells you how each node ended.
    let mut proposed_keys: Vec<&str> = run
        .journal_kind("proposed")
        .iter()
        .map(|e| e.key.as_str())
        .collect();
    proposed_keys.sort_unstable();
    for path in capped {
        assert!(
            !proposed_keys.contains(&path),
            "a node refused before its coalgebra ran must journal no proposal: {path}"
        );
    }
    let mut finish_keys: Vec<&str> = run
        .journal_kind("finish")
        .iter()
        .map(|e| e.key.as_str())
        .collect();
    finish_keys.sort_unstable();
    let mut expected_finishes: Vec<&str> = capped.to_vec();
    expected_finishes.sort_unstable();
    assert_eq!(
        finish_keys, expected_finishes,
        "every budget-forced finish is journaled as what the driver did, and only \
         the capped nodes finished at all"
    );
    for path in capped {
        assert!(
            run.journal_pairs()
                .contains(&("fold".to_string(), path.to_string())),
            "a capped node is still folded, at its own branch position: {path}"
        );
    }
}

// ---------------------------------------------------------------------------
// Scenario C — the node-count cap: §9 row 6
// ---------------------------------------------------------------------------

/// §9 row 6 — the node-count cap.
///
/// The cap is carried STRUCTURALLY on the seed (`seedAllowance`, divided among
/// children by `childAllowance`) rather than by `Tidepool.Thought.nodeCapped`,
/// whose `MonadState Int` the outer row is not — so what it bounds is the
/// number of nodes that RUN, and `maxNodes = 4` against a root splitting three
/// ways is the exactly-saturating case: root(4) reserves one and divides 3
/// among three children, each of which spends its one and can fund nobody.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn companion_node_cap_bounds_the_windows_that_run() {
    let _cache_guard = support::isolate_cache();

    let run = run_scenario(
        "nodes",
        state_json(5, 4, 4, json!({"tag": "GateOff"})),
        vec![
            script(PathKey::Exact("root"), Phase::Discover, split_three()),
            script(PathKey::Exact("root/1-alpha"), Phase::Discover, split_two()),
            script(
                PathKey::Exact("root/2-beta"),
                Phase::Discover,
                finish_reply(),
            ),
            script(
                PathKey::Exact("root/3-gamma"),
                Phase::Discover,
                finish_reply(),
            ),
            fold_script(),
        ],
        Arc::new(ScriptedGate::default()),
    )
    .await;

    let discovered = run.paths_in(Phase::Discover);
    assert_eq!(
        discovered,
        vec!["root", "root/1-alpha", "root/2-beta", "root/3-gamma"],
        "exactly `maxNodes` (4) nodes may run a coalgebra window"
    );
    for path in ["root/1-alpha/1-alpha", "root/1-alpha/2-beta"] {
        let line = run.tree_line(path);
        assert!(
            line.contains("forced: node allowance exhausted (0/4)"),
            "an overflow branch's finish must be stamped with the NAMED allowance \
             reason and the numbers that governed it: {line}"
        );
    }
    assert_eq!(run.counter("runForced"), 2);
    assert_eq!(
        run.counter("runWindows"),
        6,
        "root and root/1-alpha (each with kids) spend two windows apiece; \
         root/2-beta and root/3-gamma are childless non-root leaves that fold \
         mechanically, spending only their coalgebra; and the two unfunded, \
         node-count-capped children never reach discover and — being childless \
         non-root leaves too — never run a fold window either, so they spend none"
    );
}

// ---------------------------------------------------------------------------
// Scenario D — the fan-out cap: §9 row 7
// ---------------------------------------------------------------------------

/// §9 row 7 — the fan-out cap refuses the DESCENT, not the coalgebra's own
/// work.
///
/// Fan-out is a property of the produced layer, so the root's window has
/// already run when the cap decides — which is exactly why the gate sits
/// OUTSIDE the cap (§6): the operator is never shown a layer a budget already
/// discarded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn companion_fanout_cap_refuses_the_descent() {
    let _cache_guard = support::isolate_cache();

    let run = run_scenario(
        "fanout",
        state_json(5, 40, 2, json!({"tag": "GateOff"})),
        vec![
            script(PathKey::Exact("root"), Phase::Discover, split_three()),
            fold_script(),
        ],
        Arc::new(ScriptedGate::default()),
    )
    .await;

    assert_eq!(
        run.paths_in(Phase::Discover),
        vec!["root"],
        "NO child window may run once the fan-out cap refuses the layer"
    );
    let root_line = run.tree_line("root");
    assert!(
        root_line.contains("forced: fan-out cap 2 exceeded (3 branches proposed)"),
        "the node finishes with a NAMED fan-out-cap reason and the numbers that \
         governed it: {root_line}"
    );
    assert_eq!(run.counter("runNodes"), 1);
    assert_eq!(run.counter("runForced"), 1);
    assert_eq!(
        run.counter("runWindows"),
        2,
        "the coalgebra HAD run before the cap decided, so this node spent both windows"
    );
    // Two different facts, two different kinds, and the split between them is
    // what makes the journal safe to read back. `proposed` keeps the window's
    // own work — a layer naming three branches — because a refused proposal is
    // exactly what the friction log wants. `split` records what the driver
    // actually descended through, and a capped node descended through nothing,
    // so it emits `finish` instead. §10.1 promises a `split` entry's child
    // paths ARE the durable record of the tree's shape; emitting one here
    // would name three children that never ran and hand a future resume fold
    // a tree the run never had.
    assert_eq!(
        run.journal_kind("proposed")
            .iter()
            .map(|e| e.key.as_str())
            .collect::<Vec<_>>(),
        vec!["root"],
        "the refused proposal is still recorded as what the window said"
    );
    assert!(
        run.journal_kind("split").is_empty(),
        "a layer the fan-out cap refused was never descended through, so nothing \
         may be journaled as a split naming children that never ran"
    );
    assert_eq!(
        run.journal_kind("finish")
            .iter()
            .map(|e| e.key.as_str())
            .collect::<Vec<_>>(),
        vec!["root"],
        "what the driver actually did is a forced finish"
    );
}

// ---------------------------------------------------------------------------
// Scenario E — the gate: §9 rows 8 and 8b (ONE config, two runs)
// ---------------------------------------------------------------------------

fn gate_every_layer_state() -> Json {
    state_json(3, 40, 4, json!({"tag": "GateEveryLayer"}))
}

/// §9 row 8 — the gate is exercised through the form API, and a pruned
/// branch's window never runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn companion_gate_prune_then_approve_never_runs_the_pruned_branch() {
    let _cache_guard = support::isolate_cache();

    let gate = Arc::new(ScriptedGate::new(vec![
        verdict("Prune", "Beta", ""),
        verdict("Approve", "", ""),
    ]));
    let run = run_scenario(
        "gate-prune",
        gate_every_layer_state(),
        vec![
            script(PathKey::Exact("root"), Phase::Discover, split_three()),
            script(
                PathKey::Exact("root/1-alpha"),
                Phase::Discover,
                finish_reply(),
            ),
            script(
                PathKey::Exact("root/2-gamma"),
                Phase::Discover,
                finish_reply(),
            ),
            fold_script(),
        ],
        gate.clone(),
    )
    .await;

    assert_eq!(
        run.gate_presentations, 2,
        "a non-Approve verdict re-presents the amended layer; Approve ends the round"
    );
    assert_eq!(
        run.gate_submissions.len(),
        2,
        "both verdicts crossed the real form API"
    );
    let discovered = run.paths_in(Phase::Discover);
    assert_eq!(
        discovered,
        vec!["root", "root/1-alpha", "root/2-gamma"],
        "the PRUNED branch's window must never run, and the survivors' must — and \
         the survivor that was branch 3 is now branch 2: every accepted verdict \
         re-derives the surviving branches' paths from their NEW positions, so ids \
         stay dense and ordered"
    );
    assert!(
        !run.requests
            .iter()
            .any(|r| r.contains("Your branch: Beta (")),
        "no window may be prompted as the pruned branch"
    );
    assert_eq!(
        run.journal_kind("gate")
            .iter()
            .map(|e| e.key.as_str())
            .collect::<Vec<_>>(),
        vec!["root", "root"],
        "every presentation is journaled at the node it gated"
    );
    assert_eq!(run.counter("runNodes"), 3);
}

/// §9 row 8b — an amended branch is WORKED as amended.
///
/// THE regression this row exists for: a branch carries its `ForkBrief` TWICE
/// — on the `Branch` (what the render and every receipt read) and inside the
/// SEED (the only thing that node's own window is prompted from, since a
/// coalgebra receives only the seed). An `Amend` that wrote one and not the
/// other renders right and works wrong. So this asserts the amended branch's
/// own coalgebra PROMPT; asserting the render would pass against the bug.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn companion_gate_amend_reaches_the_branches_own_prompt() {
    let _cache_guard = support::isolate_cache();

    const AMENDED: &str = "the instruction the operator substituted";

    let gate = Arc::new(ScriptedGate::new(vec![
        verdict("Amend", "Alpha", AMENDED),
        verdict("Approve", "", ""),
    ]));
    let run = run_scenario(
        "gate-amend",
        gate_every_layer_state(),
        vec![
            script(PathKey::Exact("root"), Phase::Discover, split_two()),
            script(
                PathKey::Exact("root/1-alpha"),
                Phase::Discover,
                finish_reply(),
            ),
            script(
                PathKey::Exact("root/2-beta"),
                Phase::Discover,
                finish_reply(),
            ),
            fold_script(),
        ],
        gate.clone(),
    )
    .await;

    assert_eq!(run.gate_presentations, 2);
    let amended_prompt = run.window_prompt("root/1-alpha", Phase::Discover);
    assert!(
        amended_prompt.contains(AMENDED),
        "the amended branch's OWN window must be prompted with the operator's \
         instruction, got:\n{amended_prompt}"
    );
    assert!(
        !amended_prompt.contains(ALPHA_INSTRUCTION),
        "and never with the one it replaced — a brief written on the Branch but not \
         in the seed renders right and works wrong:\n{amended_prompt}"
    );
    let untouched = run.window_prompt("root/2-beta", Phase::Discover);
    assert!(
        untouched.contains("the instruction the model proposed for beta"),
        "a sibling the verdict did not name keeps its own brief: {untouched}"
    );
}

/// `Add`'s sibling case to row 8b above, and the one the `ChildEdge`
/// constructor (`Harness.hs`) exists to rule out structurally: an ADDED
/// branch carries its `Th.ForkBrief` on the `Th.Branch` the render reads AND
/// inside its OWN seed, the only thing that branch's own coalgebra window is
/// prompted from. A hand-copied seed (the pre-fix `Add` arm cloned a
/// sibling's whole `NodeSeed`) could carry the wrong brief there even while
/// the render looks right. Asserting the render would pass against that bug;
/// this asserts the added branch's own PROMPT.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn companion_gate_add_reaches_the_added_branchs_own_prompt() {
    let _cache_guard = support::isolate_cache();

    const ADDED_INSTRUCTION: &str = "the instruction the operator added for gamma";

    let gate = Arc::new(ScriptedGate::new(vec![
        add_verdict("Gamma", "Primary", ADDED_INSTRUCTION),
        verdict("Approve", "", ""),
    ]));
    let run = run_scenario(
        "gate-add",
        gate_every_layer_state(),
        vec![
            script(PathKey::Exact("root"), Phase::Discover, split_two()),
            script(
                PathKey::Exact("root/1-alpha"),
                Phase::Discover,
                finish_reply(),
            ),
            script(
                PathKey::Exact("root/2-beta"),
                Phase::Discover,
                finish_reply(),
            ),
            script(PathKey::Prefix("root/3-"), Phase::Discover, finish_reply()),
            fold_script(),
        ],
        gate.clone(),
    )
    .await;

    assert_eq!(
        run.gate_presentations, 2,
        "the Add-amended layer re-presents once before Approve ends the round"
    );
    let added = run.discovered_under("root/3-");
    let added_prompt = run.window_prompt(&added, Phase::Discover);
    assert!(
        added_prompt.contains(ADDED_INSTRUCTION),
        "the added branch's OWN window must be prompted with the operator's \
         instruction, got:\n{added_prompt}"
    );
    assert_eq!(
        run.paths_in(Phase::Discover),
        vec!["root", "root/1-alpha", "root/2-beta", added.as_str()],
        "the added branch's own window must run, alongside both original siblings'"
    );
}

// ---------------------------------------------------------------------------
// Scenario F — the fold-lineage rewrite: checkpoint compatibility, and the
// unified walk's own new behavior (a fold is a BRANCH carrying inherited
// ancestry; folds interleave with a cousin's still-running descent).
//
// The PRD 21 lane C4 checked-edits scenario this section used to hold (a
// leaf proposes at its own fold, root selects and applies against an
// in-heap working draft, with a corrective retry for an invalid selection)
// is retired along with the machinery it exercised: `State.draft`,
// `FoldDecision`'s `foldEditsInOrder`/`foldProposed` fields, and
// `Tidepool.Thought`'s whole `Artifact`/`EditPlan` vocabulary are gone —
// the worktree/delegate channel (`Harness.mergeFold`) is the file-edit
// mechanism now, and the in-heap draft was only ever the interim stand-in
// for it.
// ---------------------------------------------------------------------------

/// A checkpoint written before the fold-lineage rewrite still carries
/// `State`'s own now-retired `draft` key. It must still boot: the vendored
/// generic `FromJSON` decode for a record looks up ONLY the fields the
/// record type itself declares (`o .: fieldName`, `HarnessTypes.hs`'s own
/// `FromJSON Config` doc explains the same discipline for `Config`), so an
/// object carrying an extra, undeclared key simply has that key never
/// looked at — never a decode refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn companion_old_checkpoint_with_stale_draft_key_still_boots() {
    let _cache_guard = support::isolate_cache();

    let run = run_scenario(
        "stale-draft-key",
        state_json_with_stale_draft_key(3, 40, 5, json!({"tag": "GateOff"})),
        vec![
            script(PathKey::Exact("root"), Phase::Discover, finish_reply()),
            fold_script(),
        ],
        Arc::new(ScriptedGate::default()),
    )
    .await;

    assert_eq!(
        run.paths_in(Phase::Discover),
        vec!["root"],
        "a checkpoint carrying the retired `draft` key still boots and runs a real turn"
    );
    assert!(
        run.state.get("draft").is_none(),
        "the re-encoded State carries no draft field at all -- the stale key never round-trips, \
         because it was never part of the type: {}",
        run.state
    );
}

/// The two new behaviors the unified recursive walk adds, off the SAME tree
/// shape (root splits into Alpha and Beta, each of which mints exactly one
/// plain grandchild of its own via `split_one()`, so both are INTERIOR
/// nodes and both actually open a fold window) and the SAME config as the
/// "tree" scenario, so this shares that compile.
///
/// (1) STRUCTURAL: a fold's own request is a BRANCH carrying INHERITED
/// ancestry, never a fresh fork off an empty root. `Event::BranchInvocation`
/// is written ONLY for a node minted through `fork_from_snapshot` (never
/// for the pre-rewrite `runLLMTurnFork`'s empty-root fork), so its presence
/// on root's own FOLD window — naming the SAME digest and shared-prefix
/// byte count as root's own DISCOVER window's `SnapshotFrozen` receipt — is
/// exactly the proof: the fold branches off the identical frozen prefix the
/// node's own coalgebra window already froze, i.e. a continuation of its
/// own conversation.
///
/// (2) INTERLEAVING: Alpha's own subtree — its one grandchild's discover,
/// then Alpha's own fold — completes (and is journaled) BEFORE Beta's own
/// grandchild is even discovered. The pre-rewrite two-pass design could
/// never produce this order (it discovered the WHOLE tree, breadth-first
/// per level, before folding any of it); the unified walk recurses AND
/// folds each sibling in turn, depth-first. The assembled RESULT does not
/// depend on this: the final tree still lists every node in DECLARED
/// order, and every count is exactly what the shape predicts — proving the
/// reordering of WHEN things run never corrupts WHAT gets assembled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn companion_fold_is_a_branch_and_interleaves_with_cousin_discovery() {
    let _cache_guard = support::isolate_cache();

    let run = run_scenario(
        "fold-branch-interleave",
        state_json(3, 40, 5, json!({"tag": "GateOff"})),
        vec![
            script(PathKey::Exact("root"), Phase::Discover, split_two()),
            script(PathKey::Exact("root/1-alpha"), Phase::Discover, split_one()),
            script(PathKey::Exact("root/2-beta"), Phase::Discover, split_one()),
            script(
                PathKey::Prefix("root/1-alpha/1-"),
                Phase::Discover,
                finish_reply(),
            ),
            script(
                PathKey::Prefix("root/2-beta/1-"),
                Phase::Discover,
                finish_reply(),
            ),
            fold_script(),
        ],
        Arc::new(ScriptedGate::default()),
    )
    .await;

    let alpha_grandchild = run.discovered_under("root/1-alpha/1-");
    let beta_grandchild = run.discovered_under("root/2-beta/1-");

    // --- pin 1: the fold is a BRANCH, off THIS node's own frozen prefix ----
    let root_discover_node = run.window_node("root", Phase::Discover);
    let (root_digest, root_prefix_bytes) = run.frozen_of(root_discover_node);
    let root_fold_node = run.window_node("root", Phase::Fold);
    let (fold_snapshot, fold_shared, fold_suffix) = run.branch_invocation_of(root_fold_node);
    assert_eq!(
        fold_snapshot, root_digest,
        "root's own FOLD must branch off the SAME frozen prefix its own DISCOVER window froze \
         -- a continuation of its own conversation, never a fresh fork off an empty root"
    );
    assert_eq!(
        fold_shared, root_prefix_bytes,
        "the fold's shared-prefix byte count must equal root's own frozen prefix's own -- the \
         harness re-derives this itself before writing the receipt, so equality here is the \
         byte-stability proof"
    );
    assert!(
        fold_suffix > 0,
        "the fold's own prompt is a real divergent suffix past the shared (inherited) prefix, \
         never empty"
    );

    // --- pin 2: a completed subtree's fold precedes a cousin's discovery ---
    let pairs = run.journal_pairs();
    let index_of = |kind: &str, key: &str| {
        pairs
            .iter()
            .position(|(k, p)| k == kind && p == key)
            .unwrap_or_else(|| panic!("no {kind} journaled for {key}: {pairs:?}"))
    };
    let alpha_fold_idx = index_of("fold", "root/1-alpha");
    let beta_grandchild_discover_idx = index_of("proposed", &beta_grandchild);
    assert!(
        alpha_fold_idx < beta_grandchild_discover_idx,
        "alpha's own subtree (its grandchild's discover then fold, then alpha's own fold) must \
         complete before beta's grandchild is even discovered -- the pre-rewrite two-pass design \
         discovered the WHOLE tree before folding any of it; this walk folds a subtree the \
         moment it completes, interleaved with a cousin's still-running descent:\n{pairs:#?}"
    );

    // --- the assembled result is correct regardless of that reordering -----
    let tree_paths: Vec<String> = run
        .tree()
        .iter()
        .map(|l| l.split_whitespace().next().unwrap().to_string())
        .collect();
    assert_eq!(
        tree_paths,
        vec![
            "root".to_string(),
            "root/1-alpha".to_string(),
            alpha_grandchild.clone(),
            "root/2-beta".to_string(),
            beta_grandchild.clone(),
        ],
        "the final tree is still assembled in DECLARED branch order, regardless of which \
         subtree's fold actually ran first: {:?}",
        run.tree()
    );
    assert_eq!(run.counter("runNodes"), 5);
    // root, alpha and beta each have kids, so none is a mechanical leaf: each
    // spends TWO windows (coalgebra + fold). The two grandchildren are
    // childless non-root leaves that fold mechanically, spending only their
    // OWN coalgebra — one window each. (Forgetting the grandchildren's own
    // windows here is exactly the arithmetic slip this derivation now rules
    // out.)
    let non_mechanical_windows = 3 * 2; // root, alpha, beta: coalgebra + fold
    let mechanical_leaf_windows = 2; // alpha's and beta's own grandchild: coalgebra only
    assert_eq!(
        run.counter("runWindows"),
        non_mechanical_windows + mechanical_leaf_windows,
        "root, alpha and beta each spend two windows (coalgebra + fold); the two grandchildren \
         are childless non-root leaves that fold mechanically, spending one window (their own \
         coalgebra) each"
    );
}
