//! Cross-turn binding persistence on a single node: a declaration turn
//! accumulates into the node's `Tidepool.Session.Lib.G<g>` module, and a
//! later expression turn compiles session-aware (importing that module), so
//! a prior turn's declaration resolves as a live binding.
//!
//! Drives that through the REAL turn path (`drive_turn` → `run_block`) via
//! the record-replay provider: turn 1 declares `steps`, turn 2 (a follow-up)
//! computes from `steps`. The multi-block test packs the same story into ONE
//! reply (every ```haskell block runs, in order) and pins the
//! failure-mid-sequence contract.
//!
//! GHC-heavy tier: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH
//! (`--ignore-default-filter` to run).

use crate::support;

use std::path::PathBuf;
use std::sync::Arc;

use tidepool_harness::engine;
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Actor, Event, LogHeader, LogReader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::NodeState;
use tidepool_harness::{Harness, TurnOutcome};

fn prelude_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(|r| r.join("haskell/lib"))
        .unwrap_or_else(|| PathBuf::from("haskell/lib"))
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "cross-turn".into(),
        extract_fingerprint: "cross-turn".into(),
        harness_version: "test".into(),
    }
}

fn usage() -> Usage {
    Usage {
        input_tokens: 100,
        output_tokens: 20,
        cached_input_tokens: None,
        cache_write_tokens: None,
    }
}

fn reply(content: &str) -> RecordedReply {
    RecordedReply {
        content: content.to_string(),
        usage: usage(),
    }
}

/// Turn 1 DECLARES `steps`; turn 2 (a follow-up) COMPUTES from it. The value
/// only survives if the node accumulated the declaration across turns.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_declaration_persists_into_the_next_turn() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("cross_turn.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();

    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    let replies = vec![
        // Turn 1: a top-level declaration (a value binding). Classified as a
        // decl, it accumulates on the node's decl plane instead of running as an
        // expression.
        reply("I'll record the steps.\n\n```haskell\nsteps = [1, 2, 3] :: [Int]\n```"),
        // Turn 2: an expression that references the prior turn's `steps`. Only
        // resolves if turn 1's declaration persisted.
        reply("Now I'll sum them.\n\n```haskell\npure (toJSON (sum steps))\n```"),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("cross-turn root", "Record steps, then use them.")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    // Turn 1: the declaration turn completes (nothing to render — it declared).
    let turn1 = harness
        .run_to_hole_or_done(root)
        .await
        .expect("turn 1 (declaration) drives to completion");
    assert!(
        matches!(turn1, TurnOutcome::Completed { .. }),
        "the declaration turn should complete, got {}",
        outcome_tag(&turn1)
    );
    assert_eq!(harness.tree().state(root), Some(NodeState::Done));

    // Turn 2: reopen the node and reference `steps` — it must resolve as a live
    // binding carried over from turn 1.
    let turn2 = harness
        .follow_up(root, "Use the steps you recorded.")
        .await
        .expect("turn 2 (reference) drives to completion");
    match turn2 {
        TurnOutcome::Completed { rendered } => {
            assert!(
                rendered.contains('6'),
                "turn 2 must compute sum([1,2,3]) = 6 from the persisted `steps`, got: {rendered}"
            );
        }
        other => panic!(
            "turn 2 should complete with the summed value, got {}",
            outcome_tag(&other)
        ),
    }
    assert_eq!(harness.tree().state(root), Some(NodeState::Done));
}

