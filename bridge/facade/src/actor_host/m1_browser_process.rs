//! Test-only owner for the browser driver and its inherited process group.

use serde_json::Value;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::task::JoinHandle;

const FRAME_LIMIT: usize = 16 * 1024;
const STDERR_LIMIT: usize = 64 * 1024;
const IO_DEADLINE: Duration = Duration::from_secs(5);
const EXIT_DEADLINE: Duration = Duration::from_secs(3);
const REAP_DEADLINE: Duration = Duration::from_secs(5);

struct ProcessGroup {
    pid: Option<rustix::process::Pid>,
}

impl ProcessGroup {
    fn signal(&self, signal: rustix::process::Signal) -> Result<(), String> {
        let Some(pid) = self.pid else {
            return Ok(());
        };
        match rustix::process::kill_process_group(pid, signal) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
            Err(error) => Err(format!("browser process group signal failed: {error}")),
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // The guard remains armed when cleanup is interrupted or unconfirmed.
        let _ = self.signal(rustix::process::Signal::KILL);
    }
}

pub(super) struct BrowserProcess {
    group: ProcessGroup,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    partial_frame: Vec<u8>,
    stderr: Arc<Mutex<Vec<u8>>>,
    stderr_reader: Option<JoinHandle<Result<(), std::io::Error>>>,
}

