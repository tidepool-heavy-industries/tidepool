//! `tidepool-selfharness-web` — the minimal operator GUI server.
//!
//! Serves the operator page ([`tidepool_web::shell`]) and exposes a
//! [`WebGate`](tidepool_web::WebGate) the self-iterating harness driver blocks
//! on for its two operator interactions (fill a form / click continue),
//! bound to a registered node.
//!
//! `--demo` runs the server against TWO mock nodes, no harness/model/API
//! calls needed, so both the tab strip and per-node ask-stacking are
//! reviewable on localhost:
//! - the default node ([`tidepool_web::DEFAULT_NODE_ID`]) presents one form,
//!   prints the submitted answer, then parks on the continue gate — the
//!   original single-ask/continue-loop demo.
//! - a second node, `beta`, presents TWO forms CONCURRENTLY (two threads,
//!   each blocked in its own `present_form` call, like two concurrent
//!   cognition windows) — proving neither supersedes the other and each
//!   resolves independently before both park on continue together.
//!
//! Real-driver wiring lives in the sibling `tidepool-selfharness` binary,
//! which boots the same server via [`tidepool_web::spawn_operator_server`]
//! and wires the returned gate into `SelfHarnessDriver`.

use std::sync::Arc;

use clap::Parser;
use tidepool_harness::selfharness::operator::{FieldShape, FormShape, OperatorGate};
use tidepool_web::WebGate;

#[derive(Parser)]
struct Args {
    /// Run against two mock nodes, no harness/model/API calls needed.
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
        std::thread::spawn(move || demo_loop_single(gate));
        let beta = state.register_node("beta");
        std::thread::spawn(move || demo_loop_concurrent(beta));
        eprintln!(
            "[demo] mock driver running — two nodes: '{}' (single ask/continue loop) and \
             'beta' (two concurrent asks, stacked)",
            tidepool_web::DEFAULT_NODE_ID
        );
    }

    std::future::pending::<Result<(), Box<dyn std::error::Error>>>().await
}

/// The single-node mock driver: present the sample form, report the
/// submission, then park on the continue gate — one full lap of both
/// operator interactions, looping.
fn demo_loop_single(gate: Arc<WebGate>) {
    loop {
        let submission = gate.present_form(&sample_form());
        eprintln!(
            "[demo:{}] submission: {}",
            tidepool_web::DEFAULT_NODE_ID,
            serde_json::to_string(&submission).unwrap_or_default()
        );
        gate.await_continue();
        eprintln!(
            "[demo:{}] continue — next iteration",
            tidepool_web::DEFAULT_NODE_ID
        );
    }
}

/// The two-concurrent-asks mock driver: publish TWO forms on the same node
/// at once (two threads each blocked in their own `present_form`), wait for
/// both to resolve (in whatever order the operator answers them), then park
/// on continue — demonstrating that neither ask supersedes the other.
fn demo_loop_concurrent(gate: Arc<WebGate>) {
    loop {
        let g1 = gate.clone();
        let h1 = std::thread::spawn(move || g1.present_form(&sample_form_a()));
        let g2 = gate.clone();
        let h2 = std::thread::spawn(move || g2.present_form(&sample_form_b()));

        let a = h1.join().unwrap();
        let b = h2.join().unwrap();
        eprintln!(
            "[demo:beta] both concurrent asks resolved: a={} b={}",
            serde_json::to_string(&a).unwrap_or_default(),
            serde_json::to_string(&b).unwrap_or_default()
        );
        gate.await_continue();
        eprintln!("[demo:beta] continue — next iteration");
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
            },
            FieldShape {
                key: "note".into(),
                shape: FormShape::String,
            },
            FieldShape {
                key: "verbose".into(),
                shape: FormShape::Bool,
            },
        ],
    }
}

/// One of `beta`'s two concurrent forms — distinct fields so the two stacked
/// asks are visually distinguishable on the page.
fn sample_form_a() -> FormShape {
    FormShape::Product {
        type_key: "DemoBranchA".into(),
        constructor: "DemoBranchA".into(),
        fields: vec![FieldShape {
            key: "hypothesis".into(),
            shape: FormShape::String,
        }],
    }
}

/// The other of `beta`'s two concurrent forms.
fn sample_form_b() -> FormShape {
    FormShape::Product {
        type_key: "DemoBranchB".into(),
        constructor: "DemoBranchB".into(),
        fields: vec![FieldShape {
            key: "confidence".into(),
            shape: FormShape::Int,
        }],
    }
}
