//! `tidepool-selfharness-web` — the minimal operator GUI server.
//!
//! Serves the operator page ([`tidepool_web::shell`]) and exposes a
//! [`WebGate`](tidepool_web::WebGate) the self-iterating harness driver blocks
//! on for its operator interactions, bound to a registered node.
//!
//! `--demo` runs the server against a mock node TREE, no harness/model/API
//! calls needed, so the whole node lifecycle is reviewable on localhost:
//! - the default node ([`tidepool_web::DEFAULT_NODE_ID`]) narrates, presents
//!   one form, then presents the between-turns gate (an ordinary form, same
//!   as any other) — the loop node's shape.
//! - `root/1-finishes` walks a full successful lifecycle: seed → notes →
//!   an ask → finalized value (section flips to done).
//! - `root/2-concurrent` presents TWO forms CONCURRENTLY (two threads, each
//!   blocked in its own `present_form` call, like two concurrent cognition
//!   windows) — neither supersedes the other, each resolves independently.
//! - `root/3-fails` gets a seed and then fails (round exhaustion), showing
//!   the failure block.
//!
//! Real-driver wiring lives in the sibling `tidepool-selfharness` binary,
//! which boots the same server via
//! [`tidepool_web::spawn_operator_server_multi`] and wires the returned gate
//! into `SelfHarnessDriver`.

#![warn(clippy::unwrap_used, clippy::expect_used)]
use std::sync::Arc;

use clap::Parser;
use tidepool_harness::selfharness::operator::{FieldShape, FormShape, OperatorGate};
use tidepool_web::WebGate;

#[derive(Parser)]
struct Args {
    /// Run against a mock node tree, no harness/model/API calls needed.
    #[arg(long)]
    demo: bool,
    #[arg(long, default_value_t = 4601)]
    port: u16,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let demo = args.demo;
    let port = args.port;

    let (state, gate) = tidepool_web::spawn_operator_server_multi(port).await?;

    if demo {
        state.set_run_id("demo");
        let loop_gate = gate.clone();
        std::thread::spawn(move || demo_loop_single(loop_gate));
        let tree_gate = gate;
        std::thread::spawn(move || demo_tree(tree_gate));
        let concurrent = state.register_node("root/2-concurrent");
        std::thread::spawn(move || demo_loop_concurrent(concurrent));
        eprintln!(
            "[demo] mock driver running — node tree: '{}' (loop: note/ask/between-turns gate), \
             root/1-finishes (seed→notes→ask→final value), root/2-concurrent (two \
             stacked asks), root/3-fails (seed→failure)",
            tidepool_web::DEFAULT_NODE_ID
        );
    }

    std::future::pending::<Result<(), Box<dyn std::error::Error>>>().await
}

/// The loop-node mock driver: narrate, present the sample form, report the
/// submission, then present the between-turns gate — one full lap of the
/// loop node's operator interactions, looping. The gate is an ORDINARY form,
/// same wire as any other `present_form` call — no dedicated continue
/// mechanism.
fn demo_loop_single(gate: Arc<WebGate>) {
    loop {
        gate.post_note("Deciding how to split the question into branches.");
        let submission = gate.present_form(&sample_form());
        eprintln!(
            "[demo:{}] submission: {}",
            tidepool_web::DEFAULT_NODE_ID,
            serde_json::to_string(&submission).unwrap_or_default()
        );
        gate.post_note("Turn folded — 3 nodes, 2 windows. Submit to run another turn.");
        gate.present_form(&between_turns_form());
        eprintln!(
            "[demo:{}] between-turns gate answered — next iteration",
            tidepool_web::DEFAULT_NODE_ID
        );
    }
}

