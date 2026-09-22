//! Listen-channel server half: the UDS listener, the latest-wins connected-
//! client slot, the drain loop that walks the durable
//! [`super::queue::FrameQueue`] against it, and [`ListenServer`] — the
//! constructed handle the composition root boots and publishes through.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;

use super::queue::{FrameQueue, QueueError};
use super::{Ack, Frame};

/// How long a delivered frame may wait for its ack before the connection is
/// presumed dead. The client's obligation is print + flush + one ack line —
/// microseconds; this only covers scheduler stalls.
const ACK_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum ListenServerError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Queue(#[from] QueueError),
}

/// A delivery attempt over the listen channel failed — see
/// [`ListenerSlot::try_deliver`].
#[derive(Debug, thiserror::Error)]
pub enum DeliverError {
    /// No `tidepool listen` client is attached.
    #[error("no listener attached")]
    NoListener,
    /// A client is attached but the frame couldn't be written or wasn't
    /// acked in time; the connection is presumed dead and the slot has been
    /// cleared.
    #[error("listener delivery failed: {0}")]
    AckFailed(String),
}

/// The connected-client slot. At most one client delivers at a time
/// (**latest-wins** — see [`serve`]); a delivery is serialized under the
/// inner mutex, so frames never interleave.
pub struct ListenerSlot {
    inner: tokio::sync::Mutex<Option<ListenerHandle>>,
    /// Cheap lock-free read.
    connected: AtomicBool,
    /// Monotonic connection counter — guards a stale reader's clear-on-EOF
    /// against clobbering a replacement connection installed after it.
    generation: AtomicU64,
}

struct ListenerHandle {
    gen: u64,
    writer: BufWriter<OwnedWriteHalf>,
    acks: mpsc::UnboundedReceiver<u64>,
}

impl std::fmt::Debug for ListenerSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListenerSlot")
            .field("connected", &self.is_connected())
            .finish()
    }
}

impl Default for ListenerSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl ListenerSlot {
    pub fn new() -> Self {
        Self {
            inner: tokio::sync::Mutex::new(None),
            connected: AtomicBool::new(false),
            generation: AtomicU64::new(0),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    /// Install a freshly-accepted client (latest-wins: any previous handle
    /// is dropped, so the previous client's read side sees EOF and it exits
    /// cleanly — and its writer being dropped means no frame can ever reach
    /// it after the swap). Returns the connection's generation and the ack
    /// sender its reader task feeds.
    pub async fn install(&self, writer: OwnedWriteHalf) -> (u64, mpsc::UnboundedSender<u64>) {
        let gen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let (ack_tx, acks) = mpsc::unbounded_channel();
        let handle = ListenerHandle {
            gen,
            writer: BufWriter::new(writer),
            acks,
        };
        *self.inner.lock().await = Some(handle);
        self.connected.store(true, Ordering::SeqCst);
        (gen, ack_tx)
    }

    /// Clear the slot iff it still holds connection `gen` — a reader
    /// observing its own connection's EOF must not clobber a newer
    /// connection installed after it.
    pub async fn clear_if_gen(&self, gen: u64) {
        let mut guard = self.inner.lock().await;
        if guard.as_ref().map(|h| h.gen) == Some(gen) {
            *guard = None;
            self.connected.store(false, Ordering::SeqCst);
        }
    }

    /// Deliver `frame` to the attached client and await its ack. `Ok(())`
    /// means the client flushed the payload to stdout — the caller may
    /// durably advance the queue's cursor. Any failure clears the slot (the
    /// connection is presumed dead) and errs.
    pub async fn try_deliver(&self, frame: &Frame) -> Result<(), DeliverError> {
        let mut guard = self.inner.lock().await;
        let handle = guard.as_mut().ok_or(DeliverError::NoListener)?;

        let mut line = serde_json::to_vec(frame)
            .map_err(|e| DeliverError::AckFailed(format!("encode frame: {e}")))?;
        line.push(b'\n');

        let write = async {
            handle.writer.write_all(&line).await?;
            handle.writer.flush().await
        };
        if let Err(e) = write.await {
            Self::clear_locked(&mut guard, &self.connected);
            return Err(DeliverError::AckFailed(format!("write frame: {e}")));
        }

        loop {
            match tokio::time::timeout(ACK_TIMEOUT, handle.acks.recv()).await {
                // A stale (lower) seq can arrive if a previous delivery
                // timed out just as its ack landed; skip it and keep
                // waiting for ours.
                Ok(Some(s)) if s < frame.seq => continue,
                Ok(Some(s)) if s == frame.seq => return Ok(()),
                Ok(Some(s)) => {
                    Self::clear_locked(&mut guard, &self.connected);
                    return Err(DeliverError::AckFailed(format!(
                        "protocol violation: ack seq {s} > delivered seq {}",
                        frame.seq
                    )));
                }
                Ok(None) => {
                    Self::clear_locked(&mut guard, &self.connected);
                    return Err(DeliverError::AckFailed("client reader gone".into()));
                }
                Err(_) => {
                    Self::clear_locked(&mut guard, &self.connected);
                    return Err(DeliverError::AckFailed(format!(
                        "ack timeout after {ACK_TIMEOUT:?}"
                    )));
                }
            }
        }
    }

    fn clear_locked(guard: &mut Option<ListenerHandle>, connected: &AtomicBool) {
        *guard = None;
        connected.store(false, Ordering::SeqCst);
    }
}

/// Feed [`Ack`] lines from the client into the slot's ack channel until EOF
/// or a protocol error. Returning ends the connection's reader task, which
/// clears the slot for this generation.
async fn read_acks(read_half: OwnedReadHalf, ack_tx: &mpsc::UnboundedSender<u64>) {
    let mut lines = BufReader::new(read_half).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => match serde_json::from_str::<Ack>(&line) {
                Ok(ack) => {
                    if ack_tx.send(ack.seq).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "listen: bad ack line; dropping connection");
                    return;
                }
            },
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(error = %e, "listen: ack read error; dropping connection");
                return;
            }
        }
    }
}

