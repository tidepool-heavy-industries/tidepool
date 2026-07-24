//! One-shot ChatGPT-subscription sign-in for the harness box, runnable
//! before any protocol server exists: prints the authorization URL (plus
//! the ssh port-forward hint for a remote browser), blocks on the
//! loopback callback, persists the token 0600, then verifies the
//! credential chain with a refresh-token exchange — no inference call.
//!
//! ```text
//! cargo run -p tidepool-harness --example login [model]
//! ```

use tidepool_harness::provider::oauth::{
    complete_login, login_status, start_login, verify_login, LoginStatus, OauthConfig,
};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let model = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "gpt-5.2".to_string());
    let cfg = OauthConfig::new(model);

    if login_status(&cfg) == LoginStatus::SignedIn {
        println!(
            "token already present ({}); verifying…",
            cfg.token_path.display()
        );
    } else {
        let flow = start_login(&cfg).await.expect("start_login");
        println!("open this URL in your browser:\n\n  {}\n", flow.authorization_url);
        println!("note: {}\n", flow.port_forward_hint);
        println!("waiting for the browser round-trip on 127.0.0.1:{}…", cfg.callback_port);
        complete_login(&cfg, &flow).await.expect("complete_login");
        println!("token persisted to {}", cfg.token_path.display());
    }

    match verify_login(&cfg).await {
        Ok(()) => println!("auth server accepted a refresh — credential chain is live (no inference spent)"),
        Err(e) => {
            eprintln!("verification FAILED: {e}");
            std::process::exit(1);
        }
    }
}
