//! Captured command execution inside this process, for actors that have no
//! native application of their own.
//!
//! An interactive actor's commands run inside that actor's own sandbox,
//! launched by its agent process. A resident actor — one started from a
//! notebook with `R.start`, or an operator workbench — has no process to
//! launch them in, so they run here, as a child of the host. Running here is
//! not running unconfined: the caller supplies the same three things an
//! interactive launch supplies, and each is enforced by the same mechanism.
//!
//! - **Where it may write**: a [`crate::ProcessMountBoundary`], wrapped around
//!   the command exactly as an agent process is wrapped, so the repository is
//!   read-only apart from the roots the caller granted.
//! - **What it may consume**: the cgroup [`crate::command_resources`] admitted
//!   for this job, joined before `exec`, so memory is capped and accounted like
//!   every other command's.
//! - **When it stops**: the resource owner kills the admitted cgroup, including
//!   descendants. This object retains output and I/O handles, not signal authority.
//!
//! Captured output is bounded by [`RETAINED_STREAM_BYTES`], since it lives in
//! the host's memory rather than the child's cgroup. Positions stay byte
//! offsets into the original stream: a page reports where retention now begins
//! and whether a boundary fell inside a UTF-8 character, rather than silently
//! renumbering or moving it.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};

use parking_lot::Mutex;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    task::JoinHandle,
};

/// How the child's standard input is provided. A resident actor has no
/// terminal, so a PTY is not offered here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostStdin {
    Closed,
    Piped,
}

/// Which captured stream a page is read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostStream {
    Stdout,
    Stderr,
}

/// How the child finished. `Signalled` carries the signal number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostExit {
    Exited(i32),
    Signalled(i32),
}

/// One window onto a captured stream. `start`/`end` are byte offsets into the
/// original stream and may be narrower than the window that was asked for when
/// a boundary fell inside a character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPage {
    pub text: String,
    pub start: u64,
    pub end: u64,
    pub available_end: u64,
    /// The oldest byte still retained. Non-zero once the capture exceeded
    /// [`RETAINED_STREAM_BYTES`] and the front was dropped.
    pub retained_start: u64,
    pub finished: bool,
    pub lossy: bool,
    pub leading_fragment: bool,
    pub trailing_fragment: bool,
}

/// How much of each stream is retained. A host-side capture lives in this
/// process's memory, which no cgroup bounds, so it is bounded here: the tail
/// is what a caller reading a failed check wants, and the page reports the
/// bytes that were dropped ahead of it.
pub const RETAINED_STREAM_BYTES: usize = 4 * 1024 * 1024;

/// What to run, and inside what.
///
/// `cgroup` is the directory [`crate::command_resources`] admitted for this
/// job; the child joins it before `exec` so its memory is accounted and capped
/// like every other command's. `boundary` is the repository mount boundary the
/// command runs behind — the same mechanism an interactive actor's process
/// gets, so "this actor may write in its own worktree and nowhere else in the
/// repository" is enforced by mounts rather than assumed from a working
/// directory. Passing `None` runs the command unconfined and is for callers
/// that have already established confinement some other way.
pub struct HostCommandSpec<'a> {
    pub argv: &'a [String],
    pub directory: &'a Path,
    pub environment: &'a [(String, String)],
    pub stdin: HostStdin,
    pub cgroup: Option<&'a Path>,
    pub boundary: Option<&'a crate::ProcessMountBoundary>,
    /// Absolute executable selected by the launch owner for a bounded command.
    pub bubblewrap: Option<&'a Path>,
}

#[derive(Default)]
struct StreamBuffer {
    bytes: VecDeque<u8>,
    /// Bytes dropped off the front to stay inside [`RETAINED_STREAM_BYTES`].
    /// Positions stay original-stream offsets, so this is also where the
    /// retained window begins.
    dropped: u64,
    finished: bool,
}

impl StreamBuffer {
    fn push(&mut self, chunk: &[u8]) {
        let excess = self
            .bytes
            .len()
            .saturating_add(chunk.len())
            .saturating_sub(RETAINED_STREAM_BYTES);
        let from_buffer = excess.min(self.bytes.len());
        self.bytes.drain(..from_buffer);
        let from_chunk = excess - from_buffer;
        self.bytes.extend(&chunk[from_chunk..]);
        self.dropped = self.dropped.saturating_add(excess as u64);
    }

