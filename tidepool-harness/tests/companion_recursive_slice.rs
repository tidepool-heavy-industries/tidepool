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
//! served by [`KeyedProvider`] (below) and every gate presentation by
//! [`ScriptedGate`]; the only substrate that runs for real is the JIT, the
//! driver, and GHC.
//!
//! # How a scenario is scripted
//!
//! Two knobs, and between them they cover §9's whole matrix:
//!
//! - **The provider table.** Every window's prompt embeds its node's
//!   `NodePath` (`NODE <path> — DISCOVER` / `NODE <path> — FOLD`), so a
//!   scenario is a table of (path-needle, finalize reply) pairs and the whole
//!   tree is deterministic. [`KeyedProvider`] is `outer_fanout.rs`'s
//!   needle-matched provider, adapted: an entry carries a needle SET (all of
//!   which must appear) rather than one needle, because a node id derived from
//!   a model-produced branch TITLE (§3's slug — row 11) is not a string this
//!   file should be hardcoding; `["NODE root/2-", "— DISCOVER"]` names the
//!   branch by POSITION and phase and lets the harness own the slug.
//!   `ReplayProvider` cannot serve any of this: its queue is strictly FIFO,
//!   and the order in which windows reach the provider is itself under test.
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
//! One SCENARIO CONFIG is one compile shape: the driver splices the restored
//! `State` JSON into the fused `render`+`loop` compile, so two runs sharing a
//! config share that compile (the memo makes the second free), and two runs
//! differing only in their provider table or gate script cost nothing extra.
//! Each scenario below therefore asserts as many §9 rows as its config can
//! carry, and the two gate rows (8, 8b) share ONE config between two runs.
//! Answerer-side compiles are shared the same way: every leaf reuses ONE
//! `ProposeFinish` reply and every fold reuses ONE `FoldDecision` reply across
//! all six runs, so those blocks compile once for the whole file.
//!
//! Four configs is the floor, not a preference: the depth cap, the node cap
//! and the fan-out cap are three DIFFERENT `Config` values, and a run cannot
//! hold two of them without confounding which cap fired.
//!
//! GHC-heavy: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH
//! (`--ignore-default-filter` to run). Budget the wall time — the six
//! scenarios run ~7 minutes with a WARM compile memo, and meaningfully longer
//! cold. What remains warm is per-window JIT compilation, which nothing
//! memoizes.

mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value as Json};

use tidepool_handlers::{load_journal, ConsoleHandler, JournalEntry, JournalHandler, SegmentPath};
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Event as LogEvent, LogHeader, LogReader, LogWriter};
use tidepool_harness::provider::{
    DynModelProvider, ModelProvider, ProviderError, StreamSink, TurnRequest, TurnResponse, Usage,
};
use tidepool_harness::selfharness::operator::FormShape;
use tidepool_harness::selfharness::persistence;
use tidepool_harness::tree::NodeId;
use tidepool_harness::{
    answerer_decls, load_harness_source, ContinueSignal, Event as DriverEvent, Harness, Observer,
    OperatorGate, SelfHarnessDriver,
};

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
// The scripted provider — needle SETS, and a record of what it was asked
// ---------------------------------------------------------------------------

/// One scripted window: every needle in `needles` must appear in the request's
/// last message for `reply` to be served.
struct Script {
    needles: Vec<&'static str>,
    reply: String,
}