/// The multi-block contract on the REAL turn path: every ```haskell block in
/// one reply runs, in order, as one sequence (decl block feeding the expr
/// block — the packing that previously cost a model round per block); a
/// failure mid-sequence keeps the blocks that ran (the failed reply's OWN
/// decl block persists into the corrective retry) and feeds back a
/// sequence-context corrective naming the failed block and the resume point.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_block_reply_runs_in_order_and_fails_with_resume_point() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("multi_block.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();

    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    let replies = vec![
        // Reply 1: TWO blocks — a declaration block, then an expression block
        // using it. One model round, both run.
        reply(
            "Declare, then use:\n\n```haskell\nnums = [1, 2, 3] :: [Int]\n\
             double x = x * (2 :: Int)\n```\n\n\
             ```haskell\npure (toJSON (sum (map double nums)))\n```",
        ),
        // Reply 2 (follow-up): block 1 declares; block 2 fails to compile.
        // Block 1 must persist despite block 2's failure.
        reply(
            "```haskell\ntripled = [3, 6, 9] :: [Int]\n```\n\n\
             ```haskell\npure (toJSON (thisNameDoesNotExist))\n```",
        ),
        // Reply 3: the corrective retry — references reply 2's SURVIVING
        // block-1 declaration. Only resolves if the failed sequence kept it.
        reply("```haskell\npure (toJSON (sum tripled))\n```"),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root(
            "multi-block root",
            "Pack a decl and its use into one reply.",
        )
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    // Reply 1: both blocks run in one drive — the sequence completes with the
    // LAST block's value, computed through the FIRST block's declarations.
    let turn1 = harness
        .run_to_hole_or_done(root)
        .await
        .expect("multi-block turn drives to completion");
    match &turn1 {
        TurnOutcome::Completed { rendered } => {
            assert!(
                rendered.contains("12"),
                "sum (map double nums) = 12 must come from the same reply's decl block, got: {rendered}"
            );
        }
        other => panic!("expected Completed, got {}", outcome_tag(other)),
    }
    assert_eq!(harness.tree().state(root), Some(NodeState::Done));

    // Replies 2+3: block 2 of reply 2 fails; the corrective retry (reply 3)
    // computes from reply 2's surviving block-1 declaration.
    let turn2 = harness
        .follow_up(root, "Now triple them.")
        .await
        .expect("corrective retry drives to completion");
    match &turn2 {
        TurnOutcome::Completed { rendered } => {
            assert!(
                rendered.contains("18"),
                "sum tripled = 18 must come from the FAILED reply's surviving decl block, got: {rendered}"
            );
        }
        other => panic!("expected Completed, got {}", outcome_tag(other)),
    }

    // The corrective user turn carried the sequence context: which block
    // failed, what ran and persists, and where to resume.
    let (_hdr, events) = LogReader::open(&log_path).unwrap();
    let corrective = events
        .filter_map(|e| match e.expect("readable log record").event {
            Event::TurnDelta {
                role: tidepool_harness::provider::Role::User,
                content,
                ..
            } => Some(content),
            _ => None,
        })
        .find(|c| c.contains("Block 2 of 2 failed"))
        .expect("a corrective user turn carries the sequence-failure context");
    assert!(
        corrective.contains("block 1 (tripled = [3, 6, 9] :: [Int])"),
        "the corrective names the surviving block: {corrective}"
    );
    assert!(
        corrective.contains("Continue from block 2"),
        "the corrective states the resume point: {corrective}"
    );
}

/// Multi-ITEM adoption of the block lane: ONE fenced block, TWO items separated by a blank line — a
/// helper declaration, then the answer expression that calls it. Today's
/// single-item `run_block` has no template that parses "decl, then expr" as
/// one unit; the block lane classifies both items in ONE `classify_block`
/// spawn and runs them in order, the decl committing before the expression
/// that references it compiles.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_item_block_decl_then_expr_completes_in_one_reply() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("multi_item_decl_expr.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();

    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    let replies = vec![reply(
        "```haskell\nsq :: Int -> Int\nsq x = x * x\n\npure (toJSON (sq 6))\n```",
    )];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root(
            "multi-item root",
            "Declare a helper, then use it, in one block.",
        )
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    let turn = harness
        .run_to_hole_or_done(root)
        .await
        .expect("multi-item turn drives to completion");
    match &turn {
        TurnOutcome::Completed { rendered } => {
            assert!(
                rendered.contains("36"),
                "sq 6 = 36 must come from the SAME block's helper declaration, got: {rendered}"
            );
        }
        other => panic!("expected Completed, got {}", outcome_tag(other)),
    }
    assert_eq!(harness.tree().state(root), Some(NodeState::Done));
}

