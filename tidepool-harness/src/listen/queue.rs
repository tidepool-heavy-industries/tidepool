//! The durable frame queue backing the listen channel: an append-only JSONL
//! log of every published [`Frame`] via [`tidepool_repr::jsonl`] (the one
//! durable-JSONL mechanism — see the root `CLAUDE.md`'s Mechanism Index)
//! plus an atomically-written cursor file via `tidepool_atomic_write` (the
//! one same-directory atomic write-then-rename helper) recording the last
//! ACKED sequence.
//!
//! The server only calls [`FrameQueue::ack`] once a connected client has
//! confirmed delivery of that frame (print-then-ack — see the module doc on
//! [`super`]) — a frame that is durable but unacked across a restart is
//! simply redelivered, in order, once a client next attaches.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use tidepool_repr::jsonl::{self, SyncPolicy, TailPolicy};

use super::Frame;

#[derive(Debug, thiserror::Error)]
pub enum QueueError {
    #[error("listen queue io: {0}")]
    Io(#[from] std::io::Error),
    #[error("listen queue: {0}")]
    Corrupt(String),
}

/// Durable, append-only backing store for [`Frame`]s plus a durable ack
/// cursor. Safe to share across concurrent publishers (`Arc<FrameQueue>`) —
/// mutating operations ([`Self::publish`], [`Self::ack`]) are serialized
/// under an internal lock.
pub struct FrameQueue {
    frames_path: PathBuf,
    cursor_path: PathBuf,
    next_seq: AtomicU64,
    write_lock: Mutex<()>,
}

impl FrameQueue {
    /// Open (or create) the queue at `frames_path`/`cursor_path`, creating
    /// the frames file's parent directory if needed. Replays the frames log
    /// to recover the next mintable sequence number — independent of the
    /// cursor, since a frame can be durable but unacked across a restart.
    pub fn open(frames_path: PathBuf, cursor_path: PathBuf) -> Result<Self, QueueError> {
        if let Some(parent) = frames_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let (frames, torn) = jsonl::read_tail(&frames_path, parse_frame, TailPolicy::Repair)
            .map_err(|e| QueueError::Corrupt(e.to_string()))?;
        if let Some(t) = torn {
            tracing::warn!(
                line = t.line_no,
                reason = %t.reason,
                "listen: torn frame row repaired on open"
            );
        }
        let next = frames.last().map(|f: &Frame| f.seq + 1).unwrap_or(1);
        Ok(Self {
            frames_path,
            cursor_path,
            next_seq: AtomicU64::new(next),
            write_lock: Mutex::new(()),
        })
    }

    /// Durably append a new frame (minting the next sequence number) and
    /// return it.
    pub fn publish(&self, text: &str) -> Result<Frame, QueueError> {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst);
        let frame = Frame {
            seq,
            text: text.to_string(),
        };
        let line = serde_json::to_string(&frame).map_err(|e| QueueError::Corrupt(e.to_string()))?;
        jsonl::append_new_line(&self.frames_path, &line, SyncPolicy::All)?;
        Ok(frame)
    }

    /// Frames with `seq` strictly greater than the persisted cursor, in
    /// order — what a freshly-connected client must be sent to catch up.
    pub fn pending(&self) -> Result<Vec<Frame>, QueueError> {
        let cursor = self.cursor()?;
        let (frames, torn) = jsonl::read_tail(&self.frames_path, parse_frame, TailPolicy::Repair)
            .map_err(|e| QueueError::Corrupt(e.to_string()))?;
        if let Some(t) = torn {
            tracing::warn!(
                line = t.line_no,
                reason = %t.reason,
                "listen: torn frame row repaired on pending() read"
            );
        }
        Ok(frames.into_iter().filter(|f| f.seq > cursor).collect())
    }

    /// The last ACKED sequence — 0 if nothing has ever been acked.
    pub fn cursor(&self) -> Result<u64, QueueError> {
        match std::fs::read_to_string(&self.cursor_path) {
            Ok(s) => s
                .trim()
                .parse::<u64>()
                .map_err(|e| QueueError::Corrupt(format!("bad cursor file: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    /// Durably advance the cursor to `seq` — called ONLY after the client
    /// has acked frame `seq`. Atomic write-then-rename, so a crash mid-write
    /// never corrupts the cursor.
    pub fn ack(&self, seq: u64) -> Result<(), QueueError> {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        tidepool_atomic_write::write_durable(&self.cursor_path, seq.to_string().as_bytes())
            .map_err(|e| QueueError::Corrupt(e.to_string()))?;
        Ok(())
    }
}

fn parse_frame(line: &str) -> Result<Frame, String> {
    serde_json::from_str(line).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(label: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let frames = dir.path().join(label).join("frames.jsonl");
        let cursor = dir.path().join(label).join("cursor");
        (dir, frames, cursor)
    }

    #[test]
    fn publish_mints_increasing_sequence_starting_at_one() {
        let (_dir, frames, cursor) = paths("seq");
        let q = FrameQueue::open(frames, cursor).unwrap();
        let f1 = q.publish("first").unwrap();
        let f2 = q.publish("second").unwrap();
        assert_eq!(f1.seq, 1);
        assert_eq!(f2.seq, 2);
    }

    #[test]
    fn pending_excludes_acked_frames() {
        let (_dir, frames, cursor) = paths("pending");
        let q = FrameQueue::open(frames, cursor).unwrap();
        q.publish("a").unwrap();
        q.publish("b").unwrap();
        assert_eq!(q.pending().unwrap().len(), 2);

        q.ack(1).unwrap();
        let remaining = q.pending().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].seq, 2);
    }

    #[test]
    fn cursor_defaults_to_zero_when_never_acked() {
        let (_dir, frames, cursor) = paths("cursor-default");
        let q = FrameQueue::open(frames, cursor).unwrap();
        assert_eq!(q.cursor().unwrap(), 0);
    }

    #[test]
    fn reopen_resumes_sequence_and_cursor_from_disk() {
        let (_dir, frames, cursor) = paths("reopen");
        {
            let q = FrameQueue::open(frames.clone(), cursor.clone()).unwrap();
            q.publish("a").unwrap();
            q.publish("b").unwrap();
            q.ack(1).unwrap();
        }
        let q2 = FrameQueue::open(frames, cursor).unwrap();
        assert_eq!(q2.cursor().unwrap(), 1);
        let pending = q2.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].seq, 2);

        // Sequence minting continues past what was already on disk.
        let f3 = q2.publish("c").unwrap();
        assert_eq!(f3.seq, 3);
    }
}
