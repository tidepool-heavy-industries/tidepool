//! The acceptance harness for `withHandler`.
//!
//! One named test per AUTHORED semantic (`haskell/lib/Tidepool/Event.hs`'s
//! "Handler semantics" list), each proving its semantic separately, against a
//! REAL temporary git repository driven by
//! [`ScriptedWriter`](tidepool_worktree::testing::ScriptedWriter). No mock of
//! git anywhere and no LLM anywhere: a mock proves the mock agrees with the
//! author's model of git, which is exactly the thing in doubt when the code
//! under test exists to observe git honestly.
//!
//! ## Why this is a standalone driver
//!
//! It compiles real Haskell through the real extract, builds a real
//! `JitEffectMachine`, and drives it on the PARKED path directly —
//! `run_suspendable_parked` / `resume_parked` — exactly as
//! `tidepool-codegen/tests/realm_per_realm_fields.rs` and
//! `realm_stream_registry_lifetime.rs` do. It does NOT route through the
//! resident session or the harness engine, because the thing under test is the
//! interposition's interaction with suspension, and a session engine in the
//! middle would be a second suspension policy sitting on top of the one being
//! proven.
//!
//! ## What the parking contract obliges of this file
//!
//! `docs/continuation-parking-contract.md` is frozen
//! and is consumed, not extended:
//!
//! - the handled prefix is DERIVED from [`Session::stack`] — the same value
//!   that built the handler stack — never restated at a park site, so the
//!   lying-realm case is unconstructible rather than a duty to discharge;
//! - the rooting receipt `stowed_roots_count() == parked_count()` is asserted
//!   at EVERY quiescent point (see [`assert_rooting_receipt`]);
//! - the two suspension paths are never mixed: nothing here touches
//!   `suspended_continuation`, `run_child_fragment`, `enter_nested_child`, or
//!   `run_child_fragment_pure`;
//! - the realm is cycle-scoped: one [`RealmId`] per machine, and the machine is
//!   dropped at the end of the test that made it.
//!
//! Nothing in the contract's §3 (internal, free to churn) is depended on.
//!
//! ## The observation source, and why it is not a mock
//!
//! Reconciliation is injected ([`tidepool_handlers::ObservationSource`]). In
//! production the implementation ([`tidepool_handlers::MonitorObservations`])
//! adapts `WorktreeMonitor`, which owns the git reasoning and journals its
//! baseline. These tests instead supply [`RealGitObservations`] — a second,
//! independent implementation that computes the same facts from REAL git
//! reads (`rev-parse`, `merge-base --is-ancestor`, `rev-list`, `show`) against
//! the same repository `ScriptedWriter` is writing to, with its baseline held
//! in process memory (legitimate only for the single-cycle harness this is —
//! see the struct doc below). It reads git; it does not pretend to be git.
//!
//! Needs `TIDEPOOL_EXTRACT` (a built `tidepool-extract-bin`) + GHC on PATH;
//! fails loudly otherwise (see `require_ghc`) rather than skipping as a silent
//! pass.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use std::time::Duration;

use tidepool_bridge::{FromCore, ToCore};
use tidepool_bridge_effects::{
    EvCommitReceipt, EvEventId, EvHeadChangeKind, EvHeadChangeReceipt, EvRepositoryEvent,
    WtBranchName, WtGitOid, WtWorktreeId,
};
use tidepool_codegen::jit_machine::{
    ContinuationId, JitEffectMachine, JitError, ParkedOutcome, RealmId, ResumeInput,
};
use tidepool_effect::dispatch::{EffectContext, EffectHandler};
use tidepool_effect::error::EffectError;
use tidepool_effect::Response;
use tidepool_eval::value::Value;
use tidepool_handlers::{
    ConsoleHandler, EventConfig, EventError, ObservationSource, RepoEventHandler,
};
use tidepool_mcp::{CapturedOutput, DescribeEffect, EffectDecl};
use tidepool_repr::DataConTable;
use tidepool_worktree::testing::{ScriptedWriter, TestRepo};
use tidepool_worktree::GitCli;

/// The wire id every test's managed worktree goes by. It is opaque to the
/// runtime — the only thing that matters is that the Haskell handle and the
/// observation source agree on it.
const TREE: &str = "wt-under-test";

fn tree_id() -> WtWorktreeId {
    WtWorktreeId {
        raw: TREE.to_string(),
    }
}

// ============================================================================
// The row
// ============================================================================

/// The `Worktree` row entry, present so the generated `Tidepool.Effects`
/// carries `WorktreeId`/`WorktreeHandle`/`renderGitOid` — which the frozen
/// `RepoEvent` definition's own types and helpers are written against.
///
/// It handles nothing: `tidepool-handlers/src/handlers/worktree.rs` is a
/// sibling lane's file, and none of these tests calls a worktree verb (each
/// builds its `WorktreeHandle` as ordinary Haskell data). Reaching this
/// handler would mean a test started exercising a lane it does not own, so it
/// panics rather than answering.
struct UnwiredWorktreeRow;

