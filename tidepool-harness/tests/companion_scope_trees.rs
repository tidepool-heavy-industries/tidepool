//! PRD 21 lane C2 — the scope-tree acceptance suite: locked decision 4 proved
//! END TO END through the REAL harness compile path (model reply → extract →
//! GHC → resident session), the escaped-closure crown jewel, and the root
//! accounting that makes the retirement claim honest.
//!
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH — run inside
//! `nix develop` (see `haskell/CLAUDE.md`). Fixtures:
//! `examples/harness/scope-spike/HarnessTypes.hs`. The C1 fixture
//! (`examples/harness/mount-spike`) and its suite
//! (`tests/companion_mount_spike.rs`) are deliberately untouched — that suite
//! is the FLAT-session mount user, and its passing UNMODIFIED is a back-compat
//! proof obligation of this lane (`plans/self-iterating-harness/21-c2-scope-trees.md`
//! §5.3).
//!
//! # What each test proves
//!
//! - [`locked_decision_4_holds_through_the_real_compile_path`] — all four
//!   name-visibility clauses of locked decision 4, as VALUES rather than as
//!   GHC error strings: ROOT defines `helper` (and a never-shadowed `shared`),
//!   two sibling scopes are minted, one sibling defines its own `helper`, and
//!   the OTHER sibling's FIRST use of the name still sees ROOT's body. That
//!   ordering is the whole point: a scope whose decl tip is seeded from the
//!   global log tip rather than from its PARENT's tip picks up whatever a
//!   sibling pushed in between. Each scope's probe returns its own number, so
//!   a leak shows up as a wrong value, not as a brittle error-message match —
//!   the same proof shape `tidepool-runtime/tests/session_decl_scope_tree.rs`
//!   uses one layer down.
//! - [`escaped_closure_outlives_its_childs_window_and_scope`] — the crown
//!   jewel. A function-bearing value finalized by a node scoped to child `C`
//!   is mounted (C1's seam, pointed across a scope boundary) into a binding in
//!   the PARENT scope `P`. Retiring `C` releases `C`'s own bindings' roots and
//!   leaves the escapee's registered, because the surviving parent-scope entry
//!   solely owns that slot; the captured child-heap objects stay traced
//!   transitively through it. Then `C`'s producing WINDOW ends too (realm
//!   close), and a brand-new node scoped to `P` still CALLS the closure and
//!   gets the right answer.
//! - [`multiple_mounts_in_one_window`] — two mounts live in ONE scope at once:
//!   the several-function-field record `Toolkit` (every function field called
//!   SEPARATELY, plus its ordinary `Int` field read — a sentinel substituted
//!   for any one field would case-trap at THAT call, not at the crossing) and
//!   the top-level function-typed `Focus`, called through `runFocus`.
//!
//! # Not proved here (deliberately)
//!
//! "Every sibling branch reports the same parent snapshot digest" is lane C2
//! §4 and is ALREADY PINNED in `tests/companion_snapshots.rs`
//! (`siblings_share_one_frozen_prefix_byte_stably`, which re-digests each
//! CHILD's own assembled prefix). It is not duplicated here; this file is
//! about the NAME/heap half of C2, that one about the CONTEXT half.
//!
//! # Root accounting: four classes, never folded (and the honest bound)
//!
//! Every count read here is a distinct ownership class, per
//! `tidepool-codegen/CLAUDE.md` § root accounting:
//!
//! | # | class | read | scope retirement does |
//! |---|---|---|---|
//! | 1 | parked continuations | `stowed_roots_count() == parked_count()` | NOTHING |
//! | 2 | handle registry | `value_handle_count()` | NOTHING |
//! | 3 | value-plane bindings | `scope_binding_count(scope)` | frame → 0 |
//! | 4 | GC root ledger | `persistent_roots_count()` | −`roots_released` |
//!
//! Classes 1 and 2 being UNCHANGED is asserted, not assumed: a retirement that
//! quietly dropped a parked frame's root, or a handle, would otherwise read as
//! a bigger win than it is. Class 1's baseline in the crown-jewel test is
//! deliberately NON-ZERO (the producer is still parked on its finalize hole
//! when the child scope retires), so "untouched" is a real observation rather
//! than `0 == 0`.
//!
//! **Deregistered is NOT reclaimed.** What retirement does to class 4 is
//! remove roots from the GC TRACE LIST. It does not free `OldSpace` bytes —
//! `OldSpace::slots` is push-only, its cursor monotone, and no major or
//! compacting pass exists anywhere in the tree — so the retired binding's
//! tenured residue lives until the machine drops (or rotates). Stated plainly
//! so nobody re-derives it while hunting a leak: a long-resident session's old
//! space grows monotonically with the TOTAL number of mounts ever made,
//! bounded per turn; retirement caps what stays TRACED (and therefore what a
//! collection must walk), not what stays ALLOCATED. Same bound recorded in
//! `tidepool-codegen/CLAUDE.md` and PRD 21's deferred list.

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use tidepool_codegen::jit_machine::RealmId;
use tidepool_codegen::scope::ScopeId;
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::harness::{AnswerContract, Session};
use tidepool_harness::log::{Actor, LogHeader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::NodeId;
use tidepool_harness::{answerer_decls, Harness, HoleRouting, TurnOutcome};
use tidepool_repr::SessionId;
use tidepool_runtime::session::SessionLib;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

fn prelude_dir() -> PathBuf {
    repo_root().join("haskell/lib")
}

/// The C2 fixture dir — `Toolkit` (several function fields + an ordinary one)
/// and `Focus` (function-typed at top level). NOT the C1 mount-spike dir.
fn scope_spike_dir() -> PathBuf {
    repo_root().join("examples/harness/scope-spike")
}

fn header(tag: &str) -> LogHeader {
    LogHeader {
        prelude_hash: format!("scope-trees-{tag}"),
        extract_fingerprint: format!("scope-trees-{tag}"),
        harness_version: "test".into(),
    }
}

fn reply(block: &str) -> RecordedReply {
    RecordedReply {
        content: format!("```haskell\n{block}\n```"),
        usage: Usage {
            input_tokens: 50,
            output_tokens: 10,
            cached_input_tokens: None,
            cache_write_tokens: None,
        },
    }
}

fn outcome_tag(o: &TurnOutcome) -> &'static str {
    match o {
        TurnOutcome::Completed { .. } => "Completed",
        TurnOutcome::Suspended { .. } => "Suspended",
        TurnOutcome::NoBlock { .. } => "NoBlock",
    }
}

