//! THE (formerly) FAILING HALF of the nested-`async` reproducer (PRD 20
//! S1-L4) — was `#[ignore]`d and fully documented as NOT a sanctioned red,
//! now un-ignored as the fix's acceptance test. Its control — the passing
//! structural half — is
//! `tidepool-runtime/tests/green_thread_representation.rs`'s
//! `a_green_thread_can_fork_another_green_thread`. Read the two together;
//! neither means much alone. Fix:
//! `tidepool-runtime/tests/tenure_resume_gc_repro.rs`'s module doc.
//!
//! # What fails
//!
//! A green thread whose body itself calls `async` reports GC-forwarding
//! corruption, surfacing as an application-of-non-closure / case trap rather
//! than a clean error. Bisection-confirmed: removing the nesting from this
//! shape makes it pass, restoring it reproduces. Observed output, verbatim:
//!
//! ```text
//! [JIT] App: fun_ptr=0x… has tag 255 (UNKNOWN) — expected Closure!
//! [CASE TRAP] in compiled fn: loop_2_lambda_399
//!
//! panicked at resident.rs: RootCustody dropped without being consumed —
//! ValueHandle(3)'s custody was lost (never delivered via resume_handle,
//! never mounted).
//! ```
//!
//! **The custody panic is a CASCADE, not the bug.** It is the servicing path
//! unwinding past a delivery after the case trap has already fired, and the
//! token correctly reporting that a value never arrived. Chase the `tag 255`
//! line; the custody message is downstream of it. (It is still worth having:
//! without the token the lost delivery would have been silent.)
//!
//! The trap fires in `loop_2_lambda_399` — the OUTER authored loop's lambda,
//! not the nested thread body — so whatever is stale is being applied on the
//! resumption path, not inside the newly forked thread.
//!
//! # Ruled out — the fork crossing itself
//!
//! The structural control builds the SAME nesting by hand
//! (no GHC, no extract) and PASSES, including under
//! `TIDEPOOL_GC_POISON=1 TIDEPOOL_HEAP_VERIFY=1`. So these are all sound:
//! `ResidentSession::run_forked`; the sentinel-tenure of a closure at field 1
//! of a suspended request; `finalized_handle` called on a frame that
//! `run_forked` ITSELF created, while that frame is still parked; and
//! multi-level realm nesting. The bug is not in how a nested fork is
//! STRUCTURED.
//!
//! # Still suspect — a collection running while nested frames are parked
//!
//! The one variable the control does not reproduce is ALLOCATION. Its bodies
//! are hand-built and allocate almost nothing, so no collection ever runs;
//! this fixture's `mapConcurrently` over recursive sums allocates heavily.
//! Isolating that variable further needs a force-GC injection point the
//! machine does not expose publicly — deliberately not added here, since it
//! belongs with whoever owns the rooting discipline.
//!
//! # UPDATE — the same bug reaches a non-nested case, and names itself
//!
//! Wave 2 hit this WITHOUT nesting: one `forkNode`, one `sendUp`, no burst,
//! no nested `async`. Once a masking custody-drop panic was removed from the
//! spawn path (see `driver.rs`), the underlying error surfaced verbatim:
//!
//! ```text
//! AsyncSpawnWith spawner resume failed: turn run failed:
//!   heap bridge error: unexpected heap tag: 255
//! ```
//!
//! Tag 255 is FORWARDED. So the failure is: **resuming a frame whose
//! request carried a closure at field 1 that was TENURED at suspend time,
//! after enough allocation for a collection to have run.** The suspect is a
//! reference the parked frame still holds into a nursery object that tenuring
//! evacuated, read on the resumption path.
//!
//! That reframes this file. "Nested `async`" is a SYMPTOM, not the bug:
//!
//! * the nested case traps in `loop_2_lambda_399` — the outer loop's lambda,
//!   on the RESUMPTION path;
//! * the wave-2 case fails on the spawner's RESUMPTION, same tag;
//! * neither needs nesting to be explained, and one of them does not have it.
//!
//! **What makes `AsyncSpawnWith` the novel exposure:** the sentinel-tenure
//! mechanism predates green threads, but its only prior user is `finalize`,
//! whose frame is never driven onward past the tenure point — `finalize`
//! diverges by construction. `AsyncSpawnWith` is the first site that tenures
//! a field-1 closure and then RESUMES that same frame to keep running. So the
//! tenure-then-resume path is new, which is consistent with a latent gap
//! surfacing now rather than a regression.
//!
//! Why the flat wave-1 cases pass anyway: they tenure too, but their bodies
//! capture shallow data (`mapWork n = pure $! sumTo n * 10`). The failing
//! cases capture deeper closure graphs — a `NodeCtx` whose `inbox` field is
//! itself a closure — so tenuring evacuates more objects, and allocates more.
//! Graph depth and allocation volume are the two variables that separate
//! passing from failing; neither is nesting.
//!
//! # The family, and the first experiment
//!
//! This is plausibly one of three members of a single family — **values
//! crossing machine-lifecycle boundaries while a collection can move them**:
//!
//! 1. the tag255-canary bug (allocation during continuation composition
//!    without retry, fixed by extending the rooting discipline with
//!    `RootedLocal`/`RootedStack` spans — prior art AND mechanism template);
//! 2. the settle-boundary thunk bug (an unforced non-closure result crossing
//!    settle→delivery corrupted on delivery; fixed in-lane by forcing to WHNF
//!    at the settle site, documented on `Tidepool.Async`);
//! 3. this one.
//!
//! Hunt the family, not the instance. **The pointed first experiment, as
//! revised by the update above: what happens to a parked frame's own
//! references to its request when field 1 of that request is tenured, and a
//! collection then runs before the frame is resumed?** That is a narrower
//! and more reachable question than the original "what roots a nested
//! thread's not-yet-forced body", and it does not require nesting to set up
//! — one `forkNode` with a closure-capturing body is enough.
//!
//! GHC-heavy: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH.