impl DescribeEffect for UnwiredWorktreeRow {
    fn effect_decl() -> EffectDecl {
        tidepool_mcp::worktree_decl()
    }
}

impl EffectHandler<CapturedOutput> for UnwiredWorktreeRow {
    type Request = Value;

    fn handle(
        &mut self,
        _req: Value,
        _cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<Response, EffectError> {
        panic!(
            "a Worktree verb was dispatched: this harness owns the RepoEvent effect only, \
             and its worktree handles are plain data"
        );
    }
}

type Stack = frunk::HList!(ConsoleHandler, UnwiredWorktreeRow, RepoEventHandler);

// ============================================================================
// Observations from a real repository
// ============================================================================

/// One reconciliation pass computed from real git reads.
///
/// Holds the last observed head per worktree — legitimate state, because a
/// delta needs a previous — and nothing else. Its first pass over a worktree
/// establishes the baseline: it reports the head it found with
/// `UnknownChange`, and reports NO commits, because "everything reachable from
/// HEAD" is not honestly "what just happened".
///
/// That baseline lives in PROCESS MEMORY, which is legitimate here and only
/// here: every test below is one resident cycle in one process. It is exactly
/// the "start from now" shape that
/// [`ObservationSource`](tidepool_handlers::ObservationSource) forbids in
/// production — a memory baseline would decide nothing moved on the first pass
/// of a new cycle and silently lose everything committed in the gap between
/// cycles. The production adapter (`MonitorObservations`) holds no baseline at
/// all and inherits the monitor's journalled one.
struct RealGitObservations {
    git: GitCli,
    trees: BTreeMap<String, PathBuf>,
    seen_head: BTreeMap<String, String>,
    next_event_id: i64,
}

impl RealGitObservations {
    fn new(trees: impl IntoIterator<Item = (String, PathBuf)>) -> Self {
        Self {
            git: GitCli::new(),
            trees: trees.into_iter().collect(),
            seen_head: BTreeMap::new(),
            next_event_id: 1,
        }
    }

    fn read(&self, cwd: &Path, args: &[&str]) -> Result<String, EventError> {
        self.git
            .run(cwd, args)
            .map(|o| o.trimmed().to_string())
            .map_err(|r| {
                EventError::EventSourceFailed(format!(
                    "git {}: {}",
                    args.join(" "),
                    r.stderr.trim()
                ))
            })
    }