/// The rendered result of a COMPLETED expression turn, decoded as JSON.
///
/// Probes here are ordinary expression turns rather than `finalize` holes on
/// purpose: [`Harness::take_finalized_value_with_table`] TERMINATES the node,
/// which (C2) also retires that node's scope — a probe must not silently
/// dismantle the scope it is probing. `Completed { rendered }` is
/// `EvalResult::to_string_pretty`, i.e. the value's JSON with an optional
/// trailing `## Warnings` section, so the JSON prefix is split off here.
fn completed_json(outcome: &TurnOutcome, what: &str) -> serde_json::Value {
    match outcome {
        TurnOutcome::Completed { rendered } => {
            let body = rendered
                .split("\n\n## Warnings")
                .next()
                .unwrap_or(rendered)
                .trim();
            serde_json::from_str(body).unwrap_or_else(|e| {
                panic!("{what}: rendered turn result is not JSON ({e}):\n{rendered}")
            })
        }
        other => panic!(
            "{what}: expected a completed turn, got {}",
            outcome_tag(other)
        ),
    }
}

/// The ONE shared session every node in a test attaches to (as its own realm
/// and its own scope). Scopes, like the C1 mount seam, are SESSION-scoped:
/// frames live on the session's `PersistentSession`, so cross-node visibility
/// needs no extra plumbing once every node shares a session id. Mirrors
/// `companion_mount_spike.rs`'s helper (itself a inline of
/// `SelfHarnessDriver::build_outer_session`), with the C2 fixture dir.
fn build_shared_session(cfg: &EngineConfig, decl_root: &std::path::Path) -> Session {
    let handler_cfg = tidepool_handlers::HandlerConfig {
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        kv_path: tidepool_runtime::paths::cache_dir().join("scope-spike-kv.json"),
        llm_model: std::env::var("TIDEPOOL_LLM_MODEL")
            .unwrap_or_else(|_| "gpt-4o-mini".to_string()),
    };
    let stack: tidepool_harness::harness::BoxedStack =
        Box::new(tidepool_handlers::build_base_stack(&handler_cfg));
    let lib = SessionLib::open(
        SessionId(0),
        decl_root,
        tidepool_mcp::session_decl_module_env(&answerer_decls(), false),
    )
    .expect("decl plane opens")
    .with_validation_include(cfg.include.clone());
    Session::unbootstrapped(
        stack,
        cfg.suspend_tag,
        cfg.effect_names.clone(),
        tidepool_mcp::CapturedOutput::new(),
        cfg.include.clone(),
        tidepool_runtime::DEFAULT_NURSERY_SIZE,
        Some(lib),
    )
}

