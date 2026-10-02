use std::sync::Arc;

use exomonad_actor::command_jobs::{CommandBackend, CommandControl};
use parking_lot::Mutex;
use tidepool_bridge_effects::{
    CommandCleanup, CommandError, CommandOutcome, CommandOutput, CommandPage, CommandPosition,
    CommandResult, CommandSpec, CommandStatus, CommandStream,
};
use tokio::sync::{watch, Notify};

pub(super) struct TestCommands {
    specs: Mutex<Vec<CommandSpec>>,
    stdout: Mutex<String>,
    stderr: Mutex<String>,
    exit_code: std::sync::atomic::AtomicI64,
    script_exit_code: Mutex<Option<(String, i64)>>,
    /// Nonzero selects `CommandOutOfMemory(mib)` over the exit-code outcome —
    /// this backend bypasses real resource admission entirely, so an OOM
    /// outcome has to be injected directly to exercise how it presents.
    oom_mib: std::sync::atomic::AtomicI64,
    degraded_output: std::sync::atomic::AtomicBool,
    finish: watch::Sender<bool>,
    cancelled: std::sync::atomic::AtomicBool,
    output_unavailable: std::sync::atomic::AtomicBool,
    output_pending: std::sync::atomic::AtomicBool,
    controls: Mutex<Vec<CommandControl>>,
    fail_input: std::sync::atomic::AtomicBool,
    fail_close: std::sync::atomic::AtomicBool,
    output_entered: tokio::sync::Notify,
    hold_output: watch::Sender<bool>,
    output_budgets: Mutex<Vec<usize>>,
    slice_reads: std::sync::atomic::AtomicUsize,
    short_slice_read: std::sync::atomic::AtomicUsize,
    hang_cancel: std::sync::atomic::AtomicBool,
}
impl TestCommands {
    pub(super) fn completed(stdout: &str) -> Arc<Self> {
        Self::completed_streams(stdout, "")
    }

    pub(super) fn completed_streams(stdout: &str, stderr: &str) -> Arc<Self> {
        let backend = Self::new();
        *backend.stdout.lock() = stdout.into();
        *backend.stderr.lock() = stderr.into();
        backend.finish.send_replace(true);
        backend
    }