    fn commit_receipt(&self, cwd: &Path, oid: &str) -> Result<EvCommitReceipt, EventError> {
        let meta = self.read(
            cwd,
            &["show", "-s", "--format=%H%x00%P%x00%s%x00%an%x00%ct", oid],
        )?;
        let f: Vec<&str> = meta.split('\u{0}').collect();
        if f.len() < 5 {
            return Err(EventError::EventSourceFailed(format!(
                "unreadable commit metadata for {oid}: {meta:?}"
            )));
        }
        let files = self.read(cwd, &["show", "--pretty=format:", "--name-only", oid])?;
        Ok(EvCommitReceipt {
            commit_worktree: WtWorktreeId {
                raw: String::new(), // filled by the caller, which knows the id
            },
            oid: WtGitOid {
                raw: f[0].to_string(),
            },
            parents: f[1]
                .split_whitespace()
                .map(|p| WtGitOid { raw: p.to_string() })
                .collect(),
            subject: f[2].to_string(),
            author: f[3].to_string(),
            committed_at_ms: f[4].parse::<i64>().unwrap_or(0) * 1000,
            files: files
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(str::to_string)
                .collect(),
        })
    }
}

impl ObservationSource for RealGitObservations {
    fn observe(
        &mut self,
        worktrees: &[WtWorktreeId],
    ) -> Result<Vec<EvRepositoryEvent>, EventError> {
        let mut out = Vec::new();
        for id in worktrees {
            let cwd = match self.trees.get(&id.raw) {
                Some(p) => p.clone(),
                None => return Err(EventError::EventSourceLost(id.raw.clone())),
            };
            let head = self.read(&cwd, &["rev-parse", "HEAD"])?;
            let previous = self.seen_head.get(&id.raw).cloned();
            if previous.as_deref() == Some(head.as_str()) {
                continue; // idempotent: nothing moved since the last pass
            }
            self.seen_head.insert(id.raw.clone(), head.clone());
            let branch = self
                .git
                .run(&cwd, &["symbolic-ref", "--short", "HEAD"])
                .ok()
                .map(|o| WtBranchName {
                    raw: o.trimmed().to_string(),
                });

            // One id per PASS: co-emitted views of one change share it.
            let event_id = EvEventId {
                raw: self.next_event_id,
            };
            self.next_event_id += 1;

            let (gained, kind) = match previous.as_deref() {
                None => (Vec::new(), EvHeadChangeKind::UnknownChange),
                Some(old) if self.is_ancestor(&cwd, old, &head) => {
                    let listed =
                        self.read(&cwd, &["rev-list", "--reverse", &format!("{old}..{head}")])?;
                    let gained: Vec<String> = listed
                        .lines()
                        .map(str::to_string)
                        .filter(|l| !l.is_empty())
                        .collect();
                    let kind = EvHeadChangeKind::Advanced(
                        gained.iter().map(|o| WtGitOid { raw: o.clone() }).collect(),
                    );
                    (gained, kind)
                }
                Some(old) if self.is_ancestor(&cwd, &head, old) => {
                    (Vec::new(), EvHeadChangeKind::Rewound)
                }
                // Neither is an ancestor of the other: the movement is real but
                // its shape is not honestly recoverable from HEAD alone.
                // Degrading here is a correct answer, not a failure.
                Some(_) => (Vec::new(), EvHeadChangeKind::UnknownChange),
            };

            for oid in &gained {
                let mut receipt = self.commit_receipt(&cwd, oid)?;
                receipt.commit_worktree = id.clone();
                out.push(EvRepositoryEvent::ObservedCommit(event_id, receipt));
            }
            out.push(EvRepositoryEvent::ObservedHeadChange(
                event_id,
                EvHeadChangeReceipt {
                    head_worktree: id.clone(),
                    old_head: previous.map(|p| WtGitOid { raw: p }),
                    new_head: WtGitOid { raw: head },
                    kind,
                    head_branch: branch,
                    observed_at_ms: 0,
                },
            ));
        }
        Ok(out)
    }
}

impl RealGitObservations {
    fn is_ancestor(&self, cwd: &Path, a: &str, b: &str) -> bool {
        self.git
            .run(cwd, &["merge-base", "--is-ancestor", a, b])
            .is_ok()
    }
}

/// Records what the inner source EMITTED, so a test can assert that the
/// runtime genuinely observed a fact and that the fact still never reached a
/// subscription. Without this, "no replay" would only be provable as an
/// absence, which an observer that simply never saw the commit would also
/// satisfy.
struct Recording<S> {
    inner: S,
    log: Arc<Mutex<Vec<EvRepositoryEvent>>>,
}

impl<S: ObservationSource> ObservationSource for Recording<S> {
    fn observe(
        &mut self,
        worktrees: &[WtWorktreeId],
    ) -> Result<Vec<EvRepositoryEvent>, EventError> {
        let events = self.inner.observe(worktrees)?;
        self.log.lock().extend(events.iter().cloned());
        Ok(events)
    }
}

/// Every commit oid the observation source ever emitted, in order.
fn observed_commits(log: &Arc<Mutex<Vec<EvRepositoryEvent>>>) -> Vec<String> {
    log.lock()
        .iter()
        .filter_map(|e| match e {
            EvRepositoryEvent::ObservedCommit(_, r) => Some(r.oid.raw.clone()),
            _ => None,
        })
        .collect()
}

// ============================================================================
// The driver
// ============================================================================

/// FAIL LOUDLY when the environment cannot run these gates.
///
/// These gates drive a real extract + JIT + temp git repository; without
/// `TIDEPOOL_EXTRACT` and GHC they can verify NOTHING. An early return here
/// would be a skip spelled as a pass — nextest reports it as a PASS,
/// structurally indistinguishable from a real one: it runs by name, emits a
/// real PASS line, and counts toward started-vs-run. The root `CLAUDE.md`
/// requires tests without `TIDEPOOL_EXTRACT` to fail loud.
///
/// Safe because `scripts/battery.sh` derives `TIDEPOOL_EXTRACT` itself — this
/// fires only on the direct-invocation mistake that would otherwise yield a
/// false green.
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

/// What a driven turn did. `Asked` carries the `ask` prompt, which is how these
/// tests observe the ORDER in which a suspending handler was invoked.
#[derive(Debug)]
enum Step {
    Asked(String),
    Completed,
}

/// A compiled program plus the machine and handler stack driving it on the
/// parked path.
struct Session {
    machine: JitEffectMachine,
    table: DataConTable,
    stack: Stack,
    captured: CapturedOutput,
    ask_tag: u64,
    /// DERIVED from `stack` at construction and never restated at a park site
    /// — see the parking contract's "derive, don't declare".
    handled_prefix: Vec<String>,
    parked: Option<ContinuationId>,
    /// Every continuation id this realm has ever handed out, in order. Ids are
    /// never reused, so this doubles as the no-ABA check.
    ids_seen: Vec<ContinuationId>,
}

impl Session {
    /// Compile `code` (a bare statement sequence) against a row containing the
    /// RepoEvent handler, and build a session machine for it.
    fn compile(code: &str, events: RepoEventHandler) -> Self {
        let stack: Stack = frunk::hlist![ConsoleHandler, UnwiredWorktreeRow, events];

        // ONE source of truth: the decls, the effect-row type, the generated
        // `Tidepool.Effects`, the suspend threshold, and the handled prefix all
        // come from the value that IS the handler stack.
        let (decls, ask_tag) = tidepool_handlers::base_decls_with_ask(&stack);
        let handled_prefix: Vec<String> = decls[..ask_tag as usize]
            .iter()
            .map(|d| d.type_name.to_string())
            .collect();

        let preamble = tidepool_mcp::build_preamble(&decls, false);
        let row = tidepool_mcp::build_effect_stack_type(&decls);
        let source = tidepool_mcp::template_haskell(
            &preamble,
            &row,
            &tidepool_mcp::wrap_do(code),
            "",
            "",
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

        let s = Self {
            machine,
            table,
            stack,
            captured: CapturedOutput::new(),
            ask_tag,
            handled_prefix,
            parked: None,
            ids_seen: Vec::new(),
        };
        s.assert_rooting_receipt();
        s
    }

    /// The parked-continuation count this machine currently holds.
    fn parked_count(&self) -> usize {
        self.machine.parked_count()
    }

    /// Assert the receipt AND that it sits at an expected count — for the
    /// dedicated rooting test, where "1 == 1" and "0 == 0" are different facts.
    fn assert_parked(&self, expect: usize) {
        self.assert_rooting_receipt();
        assert_eq!(
            self.machine.parked_count(),
            expect,
            "expected {expect} parked continuation(s) in this realm"
        );
    }

    /// The rooting receipt the parking contract offers consumers, asserted at
    /// every quiescent point: a parked continuation must be a REGISTERED GC
    /// root for its whole parked lifetime.
    fn assert_rooting_receipt(&self) {
        assert_eq!(
            self.machine.stowed_roots_count(),
            self.machine.parked_count(),
            "every parked continuation must be a REGISTERED GC root for its whole \
             parked lifetime (parked={}, stowed={})",
            self.machine.parked_count(),
            self.machine.stowed_roots_count()
        );
    }

    fn start(&mut self) -> Result<Step, JitError> {
        let outcome = self.machine.run_suspendable_parked(
            &self.table,
            &mut self.stack,
            &self.captured,
            self.ask_tag,
            RealmId(0),
            &self.handled_prefix,
        );
        self.absorb(outcome)
    }

    /// Resume the live park with `answer` (the shape the server's validated
    /// reply would arrive in).
    fn answer(&mut self, answer: serde_json::Value) -> Result<Step, JitError> {
        let id = self.parked.expect("resume called with nothing parked");
        let bridged = answer
            .to_value(&self.table)
            .expect("bridging the ask answer");
        let outcome = self.machine.resume_parked(
            id,
            &mut self.stack,
            &self.captured,
            ResumeInput::Answer(bridged),
        );
        self.absorb(outcome)
    }

    fn absorb(&mut self, outcome: Result<ParkedOutcome, JitError>) -> Result<Step, JitError> {
        match outcome {
            Ok(ParkedOutcome::Suspended { id, request, .. }) => {
                self.parked = Some(id);
                self.ids_seen.push(id);
                let prompt = ask_prompt(&request, &self.table);
                self.assert_rooting_receipt();
                Ok(Step::Asked(prompt))
            }
            Ok(ParkedOutcome::CompletedValue(..) | ParkedOutcome::CompletedBinding { .. }) => {
                self.parked = None;
                self.assert_rooting_receipt();
                Ok(Step::Completed)
            }
            Ok(ParkedOutcome::CompletedProject { .. } | ParkedOutcome::CompletedRender { .. }) => {
                unreachable!(
                    "this harness parks only ParkKind::Plain turns — Project/Render \
                     completions cannot be produced for them"
                )
            }
            Err(e) => {
                self.parked = None;
                // A failed turn is a quiescent point too — the receipt has to
                // hold on the error path or a park is protected by nothing
                // exactly when something already went wrong.
                self.assert_rooting_receipt();
                Err(e)
            }
        }
    }

    /// Everything the program printed, in order.
    fn output(&self) -> Vec<String> {
        self.captured.drain()
    }

    /// Take the RepoEvent handler back so a test can inspect the registry after
    /// the scope ended.
    fn into_event_handler(self) -> RepoEventHandler {
        let Session { machine, stack, .. } = self;
        drop(machine); // cycle-scoped: the realm never outlives its machine
        let frunk::hlist_pat![_console, _worktree, events] = stack;
        events
    }
}

/// Pull the prompt out of an `AskWith prompt payload` suspension request.
fn ask_prompt(request: &Value, table: &DataConTable) -> String {
    match request {
        Value::Con(id, fields) if table.name_of(*id) == Some("AskWith") && fields.len() == 2 => {
            String::from_value(&fields[0], table).expect("an ask prompt is Text")
        }
        other => panic!("expected an AskWith suspension request, got {other:?}"),
    }
}

fn expect_ask(step: Result<Step, JitError>) -> String {
    match step.expect("the turn must not fail here") {
        Step::Asked(p) => p,
        Step::Completed => panic!("expected a suspension, the turn completed"),
    }
}

fn expect_completed(step: Result<Step, JitError>) {
    match step.expect("the turn must not fail here") {
        Step::Completed => {}
        Step::Asked(p) => panic!("expected completion, suspended on {p:?} instead"),
    }
}

/// The Haskell handle every program watches. Plain data — no worktree verb is
/// called, so the receipt's other fields are inert.
fn handle_expr() -> String {
    format!(
        "let tree = WorktreeHandle (WorktreeReceipt (WorktreeId \"{TREE}\") \".\" \
         (BranchName \"main\") (GitOid \"0000000000000000000000000000000000000000\") Nothing 0)"
    )
}

/// Boot a real repository with one commit, and an observation source already
/// BASELINED against it — which is what a long-lived polling runtime looks
/// like when a `withHandler` scope opens.
fn booted_repo() -> (
    TestRepo,
    RepoEventHandler,
    Arc<Mutex<Vec<EvRepositoryEvent>>>,
) {
    booted_repo_with(EventConfig {
        queue_bound: 64,
        // Every drain reconciles: the rate limit is a real mechanism but it is
        // pinned by its own unit tests in `handlers/event.rs`, and leaving it
        // on here would make these tests depend on wall-clock timing.
        poll_interval: Duration::ZERO,
    })
}

fn booted_repo_with(
    config: EventConfig,
) -> (
    TestRepo,
    RepoEventHandler,
    Arc<Mutex<Vec<EvRepositoryEvent>>>,
) {
    let repo = TestRepo::init().expect("git init a real temporary repository");
    repo.writer()
        .commit_file("seed.txt", "seed\n", "seed")
        .expect("seed commit");

    let mut source = RealGitObservations::new([(TREE.to_string(), repo.path().to_path_buf())]);
    // The baseline pass. After it the observer knows where the repository IS,
    // so a later pass reports a delta rather than the whole history.
    source.observe(&[tree_id()]).expect("baseline pass");

    let log: Arc<Mutex<Vec<EvRepositoryEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let handler = RepoEventHandler::with_source(
        Box::new(Recording {
            inner: source,
            log: log.clone(),
        }),
        config,
    );
    (repo, handler, log)
}

fn writer(repo: &TestRepo) -> ScriptedWriter<'_> {
    repo.writer()
}

/// Run `f` on a thread with room for the JIT's own stack usage.
fn in_test_thread(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

// ============================================================================
// One test per authored semantic
// ============================================================================

/// > A subscription begins at registration and never replays older events.
///
/// Two SEQUENTIAL `withHandler` scopes over the same source — the shape a
/// resident walks every time it re-registers its reactions from state and
/// stable worktree ids, which is an ordinary repeated path now that agents may
/// keep running between cycles.
///
/// Both halves of the rule are pinned here, because implementing away either
/// one is easy and the failures look nothing alike:
///
/// - a commit made while NOBODY was subscribed still reaches the first scope —
///   "no replay" is a rule about facts the runtime already observed, not a
///   licence to lose repository movement from the gap;
/// - the two commits the FIRST scope consumed never reach the second, even
///   though the second watches the same source, and a commit made after it
///   registers does.
#[test]
fn no_replay_of_events_observed_before_registration() {
    require_ghc();
    in_test_thread(|| {
        let (repo, handler, log) = booted_repo();
        // The gap window: real movement with no subscription in existence.
        let gap = writer(&repo)
            .commit_file("a.txt", "1\n", "while nobody was subscribed")
            .expect("gap-window commit");

        let code = format!(
            "{handle}\n\
             withHandler (commit tree) (\\o -> say (\"first:\" <> renderGitOid o.value.oid)) $ do\n\
             \x20 _ <- ask SNum \"cp1\"\n\
             \x20 _ <- ask SNum \"cp2\"\n\
             \x20 pure ()\n\
             withHandler (commit tree) (\\o -> say (\"second:\" <> renderGitOid o.value.oid)) $ do\n\
             \x20 _ <- ask SNum \"cp3\"\n\
             \x20 _ <- ask SNum \"cp4\"\n\
             \x20 pure ()\n\
             say \"end\"\n\
             pure (\"ok\" :: Text)",
            handle = handle_expr()
        );
        let mut s = Session::compile(&code, handler);

        // The first scope's opening drain picks the gap commit up.
        assert_eq!(expect_ask(s.start()), "cp1");
        let during_first = writer(&repo)
            .commit_file("b.txt", "2\n", "while the first scope is live")
            .expect("commit");
        assert_eq!(expect_ask(s.answer(serde_json::json!(0))), "cp2");

        // First scope ends, second registers; nothing new has happened yet.
        assert_eq!(expect_ask(s.answer(serde_json::json!(0))), "cp3");
        let during_second = writer(&repo)
            .commit_file("c.txt", "3\n", "while the second scope is live")
            .expect("commit");
        assert_eq!(expect_ask(s.answer(serde_json::json!(0))), "cp4");
        expect_completed(s.answer(serde_json::json!(0)));

        // The runtime observed each commit exactly once — so the absences below
        // are the registry declining to replay, not an observer that went blind.
        assert_eq!(
            observed_commits(&log),
            vec![
                gap.to_string(),
                during_first.to_string(),
                during_second.to_string()
            ]
        );
        assert_eq!(
            s.output(),
            vec![
                format!("first:{gap}"),
                format!("first:{during_first}"),
                format!("second:{during_second}"),
                "end".to_string(),
            ],
            "the second registration must start empty — never replaying what the \
             first already consumed — while still tracking everything after it"
        );

        let events = s.into_event_handler();
        assert!(
            events.registry().live_ids().is_empty(),
            "both scopes unregistered on exit"
        );
    });
}

/// > Events broadcast to all registered handlers. They are not consumed by the
/// > first one to see them.
///
/// Two nested `withHandler` scopes over the same source. One commit; both
/// handlers fire, each exactly once.
#[test]
fn one_commit_broadcasts_to_both_registered_handlers() {
    require_ghc();
    in_test_thread(|| {
        let (repo, handler, _log) = booted_repo();
        let code = format!(
            "{handle}\n\
             withHandler (commit tree) (\\o -> say (\"A:\" <> renderGitOid o.value.oid)) $\n\
             \x20 withHandler (commit tree) (\\o -> say (\"B:\" <> renderGitOid o.value.oid)) $ do\n\
             \x20   _ <- ask SNum \"cp\"\n\
             \x20   pure ()\n\
             pure (\"ok\" :: Text)",
            handle = handle_expr()
        );
        let mut s = Session::compile(&code, handler);

        assert_eq!(expect_ask(s.start()), "cp");
        let c = writer(&repo)
            .commit_file("a.txt", "1\n", "one commit, two reactions")
            .expect("commit");
        expect_completed(s.answer(serde_json::json!(0)));

        let mut out = s.output();
        out.sort();
        assert_eq!(
            out,
            vec![format!("A:{c}"), format!("B:{c}")],
            "one fact must reach BOTH subscriptions exactly once, not be consumed \
             by whichever drained first"
        );
        let events = s.into_event_handler();
        assert!(
            events.registry().live_ids().is_empty(),
            "both scopes unregistered on exit"
        );
    });
}

/// > Each subscription invokes one handler at a time. Later matches queue in
/// > observation order, so a handler that suspends does not race its own next
/// > invocation.
///
/// Two commits land while the body is parked, so one drain yields both. The
/// handler `ask`s — a real suspension of the resident, mid-drain — and the
/// second invocation only happens after the first resumes, with the commits in
/// the order git made them.
#[test]
fn queued_observations_invoke_in_order_across_a_handler_suspension() {
    require_ghc();
    in_test_thread(|| {
        let (repo, handler, _log) = booted_repo();
        let code = format!(
            "{handle}\n\
             withHandler (commit tree) (\\o -> do {{ _ <- ask SNum (\"h:\" <> \
             renderGitOid o.value.oid); say (\"done:\" <> renderGitOid o.value.oid) }}) $ do\n\
             \x20 _ <- ask SNum \"cp1\"\n\
             \x20 _ <- ask SNum \"cp2\"\n\
             \x20 pure ()\n\
             pure (\"ok\" :: Text)",
            handle = handle_expr()
        );
        let mut s = Session::compile(&code, handler);

        assert_eq!(expect_ask(s.start()), "cp1");
        let w = writer(&repo);
        let c1 = w.commit_file("a.txt", "1\n", "first").expect("c1");
        let c2 = w.commit_file("b.txt", "2\n", "second").expect("c2");

        // The drain before `cp2` yields [c1, c2]; the handler suspends on each.
        assert_eq!(
            expect_ask(s.answer(serde_json::json!(0))),
            format!("h:{c1}")
        );
        assert_eq!(
            expect_ask(s.answer(serde_json::json!(0))),
            format!("h:{c2}")
        );
        assert_eq!(expect_ask(s.answer(serde_json::json!(0))), "cp2");
        expect_completed(s.answer(serde_json::json!(0)));

        assert_eq!(
            s.output(),
            vec![format!("done:{c1}"), format!("done:{c2}")],
            "the second invocation must follow the first's completion, in \
             observation order"
        );
        drop(s.into_event_handler());
    });
}

/// > When the body ends, intake closes, already-observed events plus any
/// > in-flight handler drain, and then the subscription unregisters.
///
/// The commit lands while the body is parked at its LAST effect. The pump ticks
/// before effects, and there are none left, so the only thing that can deliver
/// it is `withHandler`'s own lexical drain — which must run before the
/// statement after the scope, and before the id is spent.
#[test]
fn body_end_drains_before_it_unregisters() {
    require_ghc();
    in_test_thread(|| {
        let (repo, handler, _log) = booted_repo();
        let code = format!(
            "{handle}\n\
             withHandler (commit tree) (\\o -> say (\"H:\" <> renderGitOid o.value.oid)) $ do\n\
             \x20 _ <- ask SNum \"cp\"\n\
             \x20 pure ()\n\
             say \"after\"\n\
             pure (\"ok\" :: Text)",
            handle = handle_expr()
        );
        let mut s = Session::compile(&code, handler);

        assert_eq!(expect_ask(s.start()), "cp");
        let c = writer(&repo)
            .commit_file("a.txt", "1\n", "lands at the very end of the body")
            .expect("commit");
        expect_completed(s.answer(serde_json::json!(0)));

        assert_eq!(
            s.output(),
            vec![format!("H:{c}"), "after".to_string()],
            "the lexical drain must deliver before the scope exits"
        );

        let mut events = s.into_event_handler();
        assert!(
            events.registry().live_ids().is_empty(),
            "the subscription must be unregistered on exit"
        );
        // And the id is SPENT, not merely idle: draining it is a named error.
        let spent = tidepool_bridge_effects::EvSubscriptionId { raw: 1 };
        assert_eq!(
            events.registry_mut().drain(spent),
            Err(EventError::EventUnknownSubscription(1)),
            "a spent subscription id must fail a later drain, not answer it empty"
        );
    });
}

/// > Handler failure fails the enclosing scope. It is never logged and
/// > forgotten.
#[test]
fn handler_failure_fails_the_enclosing_scope() {
    require_ghc();
    in_test_thread(|| {
        let (repo, handler, _log) = booted_repo();
        let code = format!(
            "{handle}\n\
             withHandler (commit tree) (\\_o -> error \"handler exploded\") $ do\n\
             \x20 _ <- ask SNum \"cp1\"\n\
             \x20 _ <- ask SNum \"cp2\"\n\
             \x20 pure ()\n\
             say \"after\"\n\
             pure (\"ok\" :: Text)",
            handle = handle_expr()
        );
        let mut s = Session::compile(&code, handler);

        assert_eq!(expect_ask(s.start()), "cp1");
        writer(&repo)
            .commit_file("a.txt", "1\n", "trips the failing handler")
            .expect("commit");

        let err = s
            .answer(serde_json::json!(0))
            .expect_err("a failing handler must fail the turn, not be swallowed");
        assert!(
            format!("{err}").contains("handler exploded"),
            "the failure must name the handler's own error; got: {err}"
        );
        assert!(
            !s.output().iter().any(|l| l == "after"),
            "the statement after the scope must not run"
        );
        drop(s.into_event_handler());
    });
}

/// > Queue overflow, source loss, or an inability to drain fails LOUDLY.
/// > Commits are never silently dropped.
///
/// The bound is set to one, and two commits land in a single park window, so
/// one observation cannot be queued. The subscription poisons and the drain
/// returns `EventQueueOverflow`, which `drainSubscription`'s `liftEither`
/// turns into a failure of the enclosing scope.
#[test]
fn bounded_queue_overflow_fails_loudly_rather_than_dropping_a_commit() {
    require_ghc();
    in_test_thread(|| {
        let (repo, handler, _log) = booted_repo_with(EventConfig {
            queue_bound: 1,
            poll_interval: Duration::ZERO,
        });
        let code = format!(
            "{handle}\n\
             withHandler (commit tree) (\\o -> say (\"H:\" <> renderGitOid o.value.oid)) $ do\n\
             \x20 _ <- ask SNum \"cp1\"\n\
             \x20 _ <- ask SNum \"cp2\"\n\
             \x20 pure ()\n\
             say \"after\"\n\
             pure (\"ok\" :: Text)",
            handle = handle_expr()
        );
        let mut s = Session::compile(&code, handler);

        assert_eq!(expect_ask(s.start()), "cp1");
        let w = writer(&repo);
        w.commit_file("a.txt", "1\n", "fits the bound").expect("c1");
        w.commit_file("b.txt", "2\n", "overflows it").expect("c2");

        let err = s
            .answer(serde_json::json!(0))
            .expect_err("an overflowed queue must fail the scope");
        let msg = format!("{err}");
        assert!(
            msg.contains("EventQueueOverflow"),
            "the failure must name the overflow; got: {msg}"
        );
        assert!(
            msg.contains('1'),
            "the failure must carry the dropped count so the loss is quantified; got: {msg}"
        );
        assert!(
            !s.output().iter().any(|l| l == "after"),
            "the statement after the scope must not run"
        );

        // And the subscription stays poisoned: no silent recovery.
        let mut events = s.into_event_handler();
        let sub = tidepool_bridge_effects::EvSubscriptionId { raw: 1 };
        assert_eq!(
            events.registry_mut().drain(sub),
            Err(EventError::EventQueueOverflow(1, 1)),
            "a poisoned subscription must keep reporting, never look healthy again"
        );
    });
}

// ============================================================================
// The rooting receipt, as a gate of its own
// ============================================================================

/// The parking contract's receipt — `stowed_roots_count() == parked_count()` —
/// under a deliberate interleaving, as its own named gate.
///
/// Every other test in this file asserts the same equality at each quiescent
/// point, but always as an assertion buried inside a scenario. An assertion
/// that only ever runs inside other tests cannot be certified on its own: a
/// rename or a `cfg` would leave those tests green while the receipt silently
/// stopped being checked. This test exists so the receipt has a pass line.
///
/// The interleaving exercises the two shapes this lane actually produces:
///
/// - two cycle-scoped realms (one machine each — a realm never outlives its
///   cycle) live at the same time, resumed OUT of the order they parked in;
/// - a re-suspension inside a drain, when a handler `ask`s mid-invocation,
///   which mints a FRESH continuation id in the same realm.
///
/// It also pins that no id is ever reused, which is what makes an id a safe map
/// key with no ABA hazard.
#[test]
fn rooting_receipt_holds_across_an_interleaved_park_and_resume() {
    require_ghc();
    in_test_thread(|| {
        let (repo_a, handler_a, _log_a) = booted_repo();
        let (_repo_b, handler_b, _log_b) = booted_repo();

        // Realm A: a handler that suspends inside the drain, so A re-suspends
        // in the same realm rather than merely parking once.
        let code_a = format!(
            "{handle}\n\
             withHandler (commit tree) (\\o -> do {{ _ <- ask SNum (\"a-handler:\" <> \
             renderGitOid o.value.oid); pure () }}) $ do\n\
             \x20 _ <- ask SNum \"a1\"\n\
             \x20 _ <- ask SNum \"a2\"\n\
             \x20 pure ()\n\
             pure (\"ok\" :: Text)",
            handle = handle_expr()
        );
        // Realm B: an unrelated program on its OWN machine, parked across A's
        // whole life and resumed in between A's steps.
        let code_b = "_ <- ask SNum \"b1\"\n_ <- ask SNum \"b2\"\npure (\"ok\" :: Text)";

        let mut a = Session::compile(&code_a, handler_a);
        let mut b = Session::compile(code_b, handler_b);
        a.assert_parked(0);
        b.assert_parked(0);

        assert_eq!(expect_ask(a.start()), "a1");
        a.assert_parked(1);
        b.assert_parked(0);

        assert_eq!(expect_ask(b.start()), "b1");
        a.assert_parked(1);
        b.assert_parked(1);

        let c = writer(&repo_a)
            .commit_file("a.txt", "1\n", "wakes A's handler")
            .expect("commit");

        // A re-suspends INSIDE its drain: a fresh id, same realm, while B stays
        // parked and rooted throughout.
        assert_eq!(
            expect_ask(a.answer(serde_json::json!(0))),
            format!("a-handler:{c}")
        );
        a.assert_parked(1);
        b.assert_parked(1);

        // Resume the SIBLING realm in the middle — out of park order.
        assert_eq!(expect_ask(b.answer(serde_json::json!(0))), "b2");
        a.assert_parked(1);
        b.assert_parked(1);

        assert_eq!(expect_ask(a.answer(serde_json::json!(0))), "a2");
        a.assert_parked(1);
        b.assert_parked(1);

        // B finishes first, though it parked second.
        expect_completed(b.answer(serde_json::json!(0)));
        a.assert_parked(1);
        b.assert_parked(0);

        expect_completed(a.answer(serde_json::json!(0)));
        a.assert_parked(0);
        b.assert_parked(0);

        let mut ids = a.ids_seen.clone();
        assert_eq!(ids.len(), 3, "A parked three times (a1, the handler, a2)");
        ids.sort();
        ids.dedup();
        assert_eq!(
            ids.len(),
            3,
            "a resumed continuation id is permanently spent — never reused, so it \
             is a safe map key with no ABA hazard"
        );
        assert_eq!(a.parked_count(), 0);

        drop(a.into_event_handler());
        drop(b.into_event_handler());
    });
}