/// Boot a harness over one shared session, with `replies` served in order.
fn boot(tag: &str, replies: Vec<RecordedReply>) -> (Arc<Harness>, SessionId, tempfile::TempDir) {
    let cfg = EngineConfig::from_decls(answerer_decls(), prelude_dir(), Some(scope_spike_dir()))
        .expect("engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let writer = LogWriter::create(
        std::env::temp_dir().join(format!(
            "companion-scope-trees-{tag}-{}.jsonl",
            std::process::id()
        )),
        &header(tag),
    )
    .expect("log writer");
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));
    let decl_root = tempfile::tempdir().expect("decl root");
    let session = build_shared_session(harness.cfg(), decl_root.path());
    let sid = harness.adopt_session(session);
    (harness, sid, decl_root)
}

/// Create a node on the shared session, in its own realm, compiling and
/// binding in `scope` (`None` = [`ScopeId::ROOT`], i.e. exactly a pre-C2
/// node). Scope, like realm, is set AFTER `force_attached` — that is what
/// creates the node's `convos` entry.
fn scoped_node(
    harness: &Harness,
    sid: SessionId,
    name: &str,
    realm: u64,
    scope: Option<ScopeId>,
) -> NodeId {
    let node = harness
        .create_root(name, "Run this turn.")
        .unwrap_or_else(|e| panic!("create {name}: {e}"));
    harness
        .force_attached(node, Actor::Operator, sid)
        .unwrap_or_else(|e| panic!("attach {name}: {e}"));
    harness.set_node_realm(node, RealmId(realm));
    if let Some(scope) = scope {
        harness.set_node_scope(node, scope);
        assert_eq!(
            harness.node_scope(node),
            scope,
            "{name}: the node must report the scope it was given"
        );
    }
    node
}

/// Mint a child scope through the shared session (the one owner of the scope
/// tree).
fn mint(harness: &Harness, sid: SessionId, parent: ScopeId, what: &str) -> ScopeId {
    harness
        .with_session(sid, |s| s.mint_scope(parent))
        .expect("session checkout")
        .unwrap_or_else(|| panic!("{what}: parent scope {parent:?} must be live"))
}

// ---------------------------------------------------------------------------
// TEST 1 — locked decision 4, end to end
// ---------------------------------------------------------------------------