fn script(needles: &[&'static str], reply: String) -> Script {
    Script {
        needles: needles.to_vec(),
        reply,
    }
}

/// A [`ModelProvider`] that answers each cognition window by matching a set of
/// NEEDLES against the request's last message (the hole card embeds the
/// window's own prompt verbatim, and every prompt this harness writes names
/// its node's `NodePath`), never by call ORDER.
///
/// Adapted from `outer_fanout.rs`'s provider of the same name, for the reason
/// its doc gives — `ReplayProvider`'s strict FIFO queue cannot serve windows
/// whose provider-call order is itself under test — plus one change: an entry
/// matches on ALL of its needles, so a scenario can name a branch by POSITION
/// (`"NODE root/2-"`) and PHASE (`"— DISCOVER"`) instead of hardcoding a slug
/// the harness derives from a model-produced title.
///
/// Entries are matched IN ORDER, first match wins, so a catch-all
/// (`["— FOLD"]`) can sit last. A request nothing matches is a loud
/// `ProviderError`, never a default reply: "a window that must not run, ran"
/// has to fail the run rather than be quietly served.
struct KeyedProvider {
    scripted: Vec<Script>,
    /// The prompt of every window the provider was asked to answer, in order
    /// — the record several assertions below read (which windows ran at all,
    /// and what a window was actually prompted with).
    seen: Mutex<Vec<String>>,
}

/// The message a request is MATCHED against: the last one carrying a window
/// prompt header, not simply the last message.
///
/// Two things make "the last message" wrong here, and both are real:
/// a branch child's request opens with its PARENT's frozen transcript (which
/// contains the parent's own prompt header), so matching must look at the
/// LAST such header, not the first; and a window that burns its round budget
/// is re-prompted with the driver's round-cap ultimatum, which carries no
/// header at all — a starved window (row 4b) would otherwise stop matching
/// its own scripted entry halfway through being starved.
fn window_message(req: &TurnRequest) -> String {
    req.messages
        .iter()
        .rev()
        .find(|m| m.content.contains(" — DISCOVER") || m.content.contains(" — FOLD"))
        .or_else(|| req.messages.last())
        .map(|m| m.content.clone())
        .unwrap_or_default()
}

impl KeyedProvider {
    fn new(scripted: Vec<Script>) -> Self {
        KeyedProvider {
            scripted,
            seen: Mutex::new(Vec::new()),
        }
    }
}

impl ModelProvider for KeyedProvider {
    async fn complete(
        &self,
        req: TurnRequest,
        _sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        let last = window_message(&req);
        self.seen.lock().unwrap().push(last.clone());

        let reply = self
            .scripted
            .iter()
            .find(|s| s.needles.iter().all(|n| last.contains(n)))
            .map(|s| s.reply.clone())
            .ok_or_else(|| {
                ProviderError::Api(format!(
                    "KeyedProvider: no scripted reply matches the request:\n{last}"
                ))
            })?;

        Ok(TurnResponse {
            text: reply,
            usage: Usage {
                input_tokens: 50,
                output_tokens: 10,
                cached_input_tokens: None,
            },
            reasoning: None,
            reasoning_items: Vec::new(),
        })
    }
}

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
        self.presented.lock().unwrap().len()
    }
}

impl OperatorGate for ScriptedGate {
    fn present_form(&self, shape: &FormShape) -> Json {
        self.presented.lock().unwrap().push(shape.clone());
        let mut queue = self.submissions.lock().unwrap();
        if queue.is_empty() {
            json!({})
        } else {
            queue.remove(0)
        }
    }