/// GHCi statement semantics (2026-08-20 fix): contiguous bind lines with NO
/// blank line between them — the shape a GHCi-fluent model naturally writes
/// (`seed <- pure …`, `runningTotal <- pure …`, one per line) — must split
/// into one item PER LINE and persist as REAL session bindings, not run
/// transiently inside a single wrapped `do`-expression. Turn 1's block binds
/// `a` then `b` with no blank line anywhere in it; turn 2 (a follow-up)
/// references `b` — this only resolves if turn 1's SECOND bind genuinely
/// registered as a session binding, the exact case the old blank-line-only
/// splitter collapsed into one item and lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_item_block_contiguous_binds_persist_across_rounds() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("multi_item_contiguous_binds.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();

    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    let replies = vec![
        // Turn 1: two contiguous binds (no blank line between them), then the
        // answer expression — three items, split purely on unindented lines.
        reply("```haskell\na <- pure (1 :: Int)\nb <- pure (a + 1)\npure (toJSON (b + 1))\n```"),
        // Turn 2: references `b` — only resolves if turn 1's SECOND bind
        // persisted as a live session binding.
        reply("```haskell\npure (toJSON (b + 10))\n```"),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root(
            "contiguous-binds root",
            "Bind a, then b, with no blank line, then use b next round.",
        )
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    let turn1 = harness
        .run_to_hole_or_done(root)
        .await
        .expect("turn 1 (contiguous binds + expr) drives to completion");
    match &turn1 {
        TurnOutcome::Completed { rendered } => {
            assert!(
                rendered.contains('3'),
                "b + 1 = (a + 1) + 1 = 3 must come from the SAME block's two contiguous \
                 binds, got: {rendered}"
            );
        }
        other => panic!("expected Completed, got {}", outcome_tag(other)),
    }
    assert_eq!(harness.tree().state(root), Some(NodeState::Done));

    let turn2 = harness
        .follow_up(root, "Now use b again.")
        .await
        .expect("turn 2 (reference to b) drives to completion");
    match turn2 {
        TurnOutcome::Completed { rendered } => {
            assert!(
                rendered.contains("12"),
                "b + 10 = 12 must resolve from `b`, persisted by turn 1's SECOND \
                 contiguous bind, got: {rendered}"
            );
        }
        other => panic!(
            "turn 2 should complete referencing turn 1's persisted `b`, got {}",
            outcome_tag(&other)
        ),
    }
    assert_eq!(harness.tree().state(root), Some(NodeState::Done));
}

/// The pinned identity requirement: a block `split_block_items` finds only
/// ONE item in must cost exactly the same ONE `tidepool-extract` spawn it
/// always has — the block lane's `classify_block` pre-pass must never fire
/// for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_item_turn_still_costs_one_extract_spawn() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("single_item_spawn_count.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();

    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    let replies = vec![reply("```haskell\npure (toJSON (21 + 21))\n```")];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("single-item spawn-count root", "One bare expression.")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    // Reset AFTER boot/force (neither compiles anything), so the count below
    // is attributable to exactly this one turn.
    engine::reset_extract_spawn_count();
    let turn = harness
        .run_to_hole_or_done(root)
        .await
        .expect("single-item turn drives to completion");
    assert!(
        matches!(turn, TurnOutcome::Completed { .. }),
        "expected Completed, got {}",
        outcome_tag(&turn)
    );

    let spawns = engine::extract_spawn_count();
    assert_eq!(
        spawns, 1,
        "a single-item block must cost exactly ONE tidepool-extract spawn — got {spawns}"
    );
}

/// A multi-item block's item 1 fails to compile (an out-of-scope reference —
/// parses fine, so the batch classify itself succeeds and reports it `expr`;
/// the failure is a genuine GHC compile error): the block stops there, item 2
/// never runs, and the corrective retry gets the SAME error-surface contract
/// a single-item turn already has (`HarnessError::Compile`, fed back verbatim
/// via `run_to_hole_or_done`'s corrective loop).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_item_block_error_in_first_item_stops_and_corrects() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("multi_item_error_first.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();

    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    let replies = vec![
        // Item 1 fails (out of scope); item 2 must never run.
        reply(
            "```haskell\npure (toJSON (thisNameDoesNotExist))\n\n\
             pure (toJSON (99 :: Int))\n```",
        ),
        // The corrective retry.
        reply("```haskell\npure (toJSON (7 :: Int))\n```"),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root(
            "multi-item error root",
            "Item 1 fails to compile; item 2 must not run.",
        )
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    let turn = harness
        .run_to_hole_or_done(root)
        .await
        .expect("corrective retry drives to completion");
    match &turn {
        TurnOutcome::Completed { rendered } => {
            assert!(
                rendered.contains('7'),
                "the corrective retry's own value must win, got: {rendered}"
            );
            assert!(
                !rendered.contains("99"),
                "item 2 of the failed reply must never have run, got: {rendered}"
            );
        }
        other => panic!("expected Completed, got {}", outcome_tag(other)),
    }

    let (_hdr, events) = LogReader::open(&log_path).unwrap();
    let corrective = events
        .filter_map(|e| match e.expect("readable log record").event {
            Event::TurnDelta {
                role: tidepool_harness::provider::Role::User,
                content,
                ..
            } => Some(content),
            _ => None,
        })
        .find(|c| c.contains("thisNameDoesNotExist"))
        .expect("a corrective user turn carries item 1's GHC error verbatim");
    assert!(
        corrective.contains("did not compile"),
        "the corrective uses the same wrapper every compile-class failure gets: {corrective}"
    );
}