    /// Render `[start, end)` of the captured bytes. Boundaries that fall
    /// inside a UTF-8 character are reported as fragments and trimmed out of
    /// the text rather than replaced, so a caller stitching pages together by
    /// `end` never sees a replacement character invented by paging itself.
    fn page(&self, start: u64, end: u64) -> HostPage {
        let available = self.dropped + self.bytes.len() as u64;
        let start = start.clamp(self.dropped, available);
        let end = end.clamp(start, available);
        let slice: Vec<u8> = self
            .bytes
            .range((start - self.dropped) as usize..(end - self.dropped) as usize)
            .copied()
            .collect();
        let slice = slice.as_slice();

        let leading = slice
            .iter()
            .take_while(|byte| **byte & 0b1100_0000 == 0b1000_0000)
            .count()
            .min(slice.len());
        let body = &slice[leading..];
        let mut trailing = 0;
        for back in 1..=body.len().min(3) {
            let byte = body[body.len() - back];
            if byte & 0b1100_0000 == 0b1000_0000 {
                continue;
            }
            let width = if byte >= 0b1111_0000 {
                4
            } else if byte >= 0b1110_0000 {
                3
            } else if byte >= 0b1100_0000 {
                2
            } else {
                1
            };
            if width > back {
                trailing = back;
            }
            break;
        }
        let body = &body[..body.len() - trailing];
        let text = String::from_utf8_lossy(body);
        let lossy = matches!(text, std::borrow::Cow::Owned(_));
        HostPage {
            text: text.into_owned(),
            start: start + leading as u64,
            end: end - trailing as u64,
            available_end: available,
            retained_start: self.dropped,
            finished: self.finished,
            lossy,
            leading_fragment: leading > 0,
            trailing_fragment: trailing > 0,
        }
    }
}

/// A running (or finished) host-side command. Cloning is not offered: the
/// caller holds exactly one of these per job, alongside the resource grant.
pub struct HostCommand {
    child: tokio::sync::Mutex<Child>,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    stdout: Arc<Mutex<StreamBuffer>>,
    stderr: Arc<Mutex<StreamBuffer>>,
    readers: tokio::sync::Mutex<Vec<JoinHandle<()>>>,
}

