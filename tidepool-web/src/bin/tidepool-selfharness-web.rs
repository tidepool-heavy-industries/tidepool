//! `tidepool-selfharness-web` — the minimal operator GUI server.
//!
//! Serves the single form page ([`tidepool_web::shell`]) and exposes a
//! [`WebGate`](tidepool_web::WebGate) the self-iterating harness driver blocks
//! on for its two operator interactions (fill a form / click continue).
//!
//! `--demo` runs the server against a MOCK driver: a background thread that
//! presents a sample `FormSpec` (one field of each v1 kind), prints the flat
//! submission it receives, then parks on the continue gate — so the page and
//! its aesthetic can be opened and reviewed on localhost with no harness, no
//! model, and no API calls.
//!
//! Real-driver wiring lives in the sibling `tidepool-selfharness` binary,
//! which boots the same server via [`tidepool_web::spawn_operator_server`]
//! and wires the returned gate into `SelfHarnessDriver`.

use std::sync::Arc;

use tidepool_harness::selfharness::operator::{
    EnumOption, Field, FieldKind, FormSpec, OperatorGate,
};
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

/// One field of each v1 kind, so the rendered page exercises the whole surface.
fn sample_form() -> FormSpec {
    FormSpec {
        fields: vec![
            Field {
                key: "direction".into(),
                label: "Direction".into(),
                kind: FieldKind::Enum {
                    options: vec![
                        EnumOption {
                            label: "Continue as planned".into(),
                            tag: "continue".into(),
                        },
                        EnumOption {
                            label: "Narrow the scope".into(),
                            tag: "narrow".into(),
                        },
                        EnumOption {
                            label: "Start over".into(),
                            tag: "restart".into(),
                        },
                    ],
                },
            },
            Field {
                key: "iterations".into(),
                label: "Iterations".into(),
                kind: FieldKind::Int,
            },
            Field {
                key: "note".into(),
                label: "Note to the agent".into(),
                kind: FieldKind::Text,
            },
            Field {
                key: "verbose".into(),
                label: "Verbose logging".into(),
                kind: FieldKind::Bool,
            },
        ],
    }
}

fn arg_str(args: &[String], flag: &str) -> Option<String> {
    let idx = args.iter().position(|a| a == flag)?;
    args.get(idx + 1).cloned()
}