/// Drain every currently-pending frame to the attached client, durably
/// acking each one as it lands. Stops — without error — as soon as no
/// client is attached or a delivery fails; the remaining backlog waits for
/// the next connection or [`ListenServer::publish`] notify. Returns the
/// number of frames delivered.
pub async fn drain_pending(queue: &FrameQueue, slot: &ListenerSlot) -> Result<u64, QueueError> {
    let mut delivered = 0u64;
    loop {
        let pending = queue.pending()?;
        let Some(frame) = pending.first() else {
            break;
        };
        if slot.try_deliver(frame).await.is_err() {
            break;
        }
        queue.ack(frame.seq)?;
        delivered += 1;
    }
    Ok(delivered)
}

/// Bind `sock` (mkdir parent, remove-before-bind, `0o600`) and accept
/// clients, installing each into `slot` **latest-wins** and waking `notify`
/// so [`drain_pending`] picks up any backlog immediately. Runs until a
/// bind/accept error, or until the caller aborts the task this is spawned
/// in.
pub async fn serve(
    sock: &Path,
    slot: Arc<ListenerSlot>,
    notify: Arc<Notify>,
) -> Result<(), std::io::Error> {
    if let Some(parent) = sock.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Remove a stale socket so bind() can't fail with EADDRINUSE. NotFound is fine.
    match std::fs::remove_file(sock) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }

    let listener = UnixListener::bind(sock)?;
    std::fs::set_permissions(sock, std::fs::Permissions::from_mode(0o600))?;
    tracing::info!(socket = %sock.display(), "listen: listening");

    loop {
        let (stream, _addr) = listener.accept().await?;
        let (read_half, write_half) = stream.into_split();
        let (gen, ack_tx) = slot.install(write_half).await;
        tracing::info!(gen, "listen: client attached");
        notify.notify_one();

        let slot = slot.clone();
        tokio::spawn(async move {
            read_acks(read_half, &ack_tx).await;
            // Drop the ack sender BEFORE clearing: an in-flight
            // `try_deliver` holds the slot lock while awaiting acks, and
            // the closed channel is what unblocks it promptly.
            drop(ack_tx);
            slot.clear_if_gen(gen).await;
            tracing::info!(gen, "listen: client detached");
        });
    }
}

/// Where a run's listen-channel state lives: the UDS socket, the durable
/// frames log, and the durable ack cursor — resolved via
/// `tidepool_runtime::paths` (the one path-resolution home; re-exports
/// `tidepool-toolchain::paths`).
#[derive(Debug, Clone)]
pub struct ListenPaths {
    pub sock: PathBuf,
    pub frames: PathBuf,
    pub cursor: PathBuf,
}

