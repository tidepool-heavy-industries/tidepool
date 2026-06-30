//! The resident session worker: ONE dedicated thread pinned to ONE live
//! [`Session`] (and thus one `JitEffectMachine`), driven command-by-command
//! over a single-consumer channel (serialized by construction — no permit gate,
//! independent of the eval server's `eval_semaphore`).
//!
//! Why a dedicated thread: `JitEffectMachine` owns thread-local GC registries
//! and a Cranelift `JITModule` — it must stay pinned to one thread for its whole
//! life. The async MCP server hands the worker a [`WorkerJob`] per turn and
//! awaits the reply over the job's channels (the same suspend/resume shape as
//! the eval server, reused for an in-turn `ask`).
//!
//! The [`SessionManager`] holds the active workers keyed by session name.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use tidepool_effect::dispatch::DispatchEffect;
use tidepool_mcp::CapturedOutput;

use crate::ask::{PauseGate, ReplAskDispatcher, ResumeMsg, WorkerMessage};
use crate::command::SessionCommand;
use crate::session::{Closed, Open, Session, SessionConfig, SessionHandle};
use crate::state::{shared, SessionState, SharedState};

/// One unit of work handed to the resident worker. Carries the per-turn
/// channels: `session_tx` (worker → server: Suspended/Completed/Error/Closed)
/// and `response_rx` (server → worker: the `ask` answer).
pub struct WorkerJob {
    pub cmd: SessionCommand,
    pub session_tx: tokio::sync::mpsc::UnboundedSender<WorkerMessage>,
    pub response_rx: Receiver<ResumeMsg>,
    pub gate: Arc<PauseGate>,
    pub captured: CapturedOutput,
    /// Optional `input` payload from the `session_run` request — injected into
    /// the generated module so `input :: Aeson.Value` is in scope for the
    /// block's evaluated items. `None` for blocks sent without an `input`.
    pub eval_input: Option<serde_json::Value>,
}

/// A handle to the resident worker thread: the command channel + its join
/// handle. Dropping it (or calling [`Self::shutdown`]) ends the worker.
pub struct WorkerHandle {
    cmd_tx: Sender<WorkerJob>,
    thread: Option<JoinHandle<()>>,
}

impl WorkerHandle {
    /// A clonable sender for enqueuing a job onto the worker's single channel.
    pub fn sender(&self) -> Sender<WorkerJob> {
        self.cmd_tx.clone()
    }