    fn await_continue(&self) -> ContinueSignal {
        ContinueSignal::Continue
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
/// `service_outer_fanout` emit `RunLLMTurnHole{prompt}` and then
/// `TurnStart{node}` for the window they just minted, and this harness's
/// windows are sequential by construction (`runLLMTurnBranch` and a
/// single-child `runLLMTurnFork`, each its own suspend/resume round-trip).
/// `take()` on the pending prompt makes it robust to extra `TurnStart`s.
///
/// This is what lets a `BranchInvocation` receipt — which carries a `NodeId`
/// and no path — be attributed to the node whose window it belongs to,
/// without inferring anything from event ORDER.
#[derive(Default)]
struct WindowObserver {
    pending: Mutex<Option<String>>,
    windows: Mutex<Vec<(String, NodeId)>>,
    forms: Mutex<Vec<Json>>,
}

impl Observer for WindowObserver {
    fn on_event(&self, event: &DriverEvent) {
        match event {
            DriverEvent::RunLLMTurnHole { prompt, .. } => {
                *self.pending.lock().unwrap() = Some(prompt.clone());
            }
            DriverEvent::TurnStart { node } => {
                if let Some(prompt) = self.pending.lock().unwrap().take() {
                    self.windows.lock().unwrap().push((prompt, *node));
                }
            }
            DriverEvent::FormSubmitted { submission, .. } => {
                self.forms.lock().unwrap().push(submission.clone());
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// One scenario run
// ---------------------------------------------------------------------------

/// Which window a prompt belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Discover,
    Fold,
}

/// `NODE <path> — DISCOVER (…` / `NODE <path> — FOLD (…` → `(path, phase)`.
/// The ONE place this file parses a prompt, so the coupling to
/// `Harness.hs`'s prompt headers is a single line rather than scattered.
fn parse_window(prompt: &str) -> Option<(String, Phase)> {
    let rest = prompt.strip_prefix("NODE ")?;
    let (path, tail) = rest.split_once(" — ")?;
    let phase = if tail.starts_with("DISCOVER") {
        Phase::Discover
    } else if tail.starts_with("FOLD") {
        Phase::Fold
    } else {
        return None;
    };
    Some((path.to_string(), phase))
}

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
    state_json_with_draft(max_depth, max_nodes, max_fan_out, gate_policy, "")
}

/// As [`state_json`], with an explicit starting `draft` (PRD 21 lane C4 — the
/// companion's working draft, seeded at whatever the scenario's checked-edits
/// scripted flow needs to start from; every other scenario starts from `""`,
/// exactly today's pre-C4 behavior).
fn state_json_with_draft(
    max_depth: i64,
    max_nodes: i64,
    max_fan_out: i64,
    gate_policy: Json,
    draft: &str,
) -> Json {
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
        "draft": draft,
    })
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

    let windows = observer.windows.lock().unwrap().clone();
    let gate_submissions = observer.forms.lock().unwrap().clone();
    let requests = provider.seen.lock().unwrap().clone();
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
fn split_reply(
    posture: &str,
    strategy: &str,
    focus: &str,
    branches: &[(&str, &str, &str)],
) -> String {
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
         \"{focus}\", splitStrategy = {strategy}, splitBranches = [{}] }})",
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
/// six near-identical ones would be six. The needle table is what varies per
/// scenario; the reply does not have to.
fn split_two() -> String {
    split_reply(
        "Explore",
        "WantSequential",
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
        "WantSequential",
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

/// The ONE leaf reply every scenario's leaves share — one answerer compile for
/// the whole file.
fn finish_reply() -> String {
    haskell(
        "finalize @LayerProposal (ProposeFinish { finishDraft = \"this node answers locally\" })",
    )
}

/// A structurally UNUSABLE layer: a split declaring no branches. §2's
/// `layerFromProposal` turns it into `Finish (Draft _ (InvocationFailed _))`,
/// which the algebra folds as ordinary data.
fn empty_split_reply() -> String {
    split_reply("Explore", "WantSequential", "nothing usable", &[])
}

/// The ONE fold reply every node's algebra window shares: a plain narrative
/// `FoldDecision` (PRD 21 lane C4) that selects and proposes nothing — the
/// `foldSelected`/`foldComposition`/`foldProposed` defaults every scenario
/// below that never exercises checked edits relies on to stay
/// byte-behaviorally identical to the old `FoldProposal`.
fn fold_reply() -> String {
    haskell(
        "finalize @FoldDecision (FoldDecision { foldSynthesis = \"FOLDED\", \
         foldTensions = [\"one unresolved tension\"], foldSelected = [], \
         foldComposition = [], foldProposed = [] })",
    )
}

/// The catch-all fold entry, matched last.
fn fold_script() -> Script {
    script(&["— FOLD"], fold_reply())
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
                &["NODE root — DISCOVER"],
                split_reply(
                    "Compare",
                    "WantConcurrent",
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
            script(&["NODE root/1-alpha — DISCOVER"], split_two()),
            script(&["NODE root/1-alpha/1-", "— DISCOVER"], finish_reply()),
            script(&["NODE root/1-alpha/2-", "— DISCOVER"], finish_reply()),
            // The hostile-titled branch is ALSO the unusable-layer branch
            // (row 4): a node whose own layer fails still has a node id, so
            // one branch carries both properties.
            script(&["NODE root/2-", "— DISCOVER"], empty_split_reply()),
            script(&["NODE root/3-", "— DISCOVER"], finish_reply()),
            script(&["NODE root/4-", "— DISCOVER"], finish_reply()),
            // Row 4b: this window is STARVED — it burns its round budget
            // without ever running a block, so its coalgebra comes back as a
            // typed `Left InvocationExit`.
            script(&["NODE root/5-", "— DISCOVER"], starved_reply()),
            // The ALGEBRA side of the same contract: this node's own FOLD
            // window is starved, so the exit replaces what that node owed and
            // must leave its two children's finished answers alone.
            script(&["NODE root/1-alpha — FOLD"], starved_reply()),
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
    let discovered = run.paths_in(Phase::Discover);
    let branches = run.branch_invocations();
    assert_eq!(
        branches.len(),
        discovered.len(),
        "every coalgebra window must be a REAL fork off a frozen prefix — one \
         BranchInvocation each, and none for the algebra's own (empty-root) fork \
         windows. Discovered: {discovered:?}"
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
            run.branch_summary("root", sibling).contains("FOLDED"),
            "branch {sibling}'s own answer must still reach its parent's fold: {}",
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
            run.branch_summary("root/1-alpha", child).contains("FOLDED"),
            "the children HAD answered when their parent's fold window died: {}",
            run.branch_summary("root/1-alpha", child)
        );
        assert!(
            run.tree_line(child).contains("finish(model)"),
            "and their tree lines roll up UNTOUCHED past the failed fold: {}",
            run.tree_line(child)
        );
    }
    assert_eq!(
        run.counter("runWindows"),
        16,
        "and their accounting too: eight nodes, two windows each — a failed window \
         still SPENT one, and a failed fold discards neither its children's nodes nor \
         what they cost"
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

    // The explicit Strategy transformation (§7, locked decision 9): the root
    // proposed `WantConcurrent` and the driver runs Sequential, shown
    // transformed rather than silently downgraded.
    assert!(
        run.tree_line("root")
            .contains("strategy: proposed Concurrent, executed Sequential"),
        "a transformed strategy must be stamped on the node, got: {}",
        run.tree_line("root")
    );
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
            script(&["NODE root — DISCOVER"], split_two()),
            script(&["NODE root/1-alpha — DISCOVER"], split_two()),
            script(&["NODE root/2-beta — DISCOVER"], split_two()),
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
    for path in capped {
        let line = run.tree_line(path);
        assert!(
            line.contains("forced ForcedDepth"),
            "a depth-capped node's finish must be STAMPED as budget-forced in the \
             render, not left indistinguishable from a model's own choice: {line}"
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
        10,
        "three nodes spent two windows each (coalgebra + algebra) and four spent only \
         their algebra — the cap is never itself the reason a window is spent"
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
            script(&["NODE root — DISCOVER"], split_three()),
            script(&["NODE root/1-alpha — DISCOVER"], split_two()),
            script(&["NODE root/2-beta — DISCOVER"], finish_reply()),
            script(&["NODE root/3-gamma — DISCOVER"], finish_reply()),
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
            line.contains("forced ForcedNodeCount"),
            "an overflow branch's finish must be stamped ForcedNodeCount: {line}"
        );
    }
    assert_eq!(run.counter("runForced"), 2);
    assert_eq!(
        run.counter("runWindows"),
        10,
        "four nodes spent two windows each; the two unfunded ones spent only their fold"
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
            script(&["NODE root — DISCOVER"], split_three()),
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
        root_line.contains("forced ForcedFanOut"),
        "the node finishes with a ForcedFanOut stamp: {root_line}"
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
            script(&["NODE root — DISCOVER"], split_three()),
            script(&["NODE root/1-alpha — DISCOVER"], finish_reply()),
            script(&["NODE root/2-gamma — DISCOVER"], finish_reply()),
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
            script(&["NODE root — DISCOVER"], split_two()),
            script(&["NODE root/1-alpha — DISCOVER"], finish_reply()),
            script(&["NODE root/2-beta — DISCOVER"], finish_reply()),
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
            script(&["NODE root — DISCOVER"], split_two()),
            script(&["NODE root/1-alpha — DISCOVER"], finish_reply()),
            script(&["NODE root/2-beta — DISCOVER"], finish_reply()),
            script(&["NODE root/3-", "— DISCOVER"], finish_reply()),
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
// Scenario F — checked edits (PRD 21 lane C4): a leaf proposes at its own
// fold, its parent selects and composes, the runtime applies against the
// companion's working draft, and one receipt is stamped per approved plan.
// ---------------------------------------------------------------------------

/// A `FoldDecision` reply (PRD 21 lane C4) that proposes exactly ONE new
/// draft edit of this node's own — a leaf's only route to proposing
/// anything, since its own fold has no children to select from. PLAIN DATA
/// only (`intent`/`append`): neither `runLLMTurnBranch` nor `runLLMTurnFork`
/// — the only two windows this harness ever finalizes across — can deliver a
/// finalized answer that carries a live closure, so the wire type
/// (`Harness.hs`'s `ProposedEditWire`) never has one to write here; the
/// runtime builds the real `Text -> Either EditFailure Text` itself. A blank
/// `intent` is what the runtime refuses (`Harness.wrapEdit`).
fn propose_edit_reply(synthesis: &str, intent: &str, append: &str) -> String {
    haskell(&format!(
        "finalize @FoldDecision (FoldDecision {{ foldSynthesis = \"{synthesis}\", \
         foldTensions = [], foldSelected = [], foldComposition = [], \
         foldProposed = [ProposedEditWire {{ editIntent = \"{intent}\", \
         editAppend = \"{append}\" }}] }})"
    ))
}

/// A `FoldDecision` reply that selects and composes artifact ids from the
/// pool its children advertised — `selected`/`composition` may name the SAME
/// ids in DIFFERENT orders, since `composition`, not `selected`, is what
/// governs apply order (`Tidepool.Thought.resolveSelection`).
fn select_composed_reply(synthesis: &str, selected: &[&str], composition: &[&str]) -> String {
    let quote_join = |ids: &[&str]| {
        ids.iter()
            .map(|id| format!("\"{id}\""))
            .collect::<Vec<_>>()
            .join(", ")
    };
    haskell(&format!(
        "finalize @FoldDecision (FoldDecision {{ foldSynthesis = \"{synthesis}\", \
         foldTensions = [], foldSelected = [{}], foldComposition = [{}], \
         foldProposed = [] }})",
        quote_join(selected),
        quote_join(composition)
    ))
}

/// PRD 21 lane C4 — the live fold window actually runs
/// `resolveSelection`/`approve`/`applyEdits` against the companion's working
/// draft, with failure isolation and one receipt per approved plan.
///
/// The tree: root splits into two leaves, Alpha and Beta (`split_two()`).
/// Alpha's own fold proposes an edit that SUCCEEDS; Beta's own fold proposes
/// one that always REFUSES. Neither leaf's own artifact is selectable at its
/// own fold (an empty pool — no children), so this ALSO proves "approval is
/// the parent's fold": only root, one level up, can ever apply either one.
/// Root's own fold selects BOTH ids but COMPOSES beta before alpha — the
/// opposite of `foldSelected`'s own list order — so the result can only
/// match if composition order, not selection order, governed the apply.
/// Reuses the "tree" scenario's exact config (`state_json(3, 40, 5,
/// GateOff)`), so this shares that compile shape rather than opening a new
/// one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn companion_leaf_proposes_parent_selects_runtime_applies_with_receipts() {
    let _cache_guard = support::isolate_cache();

    let alpha_id = "root/1-alpha#1";
    let beta_id = "root/2-beta#1";

    let run = run_scenario(
        "checked-edits",
        state_json_with_draft(3, 40, 5, json!({"tag": "GateOff"}), "seed"),
        vec![
            script(&["NODE root — DISCOVER"], split_two()),
            script(&["NODE root/1-alpha — DISCOVER"], finish_reply()),
            script(&["NODE root/2-beta — DISCOVER"], finish_reply()),
            script(
                &["NODE root/1-alpha — FOLD"],
                propose_edit_reply("alpha folds locally", "append alpha's suggestion", "-alpha"),
            ),
            script(
                &["NODE root/2-beta — FOLD"],
                // A BLANK intent is what the runtime refuses — the only
                // refusal a plain-data proposal can express (see
                // `propose_edit_reply`'s own doc).
                propose_edit_reply("beta folds locally", "", "-beta"),
            ),
            script(
                &["NODE root — FOLD"],
                select_composed_reply(
                    "root selects both, composed beta then alpha",
                    &[alpha_id, beta_id],
                    &[beta_id, alpha_id],
                ),
            ),
        ],
        Arc::new(ScriptedGate::default()),
    )
    .await;

    // --- the draft actually changed, at the ROOT's own position -----------
    assert_eq!(
        run.state.get("draft").and_then(|v| v.as_str()),
        Some("seed-alpha"),
        "root approved and applied alpha's edit against the turn-start draft \
         (\"seed\"); beta's own refusal left the running snapshot untouched \
         for alpha to apply against, got: {}",
        run.state
    );

    // --- one receipt per approved plan, in COMPOSITION order ---------------
    let edits_at = |path: &str| -> Vec<Json> {
        run.journal_kind("edits")
            .into_iter()
            .filter(|e| e.key == path)
            .map(|e| e.payload.clone())
            .collect()
    };
    let root_edits = edits_at("root");
    assert_eq!(
        root_edits.len(),
        1,
        "root's own fold approved exactly one selection: {root_edits:?}"
    );
    let payload = &root_edits[0];
    assert_eq!(payload.get("before").and_then(|v| v.as_str()), Some("seed"));
    assert_eq!(
        payload.get("after").and_then(|v| v.as_str()),
        Some("seed-alpha")
    );
    let receipts = payload
        .get("receipts")
        .and_then(|v| v.as_array())
        .expect("root's edits receipt carries a receipts array");
    assert_eq!(
        receipts.len(),
        2,
        "one receipt per approved plan, failing or not: {receipts:?}"
    );
    assert_eq!(
        receipts[0].get("artifact").and_then(|v| v.as_str()),
        Some(beta_id),
        "receipts follow COMPOSITION order, not foldSelected's own list order: {receipts:?}"
    );
    assert!(
        receipts[0]
            .get("outcome")
            .and_then(|o| o.get("refused"))
            .is_some(),
        "beta's own refusal is an isolated Left, never poisoning a sibling: {receipts:?}"
    );
    assert_eq!(
        receipts[1].get("artifact").and_then(|v| v.as_str()),
        Some(alpha_id)
    );
    assert_eq!(
        receipts[1]
            .get("outcome")
            .and_then(|o| o.get("applied"))
            .and_then(|v| v.as_str()),
        Some("seed-alpha"),
        "alpha's edit applied against the state beta's refusal left untouched: {receipts:?}"
    );

    // --- neither LEAF's own fold ever approves anything ---------------------
    // Decision 7 ("approval is the parent's fold"), read structurally: a
    // leaf's own pool is always empty (no children), so its own selection
    // can never resolve past Narrative, and it journals no "edits" entry at
    // all — the exact same silence a narrative fold keeps.
    assert!(
        edits_at("root/1-alpha").is_empty(),
        "a leaf's own fold cannot approve its own proposal"
    );
    assert!(
        edits_at("root/2-beta").is_empty(),
        "a leaf's own fold cannot approve its own proposal"
    );

    // --- the receipt is visible in the render tree, and ONLY at root -------
    assert!(
        run.tree_line("root")
            .contains("edits: 1 applied, 1 refused"),
        "the receipt surfaces as a badge on the node that actually applied: {}",
        run.tree_line("root")
    );
    for leaf in ["root/1-alpha", "root/2-beta"] {
        assert!(
            !run.tree_line(leaf).contains("edits:"),
            "a leaf that only PROPOSED (never approved) carries no edits badge \
             of its own: {}",
            run.tree_line(leaf)
        );
    }
}

/// PRD 21 lane C4 — a fold that neither selects nor proposes anything is
/// byte-behaviorally the pre-C4 narrative fold: no "edits" journal entry, no
/// "edits:" badge, and the draft carries all the way through a turn
/// unchanged. Every OTHER scenario in this file already pins this
/// (`fold_script()`'s shared reply supplies empty defaults throughout), so
/// this asserts it directly, once, off the smallest possible tree — a single
/// root `Finish`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn companion_narrative_fold_is_byte_behaviorally_unchanged_by_c4() {
    let _cache_guard = support::isolate_cache();

    let run = run_scenario(
        "narrative-unchanged",
        state_json_with_draft(3, 40, 5, json!({"tag": "GateOff"}), "untouched"),
        vec![
            script(&["NODE root — DISCOVER"], finish_reply()),
            fold_script(),
        ],
        Arc::new(ScriptedGate::default()),
    )
    .await;

    assert_eq!(
        run.state.get("draft").and_then(|v| v.as_str()),
        Some("untouched"),
        "a fold that selects and proposes nothing never touches the draft: {}",
        run.state
    );
    assert!(
        run.journal_kind("edits").is_empty(),
        "no artifacts were ever proposed or selected, so no edits kind is journaled"
    );
    assert!(
        !run.tree_line("root").contains("edits:"),
        "and no badge appears on the one node that folded: {}",
        run.tree_line("root")
    );
}
