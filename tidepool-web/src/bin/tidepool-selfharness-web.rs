//! `tidepool-selfharness-web` — the minimal operator GUI server.
//!
//! Serves the single form page ([`tidepool_web::shell`]) and exposes a
//! [`WebGate`](tidepool_web::WebGate) the self-iterating harness driver blocks
//! on for its two operator interactions (fill a form / click continue).
//!
//! `--demo` runs the server against a MOCK driver: a background thread that
//! presents a representative `FormShape`, prints the submitted answer, then
//! parks on the continue gate — so the page and
//! its aesthetic can be opened and reviewed on localhost with no harness, no
//! model, and no API calls.
//!
//! Real-driver wiring lives in the sibling `tidepool-selfharness` binary,
//! which boots the same server via [`tidepool_web::spawn_operator_server`]
//! and wires the returned gate into `SelfHarnessDriver`.

use std::sync::Arc;

use tidepool_harness::selfharness::operator::{FieldShape, FormShape, OperatorGate};
use tidepool_web::WebGate;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let demo = args.iter().any(|a| a == "--demo");
    let port: u16 = arg_str(&args, "--port")
        .and_then(|s| s.parse().ok())
        .unwrap_or(4601);

    let gate = tidepool_web::spawn_operator_server(port).await?;

    if demo {
        std::thread::spawn(move || demo_loop(gate));
        eprintln!("[demo] mock driver running — the page presents a sample form");
    }

    std::future::pending::<Result<(), Box<dyn std::error::Error>>>().await
}

/// The mock driver: present the sample form, report the submission, then park
/// on the continue gate — one full lap of both operator interactions, looping.
fn demo_loop(gate: Arc<WebGate>) {
    loop {
        let submission = gate.present_form(&sample_form());
        eprintln!(
            "[demo] submission: {}",
            serde_json::to_string(&submission).unwrap_or_default()
        );
        gate.await_continue();
        eprintln!("[demo] continue — next iteration");
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

fn arg_str(args: &[String], flag: &str) -> Option<String> {
    let idx = args.iter().position(|a| a == flag)?;
    args.get(idx + 1).cloned()
}