impl HostCommand {
    pub fn spawn(spec: HostCommandSpec<'_>) -> std::io::Result<Self> {
        let Some((program, arguments)) = spec.argv.split_first() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a command needs a program",
            ));
        };
        let invocation = match spec.boundary {
            Some(boundary) => boundary.wrap(
                spec.bubblewrap
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "a bounded command requires resolved bubblewrap",
                        )
                    })?
                    .to_string_lossy(),
                crate::ProcessInvocation {
                    program: program.clone(),
                    args: arguments.to_vec(),
                },
            ),
            None => crate::ProcessInvocation {
                program: program.clone(),
                args: arguments.to_vec(),
            },
        };
        #[allow(clippy::disallowed_methods, reason = "the process launcher")]
        let mut command = Command::new(&invocation.program);
        command
            .args(&invocation.args)
            .current_dir(spec.directory)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(match spec.stdin {
                HostStdin::Closed => Stdio::null(),
                HostStdin::Piped => Stdio::piped(),
            })
            .kill_on_drop(true);
        for (name, value) in spec.environment {
            command.env(name, value);
        }
        if let Some(cgroup) = spec.cgroup {
            let join = std::fs::OpenOptions::new()
                .write(true)
                .open(cgroup.join("cgroup.procs"))?;
            // SAFETY: the descriptor is owned by this stack until spawn
            // returns, and the closure only writes to it — async-signal-safe,
            // no allocation and no locking after fork. Writing "0" moves the
            // calling process, which after fork is this child.
            unsafe {
                command.pre_exec(move || {
                    rustix::io::write(&join, b"0")?;
                    Ok(())
                });
            }
        }
        let mut child = command.spawn()?;
        let stdout = Arc::new(Mutex::new(StreamBuffer::default()));
        let stderr = Arc::new(Mutex::new(StreamBuffer::default()));
        let mut readers = Vec::new();
        if let Some(pipe) = child.stdout.take() {
            readers.push(drain(pipe, Arc::clone(&stdout)));
        }
        if let Some(pipe) = child.stderr.take() {
            readers.push(drain(pipe, Arc::clone(&stderr)));
        }
        let stdin = child.stdin.take();
        Ok(Self {
            child: tokio::sync::Mutex::new(child),
            stdin: tokio::sync::Mutex::new(stdin),
            stdout,
            stderr,
            readers: tokio::sync::Mutex::new(readers),
        })
    }

    /// Wait for exit, then for both captures to reach end of file, so a
    /// finished command's output is whole before anyone reads it.
    pub async fn wait(&self) -> std::io::Result<HostExit> {
        let status = self.child.lock().await.wait().await?;
        for reader in self.readers.lock().await.drain(..) {
            // best-effort: the drain task's own join error carries nothing we
            // act on; the captured buffers are what callers read.
            reader.await.ok();
        }
        use std::os::unix::process::ExitStatusExt;
        Ok(match (status.code(), status.signal()) {
            (Some(code), _) => HostExit::Exited(code),
            (None, Some(signal)) => HostExit::Signalled(signal),
            (None, None) => HostExit::Exited(-1),
        })
    }

    /// Write to the child's standard input. Fails when input was not piped.
    pub async fn write_stdin(&self, text: &str) -> std::io::Result<()> {
        let mut stdin = self.stdin.lock().await;
        let Some(pipe) = stdin.as_mut() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "this command has no open standard input",
            ));
        };
        pipe.write_all(text.as_bytes()).await?;
        pipe.flush().await
    }

    /// Close the child's standard input. Closing twice is not an error.
    pub async fn close_stdin(&self) {
        drop(self.stdin.lock().await.take());
    }

    /// Bytes captured from `stream` so far.
    pub fn available(&self, stream: HostStream) -> u64 {
        let buffer = self.buffer(stream).lock();
        buffer.dropped + buffer.bytes.len() as u64
    }

    pub fn page(&self, stream: HostStream, start: u64, end: u64) -> HostPage {
        self.buffer(stream).lock().page(start, end)
    }

    fn buffer(&self, stream: HostStream) -> &Arc<Mutex<StreamBuffer>> {
        match stream {
            HostStream::Stdout => &self.stdout,
            HostStream::Stderr => &self.stderr,
        }
    }
}

/// The directory a host-side command runs in, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostDirectory {
    /// An explicit directory the command itself named.
    Requested(PathBuf),
    /// The worktree this actor owns through its lease.
    Custody(PathBuf),
    /// No worktree lease and no request: the checkout the run was launched from.
    Source(PathBuf),
}

impl HostDirectory {
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::Requested(path) | Self::Custody(path) | Self::Source(path) => path,
        }
    }
}