/// A multi-item block whose LAST item is a declaration, not something that
/// runs, is a COMPILE-CLASS failure — the model wrote a block this runner
/// rejects, so it enters the SAME corrective-retry protocol as any GHC
/// error: the window gets the trailing-decl guidance and survives on a
/// corrected reply. It used to be `HarnessError::Resident`, which every
/// driver context escalated as a turn-killing MECHANISM failure — the very
/// first exploratory window of the interaction-surface dogfood run died on
/// a stray trailing declaration (2026-08-20). This is stricter than a
/// single-item block (which may end on a bare decl today); the restriction
/// is `run_multi_item_block`-specific.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_item_block_ending_in_decl_retries_and_survives() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("multi_item_ends_in_decl.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();

    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    let replies = vec![
        // The exploratory mistake: definitions, then… nothing that runs.
        reply("```haskell\npure (toJSON (1 :: Int))\n\nsq :: Int -> Int\nsq x = x * x\n```"),
        // The corrective retry ends with the answer expression.
        reply("```haskell\nsq :: Int -> Int\nsq x = x * x\n\npure (toJSON (sq 3))\n```"),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root(
            "multi-item trailing-decl root",
            "The block's last item is a bare declaration.",
        )
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    let turn = harness
        .run_to_hole_or_done(root)
        .await
        .expect("a trailing declaration costs a corrective round, never the turn");
    match &turn {
        TurnOutcome::Completed { rendered } => {
            assert!(
                rendered.contains('9'),
                "the corrected reply's value must win, got: {rendered}"
            );
        }
        other => panic!("expected Completed, got {}", outcome_tag(other)),
    }

    let (_hdr, events) = LogReader::open(&log_path).unwrap();
    let corrective = events
        .filter_map(|e| match e.expect("readable log record").event {
            Event::TurnDelta {
                role: tidepool_harness::provider::Role::User,
                content,
                ..
            } => Some(content),
            _ => None,
        })
        .find(|c| c.contains("last item is a declaration"))
        .expect("a corrective user turn carries the trailing-decl guidance");
    assert!(
        corrective.contains("did not compile"),
        "the corrective uses the same wrapper every compile-class failure gets: {corrective}"
    );
}

/// Decl-salvage shape 1: a valid declaration runs BEFORE the item that fails
/// — the decl commits (via `define_scoped_in`) before the failing item is
/// ever compiled, so this already worked; what this pins is the ADDITIONAL
/// contract that the corrective must say so BY NAME, not just leave the
/// declaration silently alive. See `tidepool-harness/CLAUDE.md`'s "a
/// decl-ending block persists before it nudges" and the operator ruling in
/// `plans/` motivating this file's salvage note (`engine::decl_salvage_note`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_item_block_decl_before_failing_item_is_named_kept() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("multi_item_decl_before_failure.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();

    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    let replies = vec![
        // Item 1 declares `helperOne`; item 2 fails to compile. The decl run
        // commits before item 2 is ever reached.
        reply(
            "```haskell\nhelperOne :: Int\nhelperOne = 11\n\n\
             pure (toJSON (thisNameDoesNotExist))\n```",
        ),
        // The corrective retry uses the surviving declaration.
        reply("```haskell\npure (toJSON helperOne)\n```"),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root(
            "decl-before-failure root",
            "Declare, then a later item fails; the decl must persist and be named.",
        )
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    let turn = harness
        .run_to_hole_or_done(root)
        .await
        .expect("corrective retry drives to completion");
    match &turn {
        TurnOutcome::Completed { rendered } => {
            assert!(
                rendered.contains("11"),
                "the corrective retry must resolve `helperOne`, declared in the \
                 failed round, got: {rendered}"
            );
        }
        other => panic!("expected Completed, got {}", outcome_tag(other)),
    }

    let (_hdr, events) = LogReader::open(&log_path).unwrap();
    let corrective = events
        .filter_map(|e| match e.expect("readable log record").event {
            Event::TurnDelta {
                role: tidepool_harness::provider::Role::User,
                content,
                ..
            } => Some(content),
            _ => None,
        })
        .find(|c| c.contains("thisNameDoesNotExist"))
        .expect("a corrective user turn carries item 2's GHC error verbatim");
    assert!(
        corrective.contains("Declarations kept from this block")
            && corrective.contains("helperOne"),
        "the corrective must name `helperOne` as kept, got: {corrective}"
    );
}

