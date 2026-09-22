//! The scope-tree acceptance suite: the scope-tree name-visibility rules proved
//! END TO END through the REAL harness compile path (model reply → extract →
//! GHC → resident session).
//!
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH — run inside
//! `nix develop` (see `bridge/haskell/CLAUDE.md`). Fixtures:
//! `examples/harness/scope-spike/HarnessTypes.hs`.
//!
//! # What each test proves
//!
//! - [`locked_decision_4_holds_through_the_real_compile_path`] — all four
//!   name-visibility clauses, as VALUES rather than as
//!   GHC error strings: ROOT defines `helper` (and a never-shadowed `shared`),
//!   two sibling scopes are minted, one sibling defines its own `helper`, and
//!   the OTHER sibling's FIRST use of the name still sees ROOT's body. That
//!   ordering is the whole point: a scope whose decl tip is seeded from the
//!   global log tip rather than from its PARENT's tip picks up whatever a
//!   sibling pushed in between. Each scope's probe returns its own number, so
//!   a leak shows up as a wrong value, not as a brittle error-message match —
//!   the same proof shape `tidepool/runtime/tests/session_decl_scope_tree.rs`
//!   uses one layer down.

use crate::support;

use std::path::PathBuf;
use std::sync::Arc;

use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::harness::Session;
use tidepool_harness::log::{Actor, LogHeader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::NodeId;
use tidepool_harness::{typed_request_agent_decls, Harness, TurnOutcome};
use tidepool_repr::SessionId;
use tidepool_runtime::session::SessionLib;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

fn prelude_dir() -> PathBuf {
    repo_root().join("bridge/haskell/lib")
}

/// The C2 fixture dir — `Toolkit` (several function fields + an ordinary one)
/// and `Focus` (function-typed at top level).
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
/// and its own scope). Scopes are SESSION-scoped:
/// frames live on the session's `PersistentSession`, so cross-node visibility
/// needs no extra plumbing once every node shares a session id. An inline of
/// `SelfHarnessDriver::build_outer_session` with the C2 fixture dir.
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
        tidepool_mcp::session_decl_module_env(&typed_request_agent_decls(), false),
    )
    .expect("decl plane opens")
    .with_validation_include(cfg.include.clone());
    Session::unbootstrapped(
        stack,
        tidepool_mcp::CapturedOutput::new(),
        tidepool_runtime::DEFAULT_NURSERY_SIZE,
        Some(lib),
    )
}

/// Boot a harness over one shared session, with `replies` served in order.
fn boot(tag: &str, replies: Vec<RecordedReply>) -> (Arc<Harness>, SessionId, tempfile::TempDir) {
    let cfg = EngineConfig::from_decls(
        typed_request_agent_decls(),
        prelude_dir(),
        Some(scope_spike_dir()),
    )
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
// TEST 1 — scope-tree name-visibility rules, end to end
// ---------------------------------------------------------------------------

/// Every scope-tree name-visibility clause, through the REAL harness compile path
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