use std::sync::Arc;

mod support;

use tidepool_handlers::ConsoleHandler;
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::DynModelProvider;
use tidepool_harness::replay::ReplayProvider;
use tidepool_harness::{
    answerer_decls, load_harness_source, Harness, LogObserver, SelfHarnessDriver,
};

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

fn fixtures_dir() -> std::path::PathBuf {
    repo_root().join("tidepool-harness/tests/fixtures")
}

/// A green thread's body forks another green thread.
///
/// Was `#[ignore]`d pending the fix — see this file's module doc for the
/// full diagnosis (tag 255 / FORWARDED, a parked frame's own reference into
/// a nursery object that a sibling tenure evacuated). Fixed by folding a
/// real minor collection into `OldSpace::tenure` itself
/// (`run_minor_collection_for_tenure_fixup`, `tidepool-codegen/src/host_fns/gc.rs`)
/// — see `tidepool-runtime/tests/tenure_resume_gc_repro.rs`'s module doc for
/// the isolated repro and mechanism.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_green_thread_body_can_fork_another_green_thread() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        repo_root().join("haskell/lib"),
        Some(fixtures_dir()),
    )
    .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(Vec::new()));
    let writer = tidepool_harness::log::LogWriter::create(
        std::env::temp_dir().join(format!("nested-async-{}.jsonl", std::process::id())),
        &LogHeader {
            prelude_hash: "nested-async".into(),
            extract_fingerprint: "nested-async".into(),
            harness_version: "test".into(),
        },
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    driver.set_console_handler(ConsoleHandler);

    let source = load_harness_source(&fixtures_dir().join("NestedAsyncHarness.hs"))
        .expect("fixture harness loads");
    let outcome = driver
        .run_one_cycle(&source, None)
        .await
        .expect("a green thread's body must be able to fork another green thread");

    let state = &outcome.state_json;
    assert_eq!(
        state.get("runs").and_then(|v| v.as_i64()),
        Some(1),
        "the loop completed exactly once, got {state:?}"
    );
    // `nestedWork n = (sumTo n * 10) + 1` over `[3, 1, 2]`, in ORIGINAL order.
    let results: Vec<i64> = state
        .get("nestedResults")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(serde_json::Value::as_i64).collect())
        .unwrap_or_default();
    assert_eq!(
        results,
        vec![61, 11, 31],
        "nested threads' results must come back in the ORIGINAL list order, got {state:?}"
    );
}