fn drain<R>(mut pipe: R, buffer: Arc<Mutex<StreamBuffer>>) -> JoinHandle<()>
where
    R: AsyncReadExt + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut chunk = [0_u8; 8192];
        loop {
            match pipe.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(read) => buffer.lock().push(&chunk[..read]),
            }
        }
        buffer.lock().finished = true;
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(bytes: &[u8], finished: bool) -> StreamBuffer {
        StreamBuffer {
            bytes: bytes.iter().copied().collect(),
            dropped: 0,
            finished,
        }
    }

    #[test]
    fn a_whole_capture_pages_as_one_complete_window() {
        let page = buffer(b"hello", true).page(0, 5);
        assert_eq!(page.text, "hello");
        assert_eq!((page.start, page.end, page.available_end), (0, 5, 5));
        assert!(page.finished);
        assert!(!page.lossy && !page.leading_fragment && !page.trailing_fragment);
    }

    #[test]
    fn a_window_past_the_capture_is_clamped_to_what_exists() {
        let page = buffer(b"hello", false).page(3, 99);
        assert_eq!(page.text, "lo");
        assert_eq!((page.start, page.end, page.available_end), (3, 5, 5));
        assert!(!page.finished);
    }

    /// A boundary inside a character is reported, and the partial bytes are
    /// trimmed rather than decoded into a replacement character that was
    /// never in the stream.
    #[test]
    fn boundaries_inside_a_character_are_reported_as_fragments() {
        let bytes = "aµb".as_bytes(); // 'µ' is two bytes: 0xC2 0xB5
        assert_eq!(bytes.len(), 4);

        let trailing = buffer(bytes, true).page(0, 2);
        assert_eq!(trailing.text, "a");
        assert_eq!((trailing.start, trailing.end), (0, 1));
        assert!(trailing.trailing_fragment && !trailing.leading_fragment);

        let leading = buffer(bytes, true).page(2, 4);
        assert_eq!(leading.text, "b");
        assert_eq!((leading.start, leading.end), (3, 4));
        assert!(leading.leading_fragment && !leading.trailing_fragment);
    }

    #[test]
    fn invalid_utf8_inside_the_window_is_lossy_not_a_fragment() {
        let page = buffer(&[b'a', 0xFF, b'b'], true).page(0, 3);
        assert!(page.lossy);
        assert!(!page.leading_fragment && !page.trailing_fragment);
        assert_eq!((page.start, page.end), (0, 3));
    }

    #[test]
    fn rotation_keeps_absolute_offsets_and_the_latest_tail() {
        let mut capture = StreamBuffer::default();
        capture.push(&vec![b'a'; RETAINED_STREAM_BYTES]);
        capture.push(b"latest");

        assert_eq!(capture.dropped, 6);
        assert_eq!(capture.bytes.len(), RETAINED_STREAM_BYTES);
        let available = RETAINED_STREAM_BYTES as u64 + 6;
        let page = capture.page(available - 6, available);
        assert_eq!(page.text, "latest");
        assert_eq!(page.available_end, RETAINED_STREAM_BYTES as u64 + 6);
        assert_eq!(page.retained_start, 6);

        let oversized = vec![b'z'; RETAINED_STREAM_BYTES + 17];
        capture.push(&oversized);
        assert_eq!(capture.bytes.len(), RETAINED_STREAM_BYTES);
        assert_eq!(capture.bytes.back(), Some(&b'z'));
        assert_eq!(capture.dropped, RETAINED_STREAM_BYTES as u64 + 23);
    }

    #[tokio::test]
    async fn a_command_runs_in_the_directory_it_was_given_and_captures_both_streams() {
        let directory = tempfile::tempdir().unwrap();
        let command = HostCommand::spawn(HostCommandSpec {
            argv: &[
                "sh".into(),
                "-c".into(),
                "pwd; echo trouble >&2; exit 3".into(),
            ],
            directory: directory.path(),
            environment: &[],
            stdin: HostStdin::Closed,
            cgroup: None,
            boundary: None,
            bubblewrap: None,
        })
        .unwrap();
        assert_eq!(command.wait().await.unwrap(), HostExit::Exited(3));

        let out = command.page(HostStream::Stdout, 0, u64::MAX);
        assert!(
            out.text.trim_end().ends_with(
                directory
                    .path()
                    .canonicalize()
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
            ),
            "ran somewhere else: {out:?}"
        );
        assert!(out.finished);
        let err = command.page(HostStream::Stderr, 0, u64::MAX);
        assert_eq!(err.text.trim_end(), "trouble");
    }

    #[tokio::test]
    async fn piped_input_reaches_the_child_and_closing_it_ends_the_read() {
        let directory = tempfile::tempdir().unwrap();
        let command = HostCommand::spawn(HostCommandSpec {
            argv: &["cat".into()],
            directory: directory.path(),
            environment: &[],
            stdin: HostStdin::Piped,
            cgroup: None,
            boundary: None,
            bubblewrap: None,
        })
        .unwrap();
        command.write_stdin("echoed\n").await.unwrap();
        command.close_stdin().await;
        assert_eq!(command.wait().await.unwrap(), HostExit::Exited(0));
        assert_eq!(
            command.page(HostStream::Stdout, 0, u64::MAX).text,
            "echoed\n"
        );
    }

    #[test]
    #[ignore = "measurement harness; run explicitly at integration boundaries"]
    fn retained_output_throughput_measurement() {
        let mut capture = StreamBuffer::default();
        let chunk = vec![b'x'; 64 * 1024];
        let total = 256 * 1024 * 1024_u64;
        let started = std::time::Instant::now();
        for _ in 0..total / chunk.len() as u64 {
            capture.push(&chunk);
        }
        let elapsed = started.elapsed();
        let mib_per_second = total as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64();
        eprintln!(
            "output_capture mib_per_second={mib_per_second:.1} retained_bytes={} dropped_bytes={}",
            capture.bytes.len(),
            capture.dropped,
        );
        assert_eq!(capture.bytes.len(), RETAINED_STREAM_BYTES);
    }
}