/// Every clause of locked decision 4, through the REAL harness compile path
/// (model reply → extract → GHC → resident session), asserted as VALUES.
///
/// The sequence is the proof. `A` and `B` are BOTH minted before EITHER
/// defines anything, and `A` defines `helper` BEFORE `B` ever mentions the
/// name — so `B`'s first use is exactly the moment a scope seeded from the
/// global decl-log tip (rather than from its PARENT's tip) would serve `A`'s
/// body. Every probe returns its own scope's number, so a leak is a wrong
/// value, never a brittle GHC-error match.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn locked_decision_4_holds_through_the_real_compile_path() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    // Distinguishable bodies: `helper 1` is 101 at ROOT, 11 in A, 2 in B —
    // pairwise distinct, so no probe can pass by accident.
    let replies = vec![
        // (i) ROOT: the name both children will shadow, plus one they never do.
        reply(
            "helper :: Int -> Int\n\
             helper x = x + 100\n\n\
             shared :: Int -> Int\n\
             shared x = x + 1000",
        ),
        // (iii) A defines its OWN helper — BEFORE B has ever used the name.
        reply("helper :: Int -> Int\nhelper x = x + 10"),
        // (iv) B's FIRST use of `helper`. Must be ROOT's body (101), not A's.
        reply("pure ([helper 1, shared 1] :: [Int])"),
        // (v) B defines its own.
        reply("helper :: Int -> Int\nhelper x = x * 2"),
        // (v) B now sees B's.
        reply("pure ([helper 1, shared 1] :: [Int])"),
        // (vi) A still sees A's.
        reply("pure ([helper 1, shared 1] :: [Int])"),
        // (vii) ROOT still sees ROOT's — it never gained a child declaration.
        reply("pure ([helper 1, shared 1] :: [Int])"),
    ];
    let (harness, sid, _decl_root) = boot("decision4", replies);

    // (i) A ROOT-scoped node (no scope set at all — a pre-C2 node) declares.
    let root_node = scoped_node(&harness, sid, "scope-root", 1, None);
    assert_eq!(
        harness.node_scope(root_node),
        ScopeId::ROOT,
        "back-compat: a node that was never given a scope compiles at ROOT"
    );
    let out = harness
        .run_to_hole_or_done(root_node)
        .await
        .expect("ROOT declares helper + shared");
    assert!(
        matches!(out, TurnOutcome::Completed { .. }),
        "ROOT's decl turn must complete, got {}",
        outcome_tag(&out)
    );

    // (ii) BOTH siblings minted BEFORE either defines anything. This ordering
    // is the point: a child's decl tip is seeded from its PARENT's tip at MINT
    // time, so a sibling's later define cannot leak in.
    let scope_a = mint(&harness, sid, ScopeId::ROOT, "scope A");
    let scope_b = mint(&harness, sid, ScopeId::ROOT, "scope B");
    assert_ne!(
        scope_a, scope_b,
        "sibling scopes are distinct, monotone ids — never reused"
    );

    let a_node = scoped_node(&harness, sid, "scope-a", 2, Some(scope_a));
    let b_node = scoped_node(&harness, sid, "scope-b", 3, Some(scope_b));

    // (iii) A defines its own `helper` — while B has not yet mentioned it.
    let out = harness
        .run_to_hole_or_done(a_node)
        .await
        .expect("A declares its own helper");
    assert!(
        matches!(out, TurnOutcome::Completed { .. }),
        "A's decl turn must complete, got {}",
        outcome_tag(&out)
    );

    // (iv) THE LEAK TEST. B's FIRST use of `helper`, after A already defined
    // one. It must resolve to ROOT's body.
    let out = harness
        .run_to_hole_or_done(b_node)
        .await
        .expect("B's first use of helper");
    assert_eq!(
        completed_json(&out, "B's first use of helper"),
        serde_json::json!([101, 1001]),
        "decision 4, 'siblings never collide' + 'children read parent declarations': \
         B's FIRST use of `helper` — after sibling A defined its own and before B \
         defined anything — must be ROOT's body (1 + 100 = 101). A's body (11) here \
         means B's decl tip was seeded from the global log tip instead of from its \
         PARENT's, and A's turn leaked across the sibling boundary"
    );

    // (v) B defines its own, and a later B probe sees B's.
    let out = harness
        .follow_up(b_node, "Define your own helper.")
        .await
        .expect("B declares its own helper");
    assert!(
        matches!(out, TurnOutcome::Completed { .. }),
        "B's decl turn must complete, got {}",
        outcome_tag(&out)
    );
    let out = harness
        .follow_up(b_node, "Use helper again.")
        .await
        .expect("B re-probes helper");
    assert_eq!(
        completed_json(&out, "B's second use of helper"),
        serde_json::json!([2, 1001]),
        "decision 4, 'children write locally': after B's own define, B's `helper` is \
         B's body (1 * 2 = 2) — it shadows the inherited ROOT one in B's frame alone"
    );

    // (vi) A still sees A's — B's define did not reach the sibling.
    let out = harness
        .follow_up(a_node, "Use helper.")
        .await
        .expect("A probes helper");
    assert_eq!(
        completed_json(&out, "A's use of helper"),
        serde_json::json!([11, 1001]),
        "decision 4, 'siblings shadow freely, never collide': A's `helper` is still \
         A's body (1 + 10 = 11) after B defined a different one at the same depth"
    );

    // (vii) ROOT still sees ROOT's — the parent never gained a child decl.
    let out = harness
        .follow_up(root_node, "Use helper.")
        .await
        .expect("ROOT probes helper");
    assert_eq!(
        completed_json(&out, "ROOT's use of helper"),
        serde_json::json!([101, 1001]),
        "decision 4, 'the parent never gains child declarations by name': after BOTH \
         children defined their own `helper`, ROOT's is still ROOT's body \
         (1 + 100 = 101) — nothing ever walks downward"
    );

    // (viii) `shared` — defined only at ROOT, never shadowed — answered 1001 in
    // EVERY probe above: ROOT's, A's and B's. That is decision 4's 'children
    // read parent declarations' clause, and it is the SECOND element of each
    // assertion above rather than three extra GHC compiles.
    assert_eq!(
        harness.node_scope(a_node),
        scope_a,
        "A's node kept its scope across every turn it ran"
    );
    assert_eq!(
        harness.node_scope(b_node),
        scope_b,
        "B's node kept its scope across every turn it ran"
    );
}

