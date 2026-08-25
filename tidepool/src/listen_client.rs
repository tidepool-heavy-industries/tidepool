//! `tidepool listen` — the long-lived client for a resident harness's
//! listen channel ([`tidepool_harness::listen`]). Its stdout IS the
//! notification surface an operator's terminal/tool watches: each frame it
//! prints becomes visible the moment it's flushed.
//!
//! Exit semantics, copied from exomonad's `exo listen` reference client
//! (`rust/exo-node/src/listen/client.rs`), the literal template for the
//! server half this connects to:
//! - **bad run id / unreachable socket** → diagnostic on stdout (so a
//!   watching notification shows it) and a non-zero exit. No endless retry:
//!   a wrong id retried forever would silently convince the operator they're
//!   armed while messages queue undelivered.
//! - **EOF from the server** → clean exit: either the harness process died,
//!   or this client was replaced latest-wins by a newer `tidepool listen`.
//!   Reconnecting would thrash the slot against a successor, so the operator
//!   re-arms instead.
//! - **oversized/unparseable frame** → diagnostic + non-zero exit (protocol
//!   violation, not a message).

use std::time::Duration;

use clap::Args;
use tidepool_harness::listen::{Ack, Frame, ListenPaths};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Total budget for the initial connect: arming can race the harness
/// process's own socket bind at cold start, and the harness only exists
/// once it has actually started — 30s comfortably covers boot. Beyond that
/// the harness is genuinely absent and the client should fail loudly rather
/// than retry forever.
const CONNECT_BUDGET: Duration = Duration::from_secs(30);

/// A frame line longer than this is a protocol violation, not a message.
const MAX_FRAME_LINE_BYTES: usize = 64 * 1024;

#[derive(Args, Debug)]
pub struct ListenArgs {
    /// Run/session identifier whose listen channel to connect to — must
    /// match the id the harness process booted with (its run lease id).
    #[arg(long)]
    pub run_id: String,
}

/// The `tidepool listen` main loop: connect (bounded retry), then per frame
/// — write its text to stdout, flush, **then** ack — so an ack strictly
/// implies bytes-on-stdout, which is what lets the server durably advance
/// its cursor.
pub async fn run(args: &ListenArgs) -> Result<(), Box<dyn std::error::Error>> {
    let paths = ListenPaths::for_run(&args.run_id);

    let stream = connect_with_retry(&paths.sock).await.inspect_err(|e| {
        announce(&format!(
            "tidepool listen [{}]: socket unreachable after {CONNECT_BUDGET:?} ({e}) — NOT armed; \
             messages queue until a listener attaches",
            args.run_id
        ));
    })?;

    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = lines.next_line().await? {
        if line.len() > MAX_FRAME_LINE_BYTES {
            announce(&format!(
                "tidepool listen [{}]: oversized frame ({} bytes) — protocol violation, exiting; \
                 re-arm to resume",
                args.run_id,
                line.len()
            ));
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "oversized listen frame",
            )
            .into());
        }
        let frame: Frame = serde_json::from_str(&line).map_err(|e| {
            announce(&format!(
                "tidepool listen [{}]: unparseable frame ({e}) — protocol violation, exiting; \
                 re-arm to resume",
                args.run_id
            ));
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad frame: {e}"))
        })?;

        let mut out = frame.text.into_bytes();
        if out.last() != Some(&b'\n') {
            out.push(b'\n');
        }
        stdout.write_all(&out).await?;
        stdout.flush().await?;

        let mut ack = serde_json::to_vec(&Ack { seq: frame.seq })
            .map_err(|e| std::io::Error::other(format!("encode ack: {e}")))?;
        ack.push(b'\n');
        write_half.write_all(&ack).await?;
        write_half.flush().await?;
    }

    announce(&format!(
        "tidepool listen [{}]: channel closed (harness gone or listener replaced) — re-arm to \
         resume delivery; messages queue meanwhile",
        args.run_id
    ));
    Ok(())
}

/// Print a client-status line to BOTH stdout (so a watching caller's
/// notification carries it) and stderr (so it lands in any captured
/// diagnostics).
fn announce(msg: &str) {
    println!("{msg}");
    eprintln!("{msg}");
}

async fn connect_with_retry(sock: &std::path::Path) -> std::io::Result<UnixStream> {
    let deadline = tokio::time::Instant::now() + CONNECT_BUDGET;
    let mut delay = Duration::from_millis(200);
    loop {
        match UnixStream::connect(sock).await {
            Ok(s) => return Ok(s),
            Err(e) => {
                if tokio::time::Instant::now() + delay >= deadline {
                    return Err(e);
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(1));
            }
        }
    }
}
