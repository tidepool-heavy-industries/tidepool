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
//! The [`SessionManager`] holds the single active worker (MVP cap = 1).

use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use tidepool_effect::dispatch::DispatchEffect;
use tidepool_mcp::CapturedOutput;

use crate::ask::{PauseGate, ReplAskDispatcher, ResumeMsg, WorkerMessage};
use crate::command::SessionCommand;
use crate::session::{Closed, Session, SessionConfig, SessionHandle, Open};

/// One unit of work handed to the resident worker. Carries the per-turn
/// channels: `session_tx` (worker → server: Suspended/Completed/Error/Closed)
/// and `response_rx` (server → worker: the `ask` answer).
pub struct WorkerJob {
    pub cmd: SessionCommand,
    pub session_tx: tokio::sync::mpsc::UnboundedSender<WorkerMessage>,
    pub response_rx: Receiver<ResumeMsg>,
    pub gate: Arc<PauseGate>,
    pub captured: CapturedOutput,
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
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        if let Some(t) = self.thread.take() {
            // cmd_tx drops with self; the worker exits and we reap the thread.
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
        let _ = job.session_tx.send(WorkerMessage::Error { error: err.clone() });
    }
}

/// The single active session (MVP cap = 1). Distinct from the eval server's
/// continuation registry — a session is one resident worker, not a permit slot.
#[derive(Default)]
pub struct SessionManager {
    current: parking_lot::Mutex<Option<WorkerHandle>>,
}

impl SessionManager {
    pub fn new() -> SessionManager {
        SessionManager {
            current: parking_lot::Mutex::new(None),
        }
    }

    /// Whether a session is currently open.
    pub fn is_open(&self) -> bool {
        self.current.lock().is_some()
    }

    /// Install a freshly-spawned worker as the active session. Errors (and drops
    /// the new worker) if one is already open — MVP cap = 1.
    pub fn install(&self, handle: WorkerHandle) -> Result<(), WorkerHandle> {
        let mut cur = self.current.lock();
        if cur.is_some() {
            return Err(handle);
        }
        *cur = Some(handle);
        Ok(())
    }

    /// Clone the active worker's command sender, if a session is open.
    pub fn current_sender(&self) -> Option<Sender<WorkerJob>> {
        self.current.lock().as_ref().map(WorkerHandle::sender)
    }

    /// Remove the active worker (e.g. for `session_close`), returning it so the
    /// caller can `shutdown` it after the final `Closed` reply.
    pub fn take(&self) -> Option<WorkerHandle> {
        self.current.lock().take()
    }
}