// ---------------------------------------------------------------------------
// TEST 2 — the crown jewel
// ---------------------------------------------------------------------------

/// A closure produced in child scope `C` and mounted into parent scope `P`
/// stays callable after `C`'s bindings are retired AND after the producing
/// window's realm closes.
///
/// Shape (non-degenerate on purpose): `P` minted off ROOT, `C` minted off `P`;
/// the producer node is scoped to `C`; the mount lands in `P` via
/// [`ResidentSession::mount_handle_in`]; a SECOND `C`-scoped node makes an
/// ordinary bind of its own, so `C` owns a sole-owner root for retirement to
/// release. Then, in order: retire `C` (accounting), end the producing window
/// (realm close), call the escapee from a brand-new `P`-scoped node.
///
/// (a)–(d) are the accounting that makes (e) honest; (e) is the acceptance.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn escaped_closure_outlives_its_childs_window_and_scope() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let replies = vec![
        // Producer, scoped to C: finalizes a function-bearing value.
        reply(
            "import HarnessTypes (Toolkit (..))\n\n\
             finalize @Toolkit (Toolkit { bumpBy = \\x -> x + 1\n\
             \x20                       , scaleBy = \\x -> x * 3\n\
             \x20                       , clampAt = \\x -> if x > 5 then 5 else x\n\
             \x20                       , toolkitTag = 77 }) :: M ()",
        ),
        // Placeholder bind in P — mints the real `Val.G<g>` iface for the name.
        reply(
            "import HarnessTypes (Toolkit (..))\n\n\
             escapee <- pure (Toolkit { bumpBy = id, scaleBy = id, clampAt = id, toolkitTag = 0 })",
        ),
        // A C-scoped node's OWN ordinary bind: the sole-owner root retirement
        // must release.
        reply("childLocal <- pure (7 :: Int)"),
        // Consumer, scoped to P, AFTER C retired and the producer's window
        // closed: calls the escapee.
        reply(
            "import HarnessTypes (Toolkit (..))\n\n\
             pure ([escapee.bumpBy 41, escapee.scaleBy 14, escapee.toolkitTag] :: [Int])",
        ),
    ];
    let (harness, sid, _decl_root) = boot("escapee", replies);

    let scope_p = mint(&harness, sid, ScopeId::ROOT, "parent scope P");
    let scope_c = mint(&harness, sid, scope_p, "child scope C");

    // --- The child window produces a function-bearing value. ---
    let producer = scoped_node(&harness, sid, "escapee-producer", 1, Some(scope_c));
    harness.set_answer_contract(
        producer,
        Some(AnswerContract {
            ty: "Toolkit".to_string(),
            imports: vec!["HarnessTypes (Toolkit (..))".to_string()],
        }),
    );
    let out = harness
        .run_to_hole_or_done(producer)
        .await
        .expect("the producer drives to its finalize hole");
    let producer_hole = match &out {
        TurnOutcome::Suspended { hole, classified } => {
            assert!(
                matches!(
                    &classified.routing,
                    HoleRouting::Finalize { ty, .. } if ty.as_deref() == Some("Toolkit")
                ),
                "the producer must suspend on a Finalize Toolkit hole, got {:?}",
                classified.routing
            );
            hole.clone()
        }
        other => panic!(
            "the producer must suspend at finalize, got {}",
            outcome_tag(other)
        ),
    };

    let handle = harness
        .with_session(sid, |s| s.finalized_handle(&producer_hole))
        .expect("session checkout")
        .expect(
            "the child's finalize hole must carry a live closure handle — if this is None, \
             `Toolkit`'s mixed function/non-function shape did not classify as a closure \
             payload, which is a REAL FINDING about the seam, not a fixture problem",
        );
    assert_eq!(
        harness
            .with_session(sid, |s| s.value_handle_count())
            .expect("session checkout"),
        1,
        "accounting class 2: minting the handle is this class's one new entry"
    );

    // --- The mount: install it in the PARENT scope. The placeholder node is
    // itself P-scoped, so the throwaway iface and the real mount land in the
    // SAME frame (exactly C1's shape, one scope deeper). ---
    let placeholder = scoped_node(&harness, sid, "escapee-placeholder", 2, Some(scope_p));
    let out = harness
        .run_to_hole_or_done(placeholder)
        .await
        .expect("the placeholder bind runs");
    assert!(
        matches!(out, TurnOutcome::Completed { .. }),
        "the placeholder bind must complete synchronously, got {}",
        outcome_tag(&out)
    );
    harness
        .with_session(sid, |s| s.mount_handle_in(scope_p, "escapee", handle))
        .expect("session checkout")
        .expect("mount succeeds: the handle was live");
    assert_eq!(
        harness
            .with_session(sid, |s| s.value_handle_count())
            .expect("session checkout"),
        0,
        "accounting class 2: the handle transfers OUT to the value plane at the mount — \
         the escapee's root is now owned by P's FRAME, not by the handle registry"
    );
    assert_eq!(
        harness
            .with_session(sid, |s| s.scope_binding_count(scope_p))
            .expect("session checkout"),
        1,
        "accounting class 3: the mount is one value-plane binding, in P's frame"
    );

    // --- The child scope gets a binding of its OWN, so retirement has a
    // sole-owner root to release (a degenerate empty scope would prove
    // nothing). ---
    let child_worker = scoped_node(&harness, sid, "escapee-child-worker", 3, Some(scope_c));
    let out = harness
        .run_to_hole_or_done(child_worker)
        .await
        .expect("the child-scoped bind runs");
    assert!(
        matches!(out, TurnOutcome::Completed { .. }),
        "the child's own bind must complete synchronously, got {}",
        outcome_tag(&out)
    );
    assert_eq!(
        harness
            .with_session(sid, |s| s.scope_binding_count(scope_c))
            .expect("session checkout"),
        1,
        "accounting class 3: the child scope owns exactly one binding of its own"
    );

    // --- Baseline across ALL FOUR classes, immediately before retirement. ---
    let (roots_before, parked_before, stowed_before, handles_before) = harness
        .with_session(sid, |s| {
            (
                s.persistent_roots_count(),
                s.parked_count(),
                s.stowed_roots_count(),
                s.value_handle_count(),
            )
        })
        .expect("session checkout");
    assert_eq!(
        stowed_before, parked_before,
        "accounting class 1's own invariant: stowed roots == parked frames"
    );
    assert_eq!(
        parked_before, 1,
        "class 1's baseline is deliberately NON-ZERO — the producer is STILL parked on \
         its finalize hole — so 'a scope retirement leaves class 1 untouched' is a real \
         observation below, not 0 == 0"
    );
    assert_eq!(handles_before, 0, "class 2's baseline: nothing outstanding");

    // --- THE RETIREMENT. `terminate_node` is the one retirement path; the
    // node is C-scoped, so this exits both halves of its window (realm close,
    // then scope retirement). ---
    harness
        .terminate_node(child_worker, "child window done")
        .expect("the child-scoped node retires");

    let (roots_after, parked_after, stowed_after, handles_after, child_frame, escapee_after) =
        harness
            .with_session(sid, |s| {
                (
                    s.persistent_roots_count(),
                    s.parked_count(),
                    s.stowed_roots_count(),
                    s.value_handle_count(),
                    s.scope_binding_count(scope_c),
                    s.current_binding_in(scope_p, "escapee").is_some(),
                )
            })
            .expect("session checkout");

    // (a) class 3 returns to baseline for the retired scope.
    assert_eq!(
        child_frame, 0,
        "(a) accounting class 3: retiring the child scope returns its frame to 0 — the \
         child's names no longer resolve anywhere"
    );
    // (b) class 4 moved by exactly one root: the child's own binding was its
    // sole owner, so retirement released precisely that root.
    assert_eq!(
        roots_before - roots_after,
        1,
        "(b) accounting class 4: the GC root ledger dropped by exactly one root \
         ({roots_before} -> {roots_after}) — the child's binding solely owned its root, \
         so retirement released it and nothing else"
    );
    // (c) classes 1 and 2 untouched.
    assert_eq!(
        (parked_after, stowed_after),
        (parked_before, stowed_before),
        "(c) accounting class 1 UNCHANGED: a parked continuation's root belongs to a \
         REALM, not to a scope — scope retirement must not touch it (and folding the \
         two classes together is how a leak becomes invisible)"
    );
    assert_eq!(
        handles_after, handles_before,
        "(c) accounting class 2 UNCHANGED: the handle registry is not where a mounted \
         root lives — the mount already transferred ownership out of it"
    );
    // (d) the escapee's root survived: it is P's, not C's.
    assert!(
        escapee_after,
        "(d) the escaped closure's binding is in the PARENT's frame and solely owns its \
         root, so the child's retirement left it registered — its captured child-heap \
         objects stay traced transitively through it (reachability, exactly as locked \
         decision 4 words it)"
    );

    // --- The producing WINDOW ends too: the child node's realm closes, taking
    // its parked finalize frame with it. That is a REALM effect on class 1 (it
    // is what the class counts) — asserted here so the class-1 movement is
    // attributed to the right owner, and so the acceptance below happens after
    // BOTH halves of the child's window are gone. ---
    harness
        .terminate_node(producer, "producing window closed")
        .expect("the producer retires");
    let (roots_final, parked_final, handles_final) = harness
        .with_session(sid, |s| {
            (
                s.persistent_roots_count(),
                s.parked_count(),
                s.value_handle_count(),
            )
        })
        .expect("session checkout");
    assert_eq!(
        parked_final,
        parked_before - 1,
        "the producer's realm close dropped its parked frame — accounting class 1 moves \
         for a REALM reason, never for a scope one"
    );
    assert_eq!(
        handles_final, 0,
        "no stray handle ever accumulated outside the one mount transfer"
    );
    // The producer shares scope_c with child_worker, already retired above —
    // retiring an ALREADY-retired scope is a no-op, so the GC root ledger
    // does not move again: the release is counted once, by the retirement
    // that made it.
    assert_eq!(
        roots_final, roots_after,
        "retiring an already-retired scope must not release a second root"
    );

    // --- (e) THE ACCEPTANCE: a brand-new node, scoped to the surviving
    // parent, compiles a turn that CALLS the escaped closure. ---
    let consumer = scoped_node(&harness, sid, "escapee-consumer", 4, Some(scope_p));
    let out = harness
        .run_to_hole_or_done(consumer)
        .await
        .expect("the consumer turn compiles and runs in the parent scope");
    assert_eq!(
        completed_json(&out, "the consumer's call into the escapee"),
        serde_json::json!([42, 42, 77]),
        "(e) THE CROWN JEWEL: a closure returned from a child is STILL CALLABLE after \
         that child's window ended AND its scope retired — every function field \
         evaluates against the live captured heap (41 + 1, 14 * 3), and the ordinary \
         field still reads 77"
    );
}