impl BrowserProcess {
    pub(super) fn spawn(node: &Path, driver: &Path, browsers: &Path) -> Result<Self, String> {
        #[allow(
            clippy::disallowed_methods,
            reason = "test: declared browser driver owned with its inherited process group"
        )]
        let mut command = tokio::process::Command::new(node);
        command
            .arg(driver)
            .env("PLAYWRIGHT_BROWSERS_PATH", browsers)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        command.as_std_mut().process_group(0);
        let mut child = command
            .spawn()
            .map_err(|error| format!("could not start browser driver: {error}"))?;
        let pid = child
            .id()
            .and_then(|pid| i32::try_from(pid).ok())
            .and_then(rustix::process::Pid::from_raw)
            .ok_or("browser driver process identity was not retained")?;
        let group = ProcessGroup { pid: Some(pid) };
        let stdin = child.stdin.take().ok_or("browser stdin was not retained")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("browser stdout was not retained")?;
        let mut pipe = child
            .stderr
            .take()
            .ok_or("browser stderr was not retained")?;
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let retained = Arc::clone(&stderr);
        let stderr_reader = tokio::spawn(async move {
            let mut buffer = [0u8; 4096];
            loop {
                let count = pipe.read(&mut buffer).await?;
                if count == 0 {
                    return Ok(());
                }
                let mut bytes = retained.lock().unwrap_or_else(|error| error.into_inner());
                let keep = count.min(STDERR_LIMIT.saturating_sub(bytes.len()));
                bytes.extend_from_slice(&buffer[..keep]);
            }
        });
        Ok(Self {
            group,
            child: Some(child),
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            partial_frame: Vec::new(),
            stderr,
            stderr_reader: Some(stderr_reader),
        })
    }

    pub(super) async fn send_frame(&mut self, frame: &Value) -> Result<(), String> {
        let mut bytes = serde_json::to_vec(frame).map_err(|_| "could not encode browser frame")?;
        bytes.push(b'\n');
        let stdin = self.stdin.as_mut().ok_or("browser stdin is closed")?;
        tokio::time::timeout(IO_DEADLINE, stdin.write_all(&bytes))
            .await
            .map_err(|_| "browser input exceeded its deadline")?
            .map_err(|error| format!("browser input failed: {error}"))
    }

    /// Retains incomplete bytes across cancellation of this future. The limit
    /// is enforced before copying from the fixed-size buffered reader.
    pub(super) async fn next_frame(&mut self) -> Result<Option<Value>, String> {
        loop {
            let available = self
                .stdout
                .fill_buf()
                .await
                .map_err(|error| format!("browser stdout failed: {error}"))?;
            if available.is_empty() {
                return if self.partial_frame.is_empty() {
                    Ok(None)
                } else {
                    Err("browser exited with an unterminated frame".into())
                };
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let count = newline.unwrap_or(available.len());
            if count > FRAME_LIMIT.saturating_sub(self.partial_frame.len()) {
                return Err("browser frame exceeded protocol limit".into());
            }
            self.partial_frame.extend_from_slice(&available[..count]);
            self.stdout.consume(count + usize::from(newline.is_some()));
            if newline.is_some() {
                let frame = serde_json::from_slice(&self.partial_frame)
                    .map_err(|_| "browser emitted invalid JSON")?;
                self.partial_frame.clear();
                return Ok(Some(frame));
            }
        }
    }

    pub(super) async fn finish(
        mut self,
        outcome: Result<(), String>,
        secret: &str,
    ) -> Result<(), String> {
        // Dropping the pipe sends EOF without a flush that could itself block.
        self.stdin.take();
        let mut errors = Vec::new();
        if let Err(error) = outcome {
            errors.push(error);
        }
        let pid = self.group.pid.expect("browser owner retains its group");
        let graceful = tokio::time::timeout(EXIT_DEADLINE, async {
            loop {
                // Observe without reaping: the unreaped leader reserves the
                // group identity until the final group signal has been sent.
                match rustix::process::waitid(
                    rustix::process::WaitId::Pid(pid),
                    rustix::process::WaitIdOptions::NOHANG
                        | rustix::process::WaitIdOptions::EXITED
                        | rustix::process::WaitIdOptions::NOWAIT,
                ) {
                    Ok(Some(_)) => return Ok(()),
                    Ok(None) => tokio::time::sleep(Duration::from_millis(20)).await,
                    Err(error) => return Err(error),
                }
            }
        })
        .await;
        match graceful {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                errors.push(format!("browser exit observation failed: {error}"));
            }
            Err(_) => {
                errors.push("browser driver did not exit before cleanup deadline".into());
            }
        }
        // A successful leader exit does not establish that Chromium descendants
        // exited. Always stop the retained inherited group before releasing it.
        if let Err(error) = self.group.signal(rustix::process::Signal::TERM) {
            errors.push(error);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        let group_stopped = match self.group.signal(rustix::process::Signal::KILL) {
            Ok(()) => true,
            Err(error) => {
                errors.push(error);
                false
            }
        };
        let child = self
            .child
            .as_mut()
            .expect("browser owner retains its child");
        let status = match tokio::time::timeout(REAP_DEADLINE, child.wait()).await {
            Ok(Ok(reaped)) => Some(reaped),
            Ok(Err(error)) => {
                errors.push(format!("browser reap failed: {error}"));
                None
            }
            Err(_) => {
                errors.push("browser reap exceeded its deadline".into());
                None
            }
        };
        if let Some(status) = status {
            if !status.success() {
                errors.push(format!("browser driver status={status}"));
            }
        }
        if group_stopped && status.is_some() {
            self.group.pid = None;
        }
        if let Some(reader) = self.stderr_reader.as_mut() {
            match tokio::time::timeout(IO_DEADLINE, &mut *reader).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(error))) => errors.push(format!("browser stderr failed: {error}")),
                Ok(Err(error)) => errors.push(format!("browser stderr reader failed: {error}")),
                Err(_) => {
                    errors.push("browser stderr reader exceeded its deadline".into());
                    reader.abort();
                    if tokio::time::timeout(IO_DEADLINE, &mut *reader)
                        .await
                        .is_err()
                    {
                        errors.push("browser stderr reader abort was unconfirmed".into());
                    }
                }
            }
        }
        // Keep the handle in self until all awaits finish so cancellation also
        // reaches Drop's abort instead of detaching the stderr task.
        self.stderr_reader.take();
        if errors.is_empty() {
            return Ok(());
        }
        let bytes = self
            .stderr
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let diagnostics = String::from_utf8_lossy(&bytes);
        let message = format!("{}; stderr={diagnostics}", errors.join("; "));
        Err(if secret.is_empty() {
            message
        } else {
            message.replace(secret, "[test-secret]")
        })
    }
}

impl Drop for BrowserProcess {
    fn drop(&mut self) {
        let _ = self.group.signal(rustix::process::Signal::KILL);
        if let Some(reader) = &self.stderr_reader {
            reader.abort();
        }
        // Child's kill_on_drop remains armed until bounded finish observed exit.
    }
}

#[cfg(test)]
#[path = "m1_browser_process_tests.rs"]
mod tests;