    /// Drop the command channel and join the worker thread. The worker observes
    /// the channel close (or has already broken its loop after a `Close` job)
    /// and exits, dropping the `Session` (and freeing the machine heap).
    pub fn shutdown(mut self) {
        // Dropping cmd_tx makes the worker's `recv()` return Err → loop exits.
        drop(std::mem::replace(&mut self.cmd_tx, dead_sender()));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    /// Drop the handle WITHOUT joining the worker thread. For a worker that may
    /// be stuck in an uninterruptible PURE computation (a runaway with no effect
    /// checkpoint) — `join()` would hang forever. The thread is detached (and
    /// leaked if it never exits), but the caller (and the session slot) is freed.
    /// Used when a `Wedged` session is reclaimed or a `Close` ack times out.
    pub fn detach(mut self) {
        // Drop cmd_tx now; then take (and drop) the JoinHandle WITHOUT joining,
        // so the implicit `Drop` below also does not join (`thread` is None).
        drop(std::mem::replace(&mut self.cmd_tx, dead_sender()));
        let _ = self.thread.take();
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        // Drop the command sender FIRST, then join. Struct fields are dropped
        // only AFTER `Drop::drop` returns, so `cmd_tx` would still be alive
        // during a naive `join()` — leaving the worker parked in `rx.recv()`
        // (which returns `Err` only once EVERY sender drops) and deadlocking
        // teardown forever. Replacing `cmd_tx` with a dead sender drops the real
        // one now, so `recv()` returns `Err`, the loop exits, and `join()`
        // completes. (Mirrors `shutdown()`; a no-op if `shutdown` already ran —
        // `thread` is then `None`.)
        drop(std::mem::replace(&mut self.cmd_tx, dead_sender()));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A closed, never-receiving sender (used to swap out `cmd_tx` in `shutdown`).
fn dead_sender() -> Sender<WorkerJob> {
    let (tx, _rx) = std::sync::mpsc::channel();
    tx
}

/// Spawn the resident worker thread for a session. `base` is the server's
/// effect handler stack (cloned per turn and wrapped by [`ReplAskDispatcher`]).
///
/// The `Session` is constructed INSIDE the thread (`JitEffectMachine` is not
/// `Send`), so only the `Send` `cfg` + `base` cross the spawn boundary.
pub fn spawn_worker<H>(cfg: SessionConfig, base: H, ask_tag: u64) -> WorkerHandle
where
    H: DispatchEffect<CapturedOutput> + Clone + Send + 'static,
{
    let (cmd_tx, rx) = std::sync::mpsc::channel::<WorkerJob>();
    let thread = std::thread::Builder::new()
        .name("tidepool-repl-session".into())
        .stack_size(tidepool_runtime::EVAL_STACK_SIZE)
        .spawn(move || {
            // Install SIGILL/SIGSEGV handlers so a JIT fault yields a clean
            // error instead of killing the process.
            tidepool_codegen::signal_safety::install();
            match Session::open(cfg) {
                Ok(session) => worker_loop(session, base, ask_tag, rx),
                Err(e) => drain_with_error(rx, format!("session open failed: {e}")),
            }
        })
        .expect("spawn tidepool-repl session worker");
    WorkerHandle {
        cmd_tx,
        thread: Some(thread),
    }
}

/// The worker's command loop. Owns the `SessionHandle<Open>` and consumes it on
/// `Close` (the type-state: the resulting `SessionHandle<Closed>` has no `run`).
fn worker_loop<H>(session: Session, base: H, ask_tag: u64, rx: Receiver<WorkerJob>)
where
    H: DispatchEffect<CapturedOutput> + Clone,
{
    let mut open: Option<SessionHandle<Open>> = Some(SessionHandle::new(session));

    while let Ok(job) = rx.recv() {
        if matches!(job.cmd, SessionCommand::Close) {
            if let Some(handle) = open.take() {
                // Consume the open handle → Closed. `_closed` cannot `.run`.
                let _closed: SessionHandle<Closed> = handle.close();
            }
            let _ = job.session_tx.send(WorkerMessage::Closed);
            break;
        }

        let handle = match open.as_mut() {
            Some(h) => h,
            None => {
                let _ = job.session_tx.send(WorkerMessage::Error {
                    error: "session is closed".into(),
                });
                continue;
            }
        };

        // Build the per-turn Ask dispatcher around a fresh clone of the base
        // handler stack; it parks on `response_rx` if the turn hits `ask`.
        let mut dispatcher = ReplAskDispatcher {
            inner: base.clone(),
            ask_tag,
            session_tx: job.session_tx.clone(),
            response_rx: job.response_rx,
            gate: job.gate,
        };

        handle.set_eval_input(job.eval_input);
        let outcome = handle.run(&job.cmd, &mut dispatcher, &job.captured);
        let msg = if outcome.is_error() {
            WorkerMessage::Error {
                error: outcome.render(),
            }
        } else {
            WorkerMessage::Completed {
                result: outcome.render(),
            }
        };
        let _ = job.session_tx.send(msg);
    }
}

/// Fallback loop when the session could not be opened: reply to every job with
/// the open error (and acknowledge `Close`) so the server never hangs.
fn drain_with_error(rx: Receiver<WorkerJob>, err: String) {
    while let Ok(job) = rx.recv() {
        if matches!(job.cmd, SessionCommand::Close) {
            let _ = job.session_tx.send(WorkerMessage::Closed);
            break;
        }
        let _ = job
            .session_tx
            .send(WorkerMessage::Error { error: err.clone() });
    }
}

/// One manager entry: the worker handle plus the session's lifecycle state.
/// State lives here (not smeared across the server's maps) so it is owned in one
/// place and transitioned atomically — see [`crate::state`].
struct SessionEntry {
    handle: WorkerHandle,
    state: SharedState,
}

/// Named-session manager: holds one resident worker per session id. Sessions
/// are keyed by user-supplied name strings (arbitrary, e.g. "default",
/// "agent-1"). Distinct from the eval server's continuation registry — a
/// session is one resident worker, not a permit slot.
#[derive(Default)]
pub struct SessionManager {
    sessions: parking_lot::Mutex<HashMap<String, SessionEntry>>,
}

impl SessionManager {
    pub fn new() -> SessionManager {
        SessionManager {
            sessions: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    /// Install a freshly-spawned worker under `id`, seeded `Idle`. Errors (and
    /// drops the new worker) if a session with that id is already open.
    pub fn install(&self, id: &str, handle: WorkerHandle) -> Result<(), WorkerHandle> {
        let mut sessions = self.sessions.lock();
        if sessions.contains_key(id) {
            return Err(handle);
        }
        sessions.insert(
            id.to_string(),
            SessionEntry {
                handle,
                state: shared(SessionState::Idle),
            },
        );
        Ok(())
    }

    /// Clone the command sender for the named session, if it is open.
    pub fn get_sender(&self, id: &str) -> Option<Sender<WorkerJob>> {
        self.sessions.lock().get(id).map(|e| e.handle.sender())
    }

    /// Clone the shared lifecycle state for the named session, if it is open.
    /// The server locks this (briefly, never across an `.await`) to read/drive
    /// transitions.
    pub fn state(&self, id: &str) -> Option<SharedState> {
        self.sessions.lock().get(id).map(|e| e.state.clone())
    }

    /// Snapshot every open session's `(id, state)` — for the reaper's sweep.
    pub fn snapshot_states(&self) -> Vec<(String, SharedState)> {
        self.sessions
            .lock()
            .iter()
            .map(|(id, e)| (id.clone(), e.state.clone()))
            .collect()
    }

    /// Remove the named session (e.g. for `session_close`), returning the
    /// handle so the caller can `shutdown` it after the final `Closed` reply.
    pub fn remove(&self, id: &str) -> Option<WorkerHandle> {
        self.sessions.lock().remove(id).map(|e| e.handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_handle() -> WorkerHandle {
        let (cmd_tx, _rx) = std::sync::mpsc::channel::<WorkerJob>();
        WorkerHandle {
            cmd_tx,
            thread: None,
        }
    }

    #[test]
    fn session_manager_keying() {
        let mgr = SessionManager::new();

        // Install two distinct sessions.
        assert!(mgr.install("a", make_handle()).is_ok());
        assert!(mgr.install("b", make_handle()).is_ok());

        // Both senders are retrievable.
        assert!(mgr.get_sender("a").is_some());
        assert!(mgr.get_sender("b").is_some());
        assert!(mgr.get_sender("nonexistent").is_none());

        // Duplicate id is rejected.
        assert!(mgr.install("a", make_handle()).is_err());

        // Remove one; the other persists.
        let removed = mgr.remove("a");
        assert!(removed.is_some());
        assert!(mgr.get_sender("a").is_none());
        assert!(mgr.get_sender("b").is_some());

        // Clean up.
        let _ = mgr.remove("b");
    }
}
