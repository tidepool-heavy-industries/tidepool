//! PRD 21 lane C1 — THE mount spike: prove a function-bearing value
//! finalized by one model window (P) can be MOUNTED into a later window (C)
//! as a named invocation-local binding and called directly, across P's own
//! retirement and across C's own mid-sequence suspension (the GC window a
//! naive root would be lost in), with root ownership explicitly accounted.
//!
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH — run inside
//! `nix develop` (see `haskell/CLAUDE.md`).
//!
//! # The sequence
//!
//! 1. **P** (a `Finalize Mounted`-contracted answerer node, attached to a
//!    session it shares with every other node in this test) finalizes
//!    `Mounted { applyMounted = \x -> x + 1 }` and suspends. The runtime
//!    mints a [`tidepool_codegen::jit_machine::ValueHandle`] over the
//!    payload ([`ResidentSession::finalized_handle`]) — the SAME primitive
//!    the fn-finalize-spike suite uses to deliver a closure into a parked
//!    continuation, reached here directly rather than through the driver.
//! 2. **Z**, a throwaway node on the SAME session, runs an ordinary
//!    `mounted <- pure (Mounted { applyMounted = id })` — ordinary value-
//!    plane bind machinery already used for `x <- fork …` results. Its OWN
//!    tenured value is thrown away; what matters is that it mints a REAL
//!    `Tidepool.Session.Val.G<g>` GHC interface + `SessionVarId` under the
//!    name `mounted`, which a later turn on ANY node sharing this session
//!    already resolves through the existing `session_bind_context`
//!    plumbing — no new GHC-facing mechanism.
//! 3. The runtime redirects that binding's root to point at P's real handle
//!    ([`ResidentSession::mount_handle`]) — the mount seam itself: "a handle
//!    installed under a name in a window's declaration scope" (PRD 21's
//!    substrate-mapping sketch), the closure-tenure-then-handle path pointed
//!    the OTHER direction. P then retires (`terminate_node`) — its realm
//!    closes, but the handle was already transferred OUT before that, so the
//!    mount survives P's death.
//! 4. **C**, a brand-new node forced AFTER the mount (never sees `mounted`
//!    established — it just opens with the name already resolvable),
//!    compiles a turn that parks on `askUser` (the GC-risk window — a
//!    genuine suspend/resume cycle sits between "the name resolves" and
//!    "the closure is called"), resumes, and evaluates
//!    `mounted.applyMounted n == 42` — an ordinary reference, no `ReadInput`
//!    effect, no continuation-resume delivery.
//!
//! # Root accounting (the anti-pattern this pins)
//!
//! A mounted binding is a NEW ownership class, distinct from a parked
//! continuation's stowed root: [`ResidentSession::value_handle_count`] goes
//! 0 → 1 when P's handle is minted and back to 0 the instant
//! [`ResidentSession::mount_handle`] transfers it out — the handle registry
//! is NOT where a mounted root lives. [`ResidentSession::binding_names`]
//! (the value plane) is: it goes 0 → 1 at the SAME mutation and — like any
//! other session value-plane binding (`x <- fork …`, a living decl-plane
//! helper) — is a session-lifetime root by design, not something this
//! sequence's own cleanup is expected to zero out; what this test pins is
//! that the transition is accounted at both registries' natural mutation
//! points, never silently folded into the parked-continuation count.

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;
use tidepool_codegen::jit_machine::RealmId;
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::harness::{AnswerContract, Session};
use tidepool_harness::log::{Actor, LogHeader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::{typed_request_agent_decls, Harness, SuspensionRouting, TurnOutcome};
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

fn mount_spike_dir() -> PathBuf {
    repo_root().join("examples/harness/mount-spike")
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "mount-spike".into(),
        extract_fingerprint: "mount-spike".into(),
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

/// Build the ONE shared session P/Z/C all attach to (as distinct realms) —
/// the mount seam is SESSION-scoped (`Val.G<g>` bindings live on
/// `PersistentSession`, not per-node), so cross-node visibility needs no
/// extra plumbing once every node shares a session id. Mirrors
/// `SelfHarnessDriver::build_outer_session`/`node_decl_plane`, inlined here
/// since this spike drives nodes directly rather than through the driver.
fn build_shared_session(cfg: &EngineConfig, decl_root: &std::path::Path) -> Session {
    let handler_cfg = tidepool_handlers::HandlerConfig {
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        kv_path: tidepool_runtime::paths::cache_dir().join("mount-spike-kv.json"),
        llm_model: std::env::var("TIDEPOOL_LLM_MODEL")
            .unwrap_or_else(|_| "gpt-4o-mini".to_string()),
    };
    let stack: tidepool_harness::harness::BoxedStack =
        Box::new(tidepool_handlers::build_base_stack(&handler_cfg));
    let lib = SessionLib::open(
        tidepool_repr::SessionId(0),
        decl_root,
        tidepool_mcp::session_decl_module_env(&typed_request_agent_decls(), false),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mounted_closure_survives_retirement_and_suspension() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let cfg = EngineConfig::from_decls(
        typed_request_agent_decls(),
        prelude_dir(),
        Some(mount_spike_dir()),
    )
    .expect("engine config");

    let replies = vec![
        // P: finalizes the function-bearing value.
        reply(
            "import HarnessTypes (Mounted (..))\n\n\
             finalize @Mounted (Mounted {applyMounted = \\x -> x + 1}) :: M ()",
        ),
        // Z: a THROWAWAY same-type placeholder bind — mints the real
        // `Val.G<g>` iface/SessionVarId under the name `mounted`. Its own
        // tenured value is discarded: the mount step below redirects the
        // SAME binding to P's real handle before anything ever reads it.
        reply(
            "import HarnessTypes (Mounted (..))\n\nmounted <- pure (Mounted {applyMounted = id})",
        ),
        // C: parks on askUser (the GC-risk window between "the name
        // resolves" and "the closure is called"), resumes, calls `mounted`
        // by ordinary reference — no ReadInput effect, no handle delivery.
        reply(
            "import HarnessTypes (Mounted (..))\n\n\
             do\n  n <- askUser @Int\n  finalize @Bool (mounted.applyMounted n == 42)",
        ),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let writer = LogWriter::create(
        std::env::temp_dir().join(format!(
            "companion-mount-spike-{}.jsonl",
            std::process::id()
        )),
        &header(),
    )
    .expect("log writer");
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let decl_root = tempfile::tempdir().expect("decl root");
    let session = build_shared_session(harness.cfg(), decl_root.path());
    let sid = harness.adopt_session(session);

    assert_eq!(
        harness
            .with_session(sid, |s| s.value_handle_count())
            .expect("session checkout"),
        0,
        "no handle outstanding before anything runs"
    );

    // --- Producer window: finalize Mounted. ---
    let node_p = harness
        .create_root("mount-p", "Finalize the mounted value.")
        .expect("create P");
    harness
        .force_attached(node_p, Actor::Operator, sid)
        .expect("attach P");
    harness.set_node_realm(node_p, RealmId(1));
    harness.set_answer_contract(
        node_p,
        Some(AnswerContract {
            ty: "Mounted".to_string(),
            imports: vec!["HarnessTypes (Mounted (..))".to_string()],
        }),
    );
    let outcome_p = harness
        .run_to_hole_or_done(node_p)
        .await
        .expect("P drives to a hole");
    let hole_p = match &outcome_p {
        TurnOutcome::Suspended { hole, classified } => {
            assert!(
                matches!(
                    &classified.routing,
                    SuspensionRouting::Finalize { ty, .. } if ty.as_deref() == Some("Mounted")
                ),
                "P must suspend on a Finalize Mounted hole, got {:?}",
                classified.routing
            );
            hole.clone()
        }
        other => panic!("P must suspend at finalize, got {}", outcome_tag(other)),
    };

    // Mint the handle directly off the shared session's machine — the same
    // primitive `Harness::take_finalized_handle_keep_open` uses internally,
    // reached here without the node-level wrapper since this spike composes
    // the mount from outside the crate's own turn loop.
    let handle = harness
        .with_session(sid, |s| s.finalized_handle(&hole_p))
        .expect("session checkout")
        .expect("P's finalize hole carries a live closure handle");
    assert_eq!(
        harness
            .with_session(sid, |s| s.value_handle_count())
            .expect("session checkout"),
        1,
        "minting the handle is this ownership class's ONE new root"
    );

    // --- Runtime: mint the placeholder identity, then redirect it to P's
    // real handle — the mount itself. ---
    let node_z = harness
        .create_root("mount-z", "Mint the mounted placeholder.")
        .expect("create Z");
    harness
        .force_attached(node_z, Actor::Operator, sid)
        .expect("attach Z");
    let outcome_z = harness
        .run_to_hole_or_done(node_z)
        .await
        .expect("Z's placeholder bind runs");
    assert!(
        matches!(outcome_z, TurnOutcome::Completed { .. }),
        "the placeholder bind must complete synchronously (no suspension), got {}",
        outcome_tag(&outcome_z)
    );

    harness
        .with_session(sid, |s| s.mount_handle("mounted", handle))
        .expect("session checkout")
        .expect("mount succeeds: the handle was live");
    assert_eq!(
        harness
            .with_session(sid, |s| s.value_handle_count())
            .expect("session checkout"),
        0,
        "the handle transfers OUT of the handle registry once mounted — a \
         DIFFERENT ownership class (the value plane) now owns the root"
    );
    assert_eq!(
        harness
            .with_session(sid, |s| s.binding_names().len())
            .expect("session checkout"),
        1,
        "the mounted-root count: exactly one live value-plane binding"
    );

    // Producer window ends. Its realm closes; the handle it once owned is
    // already gone from the handle registry, so this must not disturb the
    // mount.
    harness
        .terminate_node(node_p, "mounted")
        .expect("P retires");
    assert_eq!(
        harness
            .with_session(sid, |s| s.binding_names().len())
            .expect("session checkout"),
        1,
        "the mount survives the producer's own realm scope-exit"
    );

    // --- Consumer window: opens with `mounted` already resolvable by name
    // (it never establishes the binding itself), parks on an effect (the
    // GC-risk window), resumes, and calls the mounted closure. ---
    let node_c = harness
        .create_root("mount-c", "Call the mounted value after a suspension.")
        .expect("create C");
    harness
        .force_attached(node_c, Actor::Operator, sid)
        .expect("attach C");
    harness.set_node_realm(node_c, RealmId(2));
    harness.set_answer_contract(
        node_c,
        Some(AnswerContract {
            ty: "Bool".to_string(),
            imports: vec![],
        }),
    );
    let outcome_c1 = harness
        .run_to_hole_or_done(node_c)
        .await
        .expect("C drives to askUser");
    match &outcome_c1 {
        TurnOutcome::Suspended { classified, .. } => {
            assert!(
                matches!(classified.routing, SuspensionRouting::AskUser { .. }),
                "C must park on askUser first — the GC-risk window between \
                 the name resolving and the closure being called — got {:?}",
                classified.routing
            );
        }
        other => panic!("C must suspend at askUser, got {}", outcome_tag(other)),
    }

    // Resume across the GC window: this is where a naive (unrooted) mount
    // would be lost.
    harness
        .answer_dialog(node_c, json!(41))
        .await
        .expect("askUser answered");

    let finalize_hole = harness
        .pending_suspension(node_c)
        .expect("C is suspended again, now on finalize");
    assert!(
        matches!(
            &finalize_hole.routing,
            SuspensionRouting::Finalize { ty, .. } if ty.as_deref() == Some("Bool")
        ),
        "C must have resumed straight through to Finalize Bool in the SAME \
         compiled turn, got {:?}",
        finalize_hole.routing
    );

    let (value, table) = harness
        .take_finalized_value_with_table(node_c)
        .expect("C finalizes");
    assert_eq!(
        tidepool_runtime::value_to_json(&value, &table, 0),
        json!(true),
        "mounted.applyMounted 41 == 42 must evaluate true — the mounted \
         closure crossed suspension, node retirement, and a brand-new \
         window's compile, and is still the SAME live function"
    );

    // --- Everything drops; root accounting returns to baseline for every
    // PARKED-continuation-class root (the handle registry never re-gains an
    // entry the mount didn't put there). ---
    harness.terminate_node(node_c, "done").expect("C retires");
    assert_eq!(
        harness
            .with_session(sid, |s| s.value_handle_count())
            .expect("session checkout"),
        0,
        "no stray handle ever accumulated outside the one mount transfer"
    );
}