impl ListenPaths {
    /// Derive every listen-channel path for `run_id` (an opaque host-
    /// supplied identifier, e.g. a self-iterating harness's run lease id)
    /// from the shared path-resolution home.
    pub fn for_run(run_id: &str) -> Self {
        Self {
            sock: tidepool_runtime::paths::listen_sock(run_id),
            frames: tidepool_runtime::paths::listen_frames_path(run_id),
            cursor: tidepool_runtime::paths::listen_cursor_path(run_id),
        }
    }
}

/// The constructed listen-channel server handle: owns the durable queue,
/// the connected-client slot, and the background accept/drain tasks.
/// [`Self::publish`] is the one way anything in this process emits a frame.
/// No dependency on the selfharness driver or any other host internals —
/// the composition root constructs one at boot from [`ListenPaths`] alone.
pub struct ListenServer {
    queue: Arc<FrameQueue>,
    slot: Arc<ListenerSlot>,
    notify: Arc<Notify>,
    accept_task: JoinHandle<()>,
    drain_task: JoinHandle<()>,
}

impl ListenServer {
    /// Open the durable queue and bind the socket at `paths`, then spawn the
    /// accept loop and the drain loop as background tasks. Must run inside a
    /// tokio runtime (spawns onto the ambient one).
    pub fn start(paths: ListenPaths) -> Result<Self, ListenServerError> {
        let queue = Arc::new(FrameQueue::open(paths.frames, paths.cursor)?);
        let slot = Arc::new(ListenerSlot::new());
        let notify = Arc::new(Notify::new());

        let accept_task = {
            let slot = slot.clone();
            let notify = notify.clone();
            let sock = paths.sock;
            tokio::spawn(async move {
                if let Err(e) = serve(&sock, slot, notify).await {
                    tracing::warn!(error = %e, "listen: accept loop ended");
                }
            })
        };

        let drain_task = {
            let queue = queue.clone();
            let slot = slot.clone();
            let notify = notify.clone();
            tokio::spawn(async move {
                loop {
                    notify.notified().await;
                    if let Err(e) = drain_pending(&queue, &slot).await {
                        tracing::warn!(error = %e, "listen: drain loop error");
                    }
                }
            })
        };

        Ok(Self {
            queue,
            slot,
            notify,
            accept_task,
            drain_task,
        })
    }

    /// Durably publish `text` as a new frame and wake the drain loop to
    /// attempt immediate delivery to whatever client is currently attached.
    /// A frame published with no client attached simply stays queued.
    pub fn publish(&self, text: &str) -> Result<Frame, ListenServerError> {
        let frame = self.queue.publish(text)?;
        self.notify.notify_one();
        Ok(frame)
    }

    pub fn is_connected(&self) -> bool {
        self.slot.is_connected()
    }