    /// Like [`Self::completed`], but `execute` only settles after `delay`
    /// elapses: for a test that needs a command whose actual execution
    /// occupies measurable wall time, rather than one that is already
    /// finished before anything observes it.
    pub(super) fn completed_after(delay: std::time::Duration, stdout: &str) -> Arc<Self> {
        let backend = Self::new();
        *backend.stdout.lock() = stdout.into();
        let finish = backend.finish.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            finish.send_replace(true);
        });
        backend
    }

    /// How many commands this backend actually executed. A regression that
    /// asserts committed effects were not replayed reads this.
    pub(super) fn executions(&self) -> usize {
        self.specs.lock().len()
    }

    pub(super) fn control_count(&self) -> usize {
        self.controls.lock().len()
    }

    pub(super) fn output_budgets(&self) -> Vec<usize> {
        self.output_budgets.lock().clone()
    }

    pub(super) fn finish(&self) {
        self.finish.send_replace(true);
    }

    pub(super) fn set_exit_code(&self, exit_code: i64) {
        self.exit_code
            .store(exit_code, std::sync::atomic::Ordering::Release);
    }

    pub(super) fn set_output_unavailable(&self) {
        self.output_unavailable
            .store(true, std::sync::atomic::Ordering::Release);
    }

    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            specs: Mutex::new(Vec::new()),
            stdout: Mutex::new("result".into()),
            stderr: Mutex::new(String::new()),
            exit_code: 0.into(),
            script_exit_code: Mutex::new(None),
            oom_mib: 0.into(),
            degraded_output: false.into(),
            finish: watch::channel(false).0,
            cancelled: false.into(),
            output_unavailable: false.into(),
            output_pending: false.into(),
            controls: Mutex::new(Vec::new()),
            fail_input: false.into(),
            fail_close: false.into(),
            output_entered: tokio::sync::Notify::new(),
            hold_output: watch::channel(false).0,
            output_budgets: Mutex::new(Vec::new()),
            slice_reads: 0.into(),
            short_slice_read: 0.into(),
            hang_cancel: false.into(),
        })
    }

    /// A command finished by the resource owner killing it for memory, with
    /// `mib` as the applied cap the model-facing heading should name.
    pub(super) fn completed_oom(mib: i64) -> Arc<Self> {
        let backend = Self::new();
        backend
            .oom_mib
            .store(mib, std::sync::atomic::Ordering::Release);
        backend.finish.send_replace(true);
        backend
    }

    /// A `control(.., Cancel)` call on this backend never resolves. Exercises
    /// the actor-turn bound wrapping that call, which must not let a stuck
    /// backend freeze the actor.
    pub(super) fn hang_cancel(&self) {
        self.hang_cancel
            .store(true, std::sync::atomic::Ordering::Release);
    }

    pub(super) fn shorten_slice_read(&self, read: usize) {
        self.slice_reads
            .store(0, std::sync::atomic::Ordering::Release);
        self.short_slice_read
            .store(read, std::sync::atomic::Ordering::Release);
    }
}
impl CommandBackend for TestCommands {
    fn cleanup<'a>(&'a self, _id: &'a str) -> futures_util::future::BoxFuture<'a, CommandCleanup> {
        Box::pin(async { CommandCleanup::CommandClean })
    }
    fn execute<'a>(
        &'a self,
        _id: &'a str,
        spec: CommandSpec,
        _phase: watch::Sender<CommandStatus>,
    ) -> futures_util::future::BoxFuture<'a, CommandResult> {
        Box::pin(async move {
            let script_exit_code = self
                .script_exit_code
                .lock()
                .as_ref()
                .filter(|(script, _)| spec.argv.get(4) == Some(script))
                .map(|(_, exit_code)| *exit_code);
            self.specs.lock().push(spec);
            let mut done = self.finish.subscribe();
            while !*done.borrow_and_update() {
                done.changed().await.unwrap();
            }
            let oom_mib = self.oom_mib.load(std::sync::atomic::Ordering::Acquire);
            CommandResult {
                outcome: if oom_mib != 0 {
                    CommandOutcome::CommandOutOfMemory(oom_mib)
                } else if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
                    CommandOutcome::CommandCancelled
                } else {
                    CommandOutcome::CommandExited(script_exit_code.unwrap_or_else(|| {
                        self.exit_code.load(std::sync::atomic::Ordering::Acquire)
                    }))
                },
                cleanup: CommandCleanup::CommandClean,
            }
        })
    }
    fn control<'a>(
        &'a self,
        _id: &'a str,
        operation: CommandControl,
    ) -> futures_util::future::BoxFuture<'a, Result<(), CommandError>> {
        Box::pin(async move {
            self.controls.lock().push(operation.clone());
            if (matches!(operation, CommandControl::Input(_))
                && self.fail_input.load(std::sync::atomic::Ordering::Acquire))
                || (matches!(operation, CommandControl::CloseInput)
                    && self.fail_close.load(std::sync::atomic::Ordering::Acquire))
            {
                return Err(CommandError::CommandUnavailable(
                    "test acknowledgment unavailable".into(),
                ));
            }
            if matches!(operation, CommandControl::Cancel) {
                if self.hang_cancel.load(std::sync::atomic::Ordering::Acquire) {
                    std::future::pending::<()>().await;
                }
                self.cancelled
                    .store(true, std::sync::atomic::Ordering::Release);
                self.finish.send_replace(true);
            }
            Ok(())
        })
    }
    fn read<'a>(
        &'a self,
        _id: &'a str,
        stream: CommandStream,
        position: CommandPosition,
    ) -> futures_util::future::BoxFuture<'a, Result<CommandPage, CommandError>> {
        Box::pin(async move {
            if self
                .output_unavailable
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err(CommandError::CommandUnavailable(
                    "output transport lost".into(),
                ));
            }
            if self
                .output_pending
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err(CommandError::CommandOutputPending);
            }
            let mut page = match stream {
                CommandStream::Stdout => test_page(&self.stdout.lock()),
                CommandStream::Stderr => test_page(&self.stderr.lock()),
            };
            let (offset, limit) = match position {
                CommandPosition::OutputSlice(offset, bytes) => {
                    let read = self
                        .slice_reads
                        .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
                        + 1;
                    let limit = if read
                        == self
                            .short_slice_read
                            .load(std::sync::atomic::Ordering::Acquire)
                    {
                        (bytes as usize).saturating_sub(1)
                    } else {
                        bytes as usize
                    };
                    (offset, limit)
                }
                CommandPosition::OutputOffset(offset) => (offset, 65536),
                CommandPosition::OutputBeginning => (0, 65536),
                CommandPosition::OutputTail => (page.end.saturating_sub(65536), 65536),
            };
            let mut start = offset.min(page.end).max(0) as usize;
            while start < page.text.len() && !page.text.is_char_boundary(start) {
                start += 1;
            }
            let mut end = (start + limit).min(page.text.len());
            while end > start && !page.text.is_char_boundary(end) {
                end -= 1;
            }
            page.text = page.text[start..end].to_owned();
            page.start = start as i64;
            page.end = end as i64;
            // A retained stream that rotated bytes away, decoded with
            // replacement characters, and has not reached end of file: the
            // three signals a capture must never flatten into a plain string.
            if self
                .degraded_output
                .load(std::sync::atomic::Ordering::Acquire)
            {
                page.lossy = true;
                page.lost_bytes = 64;
                page.retained_start = 64;
                page.finished = false;
                page.available_end = page.end + 500;
            }
            Ok(page)
        })
    }
    fn output<'a>(
        &'a self,
        _id: &'a str,
        bytes: usize,
    ) -> futures_util::future::BoxFuture<'a, Result<CommandOutput, CommandError>> {
        Box::pin(async move {
            self.output_entered.notify_one();
            let mut held = self.hold_output.subscribe();
            while *held.borrow_and_update() {
                held.changed().await.unwrap();
            }
            if self
                .output_unavailable
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err(CommandError::CommandUnavailable(
                    "output transport lost".into(),
                ));
            }
            self.output_budgets.lock().push(bytes);
            Ok(CommandOutput {
                stdout: test_initial_page(&self.stdout.lock(), bytes),
                stderr: test_initial_page(&self.stderr.lock(), bytes),
            })
        })
    }
}

fn test_initial_page(text: &str, bytes: usize) -> CommandPage {
    let mut page = test_page(text);
    if page.text.len() > bytes {
        let mut end = bytes;
        while !page.text.is_char_boundary(end) {
            end -= 1;
        }
        page.text.truncate(end);
        page.end = end as i64;
    }
    page
}

fn test_page(text: &str) -> CommandPage {
    CommandPage {
        text: text.into(),
        start: 0,
        end: text.len() as i64,
        available_end: text.len() as i64,
        retained_start: 0,
        lost_bytes: 0,
        finished: true,
        lossy: false,
        leading_fragment: false,
        trailing_fragment: false,
    }
}
