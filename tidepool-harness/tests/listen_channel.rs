//! Integration tests for the operator listen channel
//! (`tidepool_harness::listen`) — pure Rust, no GHC, fast tier. Drives the
//! REAL `ListenServer` (real UDS socket, real background accept/drain
//! tasks) against a hand-rolled acking client, mirroring how `tidepool
//! listen` itself behaves.

use std::path::PathBuf;
use std::time::Duration;

use tidepool_harness::listen::{Ack, Frame, ListenPaths, ListenServer};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

fn listen_paths(dir: &std::path::Path, label: &str) -> ListenPaths {
    let root = dir.join(label);
    ListenPaths {
        sock: root.join("listen.sock"),
        frames: root.join("frames.jsonl"),
        cursor: root.join("cursor"),
    }
}

/// Connect to `sock` (bounded retry — the server's accept loop may still be
/// spinning up) and read/ack frames one at a time, delaying `ack_delay`
/// before each ack. Returns every frame received, in arrival order.
async fn connect_and_drain(sock: &PathBuf, ack_delay: Duration, take: usize) -> Vec<Frame> {
    let stream = connect_with_retry(sock).await;
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    let mut seen = Vec::new();
    while seen.len() < take {
        let Some(line) = lines.next_line().await.unwrap() else {
            break;
        };
        let frame: Frame = serde_json::from_str(&line).unwrap();
        if !ack_delay.is_zero() {
            tokio::time::sleep(ack_delay).await;
        }
        let mut ack = serde_json::to_vec(&Ack { seq: frame.seq }).unwrap();
        ack.push(b'\n');
        w.write_all(&ack).await.unwrap();
        w.flush().await.unwrap();
        seen.push(frame);
    }
    seen
}

async fn connect_with_retry(sock: &PathBuf) -> UnixStream {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match UnixStream::connect(sock).await {
            Ok(s) => return s,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => panic!("could not connect to {}: {e}", sock.display()),
        }
    }
}

#[tokio::test]
async fn frames_published_while_disconnected_drain_on_connect_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let paths = listen_paths(dir.path(), "drain");
    let sock = paths.sock.clone();
    let server = ListenServer::start(paths).unwrap();

    // Published with nobody attached — must queue durably.
    server.publish("a").unwrap();
    server.publish("b").unwrap();
    server.publish("c").unwrap();

    let seen = connect_and_drain(&sock, Duration::ZERO, 3).await;
    assert_eq!(
        seen.iter().map(|f| f.text.as_str()).collect::<Vec<_>>(),
        vec!["a", "b", "c"]
    );
    assert_eq!(
        seen.iter().map(|f| f.seq).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );

    server.shutdown();
}

#[tokio::test]
async fn cursor_advances_only_after_the_client_acks() {
    let dir = tempfile::tempdir().unwrap();
    let paths = listen_paths(dir.path(), "ack-gate");
    let sock = paths.sock.clone();
    let frames_path = paths.frames.clone();
    let cursor_path = paths.cursor.clone();
    let server = ListenServer::start(paths).unwrap();

    // Client connects but delays its ack — long enough that we can observe
    // the pre-ack cursor state without waiting out the real ack timeout.
    let client =
        tokio::spawn(async move { connect_and_drain(&sock, Duration::from_millis(300), 1).await });

    // Give the client time to connect before publishing, so delivery is
    // attempted promptly.
    tokio::time::sleep(Duration::from_millis(50)).await;
    server.publish("hello").unwrap();

    // The frame has been written to the client but not yet acked: cursor
    // must still be pinned at 0.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mid_cursor =
        tidepool_harness::listen::FrameQueue::open(frames_path.clone(), cursor_path.clone())
            .unwrap()
            .cursor()
            .unwrap();
    assert_eq!(
        mid_cursor, 0,
        "cursor must not advance before the ack lands"
    );

    let seen = client.await.unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].text, "hello");

    // The client's `.await` only guarantees the ack bytes were flushed onto
    // the socket — the server's own reader/drain tasks still need a beat to
    // process them and durably advance the cursor.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let final_cursor = loop {
        let c =
            tidepool_harness::listen::FrameQueue::open(frames_path.clone(), cursor_path.clone())
                .unwrap()
                .cursor()
                .unwrap();
        if c == 1 || tokio::time::Instant::now() >= deadline {
            break c;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(final_cursor, 1, "cursor advances once the ack lands");

    server.shutdown();
}

#[tokio::test]
async fn latest_connection_wins_and_replaces_the_old_one() {
    let dir = tempfile::tempdir().unwrap();
    let paths = listen_paths(dir.path(), "latest-wins");
    let sock = paths.sock.clone();
    let server = ListenServer::start(paths).unwrap();

    let stream_a = connect_with_retry(&sock).await;
    // Give the accept loop a beat to install client A before B connects, so
    // the ordering (A installed, then replaced by B) is deterministic.
    tokio::time::sleep(Duration::from_millis(50)).await;
    while !server.is_connected() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let sock2 = sock.clone();
    let client_b = tokio::spawn(async move { connect_and_drain(&sock2, Duration::ZERO, 1).await });
    tokio::time::sleep(Duration::from_millis(50)).await;

    server.publish("to the second").unwrap();

    // Client A's connection was replaced: its read side sees EOF, never the
    // published frame.
    let (read_a, _write_a) = stream_a.into_split();
    let mut lines_a = BufReader::new(read_a).lines();
    assert_eq!(lines_a.next_line().await.unwrap(), None);

    let seen_b = client_b.await.unwrap();
    assert_eq!(seen_b.len(), 1);
    assert_eq!(seen_b[0].text, "to the second");

    server.shutdown();
}

#[tokio::test]
async fn cursor_and_sequence_survive_a_server_restart() {
    let dir = tempfile::tempdir().unwrap();
    let paths = listen_paths(dir.path(), "restart");
    let sock = paths.sock.clone();

    // First process: publish two frames, nobody ever connects — both stay
    // durable and unacked.
    {
        let server = ListenServer::start(paths.clone()).unwrap();
        server.publish("first").unwrap();
        server.publish("second").unwrap();
        server.shutdown();
    }

    // "Restart": a fresh ListenServer over the SAME paths must resume the
    // backlog and continue minting sequence numbers past what's on disk.
    let server2 = ListenServer::start(paths).unwrap();
    let seen = connect_and_drain(&sock, Duration::ZERO, 2).await;
    assert_eq!(
        seen.iter().map(|f| f.text.as_str()).collect::<Vec<_>>(),
        vec!["first", "second"]
    );
    assert_eq!(seen.iter().map(|f| f.seq).collect::<Vec<_>>(), vec![1, 2]);

    let f3 = server2.publish("third").unwrap();
    assert_eq!(f3.seq, 3, "sequence minting must continue past the restart");

    server2.shutdown();
}