/// Walk the two lifecycle-shaped nodes once: `root/1-finishes` goes seed →
/// notes → ask → finalized value; `root/3-fails` goes seed → failure. Both
/// exercise the exact `OperatorGate` calls the real driver makes at branch
/// birth and fold.
fn demo_tree(gate: Arc<WebGate>) {
    let child = match gate.node_gate("root/1-finishes") {
        Some(c) => c,
        None => return,
    };
    gate.node_seeded(
        "root/1-finishes",
        "NODE root/1 — DISCOVER: map how the demo lifecycle renders, end to end. \
         Finalize a LayerProposal when confident.",
    );
    child.post_note("Reading the substrate before deciding anything.");
    let answer = child.present_form(&steering_form());
    eprintln!(
        "[demo:root/1-finishes] steering answered: {}",
        serde_json::to_string(&answer).unwrap_or_default()
    );
    child.post_note("Steering received — finalizing.");
    gate.retire_node("root/1-finishes");
    gate.node_finalized(
        "root/1-finishes",
        "{\"tag\":\"FinishLayer\",\"summary\":\"The lifecycle renders end to end.\",\
         \"confidence\":\"High\"}",
    );

    if gate.node_gate("root/3-fails").is_some() {
        gate.node_seeded(
            "root/3-fails",
            "NODE root/3 — DISCOVER: what a window that never finalizes looks like.",
        );
        gate.retire_node("root/3-fails");
        gate.node_failed(
            "root/3-fails",
            "round exhaustion — 8 rounds without finalize",
        );
    }
}

/// The two-concurrent-asks mock driver: publish TWO forms on the same node
/// at once (two threads each blocked in their own `present_form`), wait for
/// both to resolve (in whatever order the operator answers them), then
/// present the between-turns gate — demonstrating that neither ask
/// supersedes the other.
fn demo_loop_concurrent(gate: Arc<WebGate>) {
    loop {
        let g1 = gate.clone();
        let h1 = std::thread::spawn(move || g1.present_form(&sample_form_a()));
        let g2 = gate.clone();
        let h2 = std::thread::spawn(move || g2.present_form(&sample_form_b()));

        #[allow(
            clippy::unwrap_used,
            reason = "demo smoke-test thread; a panic here means the interactive demo itself is broken"
        )]
        let a = h1.join().unwrap();
        #[allow(
            clippy::unwrap_used,
            reason = "demo smoke-test thread; a panic here means the interactive demo itself is broken"
        )]
        let b = h2.join().unwrap();
        eprintln!(
            "[demo:root/2-concurrent] both concurrent asks resolved: a={} b={}",
            serde_json::to_string(&a).unwrap_or_default(),
            serde_json::to_string(&b).unwrap_or_default()
        );
        gate.present_form(&between_turns_form());
        eprintln!("[demo:root/2-concurrent] between-turns gate answered — next iteration");
    }
}

/// A representative derived-style product form.
fn sample_form() -> FormShape {
    FormShape::Product {
        type_key: "Demo".into(),
        constructor: "Demo".into(),
        fields: vec![
            FieldShape {
                key: "iterations".into(),
                shape: FormShape::Int,
                doc: None,
            },
            FieldShape {
                key: "note".into(),
                shape: FormShape::String,
                doc: None,
            },
            FieldShape {
                key: "verbose".into(),
                shape: FormShape::Bool,
                doc: None,
            },
        ],
        doc: None,
    }
}

/// A demo stand-in for the real driver's `between_loops_gate_shape` —
/// same shape (one optional `steer` field), not the exact type name, since
/// this binary has no `tidepool-harness` driver instance to borrow it from.
fn between_turns_form() -> FormShape {
    FormShape::Product {
        type_key: "BetweenTurns".into(),
        constructor: "BetweenTurns".into(),
        fields: vec![FieldShape {
            key: "steer".into(),
            shape: FormShape::Optional(Box::new(FormShape::String)),
            doc: Some("Optional message for the next turn — leave blank to just continue.".into()),
        }],
        doc: Some("Turn complete — start the next turn?".into()),
    }
}

/// A single-field product form — every demo form below except [`sample_form`]
/// is exactly this shape with a different type/key/leaf.
fn one_field_form(type_key: &str, key: &str, shape: FormShape) -> FormShape {
    FormShape::Product {
        type_key: type_key.into(),
        constructor: type_key.into(),
        fields: vec![FieldShape {
            key: key.into(),
            shape,
            doc: None,
        }],
        doc: None,
    }
}

/// The steering form `root/1-finishes` raises — the free-text ask shape the
/// real companion uses for model-initiated steering.
fn steering_form() -> FormShape {
    one_field_form("OperatorSteering", "steeringReply", FormShape::String)
}

/// One of `root/2-concurrent`'s two concurrent forms — distinct fields so
/// the two stacked asks are visually distinguishable on the page.
fn sample_form_a() -> FormShape {
    one_field_form("DemoBranchA", "hypothesis", FormShape::String)
}

/// The other of `root/2-concurrent`'s two concurrent forms.
fn sample_form_b() -> FormShape {
    one_field_form("DemoBranchB", "confidence", FormShape::Int)
}