/// Decl-salvage shape 2 (the fix): a valid declaration sits AFTER the item
/// that fails. `run_multi_item_block`'s singleton path
/// returned on the first compile error without ever visiting later items —
/// so the declaration was silently dropped and a later round referencing it
/// died on "Not in scope". Now the walk continues scanning (without running
/// anything further) so the still-valid declaration commits.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_item_block_salvages_decl_after_earlier_item_fails() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("multi_item_decl_after_failure.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();

    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    let replies = vec![
        // Item 1 fails to compile; item 2 (a valid declaration) must still
        // be committed to the decl plane.
        reply(
            "```haskell\npure (toJSON (thisNameDoesNotExist))\n\n\
             helperTwo :: Int\nhelperTwo = 22\n```",
        ),
        // The corrective retry uses the salvaged declaration.
        reply("```haskell\npure (toJSON helperTwo)\n```"),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root(
            "decl-after-failure root",
            "An earlier item fails; a later declaration must still be salvaged.",
        )
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    let turn = harness
        .run_to_hole_or_done(root)
        .await
        .expect("corrective retry drives to completion");
    match &turn {
        TurnOutcome::Completed { rendered } => {
            assert!(
                rendered.contains("22"),
                "the corrective retry must resolve `helperTwo`, salvaged from the \
                 failed round despite the earlier item's failure, got: {rendered}"
            );
        }
        other => panic!("expected Completed, got {}", outcome_tag(other)),
    }

    let (_hdr, events) = LogReader::open(&log_path).unwrap();
    let corrective = events
        .filter_map(|e| match e.expect("readable log record").event {
            Event::TurnDelta {
                role: tidepool_harness::provider::Role::User,
                content,
                ..
            } => Some(content),
            _ => None,
        })
        .find(|c| c.contains("thisNameDoesNotExist"))
        .expect("a corrective user turn carries item 1's GHC error verbatim");
    assert!(
        corrective.contains("Declarations kept from this block")
            && corrective.contains("helperTwo"),
        "the corrective must name `helperTwo` as kept despite sitting after the \
         failed item, got: {corrective}"
    );
}

/// Decl-salvage shape 3: the round-2 live-dogfood shape (2026-08-24 —
/// `dogfood-iso/verification-report.md`). A model bundles a `data` decl with
/// value-level helpers and a use of the type in ONE block; the use fails to
/// compile. The declaration (and the value binders riding with it in the
/// same run) must survive so a later round naming the type does not die on
/// "Not in scope".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_item_block_decl_then_failing_typed_use_persists_decl() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("multi_item_decl_typed_use_failure.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();

    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    let replies = vec![
        // Item 1 declares a type and a helper over it; item 2 misuses the
        // type (no `Num Track` instance) and fails to compile.
        reply(
            "```haskell\ndata Track = TrackA | TrackB\n\n\
             trackTag :: Track -> Int\ntrackTag TrackA = 1\ntrackTag TrackB = 2\n\n\
             pure (toJSON (TrackA + 1))\n```",
        ),
        // The corrective retry uses the surviving type AND helper.
        reply("```haskell\npure (toJSON (trackTag TrackA))\n```"),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root(
            "decl-then-typed-use-failure root",
            "A decl and a failing use of it are bundled in one block.",
        )
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    let turn = harness
        .run_to_hole_or_done(root)
        .await
        .expect("corrective retry drives to completion");
    match &turn {
        TurnOutcome::Completed { rendered } => {
            assert!(
                rendered.contains('1'),
                "the corrective retry must resolve `trackTag TrackA`, both declared \
                 in the failed round, got: {rendered}"
            );
        }
        other => panic!("expected Completed, got {}", outcome_tag(other)),
    }

    let (_hdr, events) = LogReader::open(&log_path).unwrap();
    let corrective = events
        .filter_map(|e| match e.expect("readable log record").event {
            Event::TurnDelta {
                role: tidepool_harness::provider::Role::User,
                content,
                ..
            } => Some(content),
            _ => None,
        })
        .find(|c| c.contains("did not compile"))
        .expect("a corrective user turn carries item 2's GHC error verbatim");
    assert!(
        corrective.contains("Declarations kept from this block") && corrective.contains("trackTag"),
        "the corrective must name `trackTag` as kept, got: {corrective}"
    );
}

fn outcome_tag(o: &TurnOutcome) -> &'static str {
    match o {
        TurnOutcome::Completed { .. } => "Completed",
        TurnOutcome::Suspended { .. } => "Suspended",
        TurnOutcome::NoBlock { .. } => "NoBlock",
    }
}