    /// Stop the background accept and drain tasks. Dropping the returned
    /// handle (which this consumes) also drops the connected-client slot,
    /// which closes any attached client's socket half.
    pub fn shutdown(self) {
        self.accept_task.abort();
        self.drain_task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixStream;

    /// Split a socketpair into (server write half installed later, server
    /// read half, client end).
    fn pair() -> (OwnedWriteHalf, OwnedReadHalf, UnixStream) {
        let (server, client) = UnixStream::pair().unwrap();
        let (server_read, server_write) = server.into_split();
        (server_write, server_read, client)
    }

    /// A well-behaved client task: read each frame off `client`, ack its seq.
    fn spawn_acking_client(client: UnixStream) -> tokio::task::JoinHandle<Vec<Frame>> {
        tokio::spawn(async move {
            let (read, mut write) = client.into_split();
            let mut lines = BufReader::new(read).lines();
            let mut seen = Vec::new();
            while let Ok(Some(line)) = lines.next_line().await {
                let frame: Frame = serde_json::from_str(&line).unwrap();
                let mut ack = serde_json::to_vec(&Ack { seq: frame.seq }).unwrap();
                ack.push(b'\n');
                seen.push(frame);
                if write.write_all(&ack).await.is_err() {
                    break;
                }
                let _ = write.flush().await;
            }
            seen
        })
    }

    #[tokio::test]
    async fn no_listener_is_the_fast_path() {
        let slot = ListenerSlot::new();
        assert!(!slot.is_connected());
        let frame = Frame {
            seq: 1,
            text: "hi".into(),
        };
        assert!(matches!(
            slot.try_deliver(&frame).await,
            Err(DeliverError::NoListener)
        ));
    }

    #[tokio::test]
    async fn acked_delivery_succeeds_and_frames_never_interleave() {
        let slot = ListenerSlot::new();
        let (write, read, client) = pair();
        let (_gen, ack_tx) = slot.install(write).await;
        tokio::spawn(async move { read_acks(read, &ack_tx).await });
        let client_task = spawn_acking_client(client);

        assert!(slot.is_connected());
        slot.try_deliver(&Frame {
            seq: 1,
            text: "first\nwith lines".into(),
        })
        .await
        .unwrap();
        slot.try_deliver(&Frame {
            seq: 2,
            text: "second".into(),
        })
        .await
        .unwrap();

        drop(slot); // drops the writer → client sees EOF and returns
        let seen = client_task.await.unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].seq, 1);
        assert_eq!(seen[0].text, "first\nwith lines");
        assert_eq!(seen[1].seq, 2);
    }

    #[tokio::test]
    async fn silent_client_times_out_and_clears() {
        // Real-time test: waits out ACK_TIMEOUT — tokio's test-util pause
        // isn't enabled here, matching the exomonad reference's own test.
        let slot = ListenerSlot::new();
        let (write, _read, _client) = pair(); // client never acks; keep both ends alive
        let (_gen, _ack_tx) = slot.install(write).await; // channel open, no acks

        match slot
            .try_deliver(&Frame {
                seq: 1,
                text: "hello".into(),
            })
            .await
        {
            Err(DeliverError::AckFailed(detail)) => assert!(detail.contains("timeout")),
            other => panic!("expected ack timeout, got {other:?}"),
        }
        assert!(!slot.is_connected(), "a dead connection clears the slot");
    }

    #[tokio::test]
    async fn latest_wins_replaces_and_first_client_sees_eof() {
        let slot = ListenerSlot::new();

        let (write1, _read1, client1) = pair();
        let (gen1, _ack_tx1) = slot.install(write1).await;

        let (write2, read2, client2) = pair();
        let (gen2, ack_tx2) = slot.install(write2).await;
        assert!(gen2 > gen1);
        tokio::spawn(async move { read_acks(read2, &ack_tx2).await });
        let client2_task = spawn_acking_client(client2);

        // The replaced connection's writer was dropped at the swap → client1 sees EOF.
        let (read1c, _w) = client1.into_split();
        let mut lines1 = BufReader::new(read1c).lines();
        assert_eq!(lines1.next_line().await.unwrap(), None);

        // A stale reader's clear must not clobber the replacement…
        slot.clear_if_gen(gen1).await;
        assert!(slot.is_connected());

        // …and delivery reaches only the new client.
        slot.try_deliver(&Frame {
            seq: 1,
            text: "to the second".into(),
        })
        .await
        .unwrap();
        drop(slot);
        let seen = client2_task.await.unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].text, "to the second");
    }

    #[tokio::test]
    async fn drain_pending_delivers_backlog_in_order_and_advances_cursor_only_after_ack() {
        let dir = tempfile::tempdir().unwrap();
        let queue =
            FrameQueue::open(dir.path().join("frames.jsonl"), dir.path().join("cursor")).unwrap();
        queue.publish("a").unwrap();
        queue.publish("b").unwrap();
        queue.publish("c").unwrap();

        let slot = ListenerSlot::new();
        let (write, read, client) = pair();
        let (_gen, ack_tx) = slot.install(write).await;
        tokio::spawn(async move { read_acks(read, &ack_tx).await });
        let client_task = spawn_acking_client(client);

        let delivered = drain_pending(&queue, &slot).await.unwrap();
        assert_eq!(delivered, 3);
        assert_eq!(queue.cursor().unwrap(), 3);
        assert!(queue.pending().unwrap().is_empty());

        drop(slot);
        let seen = client_task.await.unwrap();
        assert_eq!(
            seen.iter().map(|f| f.text.clone()).collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
        assert_eq!(
            seen.iter().map(|f| f.seq).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[tokio::test]
    async fn drain_pending_stops_and_leaves_cursor_pinned_when_no_client_attached() {
        let dir = tempfile::tempdir().unwrap();
        let queue =
            FrameQueue::open(dir.path().join("frames.jsonl"), dir.path().join("cursor")).unwrap();
        queue.publish("a").unwrap();

        let slot = ListenerSlot::new();
        let delivered = drain_pending(&queue, &slot).await.unwrap();
        assert_eq!(delivered, 0);
        assert_eq!(queue.cursor().unwrap(), 0);
        assert_eq!(queue.pending().unwrap().len(), 1);
    }
}