// ---------------------------------------------------------------------------
// TEST 3 — several mounts, one window
// ---------------------------------------------------------------------------

/// TWO mounts live in ONE scope at the same time, both callable from a turn
/// compiled in it: the several-function-field record `Toolkit` and the
/// top-level function-typed `Focus`.
///
/// Every one of `Toolkit`'s function fields is called SEPARATELY, and its
/// ordinary `Int` field is read: a sentinel substituted for any single field
/// would case-trap at THAT call rather than at the crossing, so one aggregate
/// call could pass while three fields were broken.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multiple_mounts_in_one_window() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let replies = vec![
        // Producer 1: the several-function-field record.
        reply(
            "import HarnessTypes (Toolkit (..))\n\n\
             finalize @Toolkit (Toolkit { bumpBy = \\x -> x + 1\n\
             \x20                       , scaleBy = \\x -> x * 3\n\
             \x20                       , clampAt = \\x -> if x > 5 then 5 else x\n\
             \x20                       , toolkitTag = 77 }) :: M ()",
        ),
        // Producer 2: the lens-shaped, top-level function-typed one.
        reply(
            "import HarnessTypes (Focus (..))\n\n\
             finalize @Focus (Focus { runFocus = \\f x -> f (f x) }) :: M ()",
        ),
        // Placeholder binds, both in the SAME window scope.
        reply(
            "import HarnessTypes (Toolkit (..))\n\n\
             toolkit <- pure (Toolkit { bumpBy = id, scaleBy = id, clampAt = id, toolkitTag = 0 })",
        ),
        reply(
            "import HarnessTypes (Focus (..))\n\nfocus <- pure (Focus { runFocus = \\f x -> f x })",
        ),
        // One turn, both mounts, every field called separately.
        reply(
            "import HarnessTypes (Toolkit (..), Focus (..))\n\n\
             pure ([ toolkit.bumpBy 1\n\
             \x20   , toolkit.scaleBy 2\n\
             \x20   , toolkit.clampAt 9\n\
             \x20   , toolkit.toolkitTag\n\
             \x20   , focus.runFocus (\\x -> x + 5) 0 ] :: [Int])",
        ),
    ];
    let (harness, sid, _decl_root) = boot("multimount", replies);

    // ONE window = one scope. Both mounts land in it.
    let window = mint(&harness, sid, ScopeId::ROOT, "the window scope");

    // --- Both producers, each its own realm, both scoped to the window. ---
    let mut handles = Vec::new();
    for (i, (name, ty, import)) in [
        (
            "multimount-toolkit",
            "Toolkit",
            "HarnessTypes (Toolkit (..))",
        ),
        ("multimount-focus", "Focus", "HarnessTypes (Focus (..))"),
    ]
    .into_iter()
    .enumerate()
    {
        let producer = scoped_node(&harness, sid, name, 1 + i as u64, Some(window));
        harness.set_answer_contract(
            producer,
            Some(AnswerContract {
                ty: ty.to_string(),
                imports: vec![import.to_string()],
            }),
        );
        let out = harness
            .run_to_hole_or_done(producer)
            .await
            .unwrap_or_else(|e| panic!("{name} drives to its finalize hole: {e}"));
        let hole = match &out {
            TurnOutcome::Suspended { hole, classified } => {
                assert!(
                    matches!(
                        &classified.routing,
                        HoleRouting::Finalize { ty: Some(t), .. } if t == ty
                    ),
                    "{name} must suspend on a Finalize {ty} hole, got {:?}",
                    classified.routing
                );
                hole.clone()
            }
            other => panic!(
                "{name} must suspend at finalize, got {}",
                outcome_tag(other)
            ),
        };
        let handle = harness
            .with_session(sid, |s| s.finalized_handle(&hole))
            .expect("session checkout")
            .unwrap_or_else(|| {
                panic!(
                    "{name}: `{ty}` finalized no live closure handle. For `Toolkit` that \
                     would mean its MIXED function/non-function record shape does not \
                     classify as a closure payload — a REAL FINDING about the seam's \
                     transitive walk, to be reported rather than fixtured around"
                )
            });
        handles.push(handle);
    }
    assert_eq!(
        harness
            .with_session(sid, |s| s.value_handle_count())
            .expect("session checkout"),
        2,
        "accounting class 2: two live handles, one per producer, before either mounts"
    );

    // --- Two placeholder binds in the SAME window scope, then two mounts. ---
    for (i, node_name) in ["multimount-ph-toolkit", "multimount-ph-focus"]
        .into_iter()
        .enumerate()
    {
        let ph = scoped_node(&harness, sid, node_name, 10 + i as u64, Some(window));
        let out = harness
            .run_to_hole_or_done(ph)
            .await
            .unwrap_or_else(|e| panic!("{node_name} placeholder bind runs: {e}"));
        assert!(
            matches!(out, TurnOutcome::Completed { .. }),
            "{node_name}'s placeholder bind must complete synchronously, got {}",
            outcome_tag(&out)
        );
    }

    for (handle, binding) in handles.into_iter().zip(["toolkit", "focus"]) {
        harness
            .with_session(sid, |s| s.mount_handle_in(window, binding, handle))
            .expect("session checkout")
            .unwrap_or_else(|e| panic!("mounting `{binding}` succeeds: {e}"));
    }

    assert_eq!(
        harness
            .with_session(sid, |s| s.value_handle_count())
            .expect("session checkout"),
        0,
        "accounting class 2: BOTH handles transferred out to the window's frame"
    );
    let mut names = harness
        .with_session(sid, |s| s.binding_names_in(window))
        .expect("session checkout");
    names.sort();
    assert_eq!(
        names,
        vec!["focus".to_string(), "toolkit".to_string()],
        "both mounts are visible in the SAME window scope at the same time — multiple \
         mounts per window, not one"
    );
    assert_eq!(
        harness
            .with_session(sid, |s| s.scope_binding_count(window))
            .expect("session checkout"),
        2,
        "accounting class 3: the window's own frame owns both mounts"
    );

    // --- One turn compiled in that window calls BOTH, field by field. ---
    let consumer = scoped_node(&harness, sid, "multimount-consumer", 20, Some(window));
    let out = harness
        .run_to_hole_or_done(consumer)
        .await
        .expect("the consumer turn compiles and runs in the window scope");
    assert_eq!(
        completed_json(&out, "both mounts, called field by field"),
        serde_json::json!([2, 6, 5, 77, 10]),
        "every mounted field survives the crossing INDIVIDUALLY: Toolkit's three \
         function fields called separately (1 + 1, 2 * 3, clamp 9 to 5), its ordinary \
         Int field read (77), and Focus's top-level function applied through runFocus \
         (twice (+5) from 0 = 10) — a sentinel substituted for any ONE of them would \
         case-trap at that call, not at the crossing"
    );
}
