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
/// a boundary fell inside a character or its start was lost from the retained
/// tail. Malformed bytes are represented as lossy text, including final EOF.
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

    /// Check the retained neighbors of a boundary, rather than interpreting
    /// arbitrary continuation bytes as a valid character's suffix.
    fn character_crossing(&self, boundary: u64) -> Option<(u64, u64)> {
        let available = self.dropped + self.bytes.len() as u64;
        for back in 1..=3 {
            let Some(start) = boundary.checked_sub(back) else {
                break;
            };
            if start < self.dropped {
                break;
            }
            let index = (start - self.dropped) as usize;
            let width = match self.bytes.get(index)? {
                0xC2..=0xDF => 2,
                0xE0..=0xEF => 3,
                0xF0..=0xF4 => 4,
                _ => continue,
            };
            if start + width as u64 <= boundary {
                continue;
            }
            let count = width.min((available - start) as usize);
            let mut bytes = [0; 4];
            for (offset, byte) in bytes[..count].iter_mut().enumerate() {
                *byte = self.bytes[index + offset];
            }
            match std::str::from_utf8(&bytes[..count]) {
                Ok(_) => return Some((start, start + width as u64)),
                Err(error) if !self.finished && error.error_len().is_none() => {
                    return Some((start, start + width as u64));
                }
                Err(_) => {}
            }
        }
        None
    }

    /// Render `[start, end)` of the captured bytes. Boundaries that fall
    /// inside a UTF-8 character are reported as fragments and trimmed out of
    /// the text rather than replaced, so a caller stitching pages together by
    /// `end` never sees a replacement character invented by paging itself.
    /// A valid incomplete prefix at live EOF is deferred until it completes or
    /// the stream finishes; at final EOF it is malformed and decoded lossily.
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

        // The omitted prefix can contain a character's start, but a UTF-8
        // character can have at most three continuation bytes. This uncertainty
        // applies to subwindows within that same retained prefix as well.
        let unknown_prefix_end = self.dropped
            + if self.dropped > 0 {
                self.bytes
                    .iter()
                    .take(3)
                    .take_while(|byte| **byte & 0xC0 == 0x80)
                    .count() as u64
            } else {
                0
            };
        let leading = if start < unknown_prefix_end {
            (unknown_prefix_end.min(end) - start) as usize
        } else {
            self.character_crossing(start)
                .map_or(0, |(_, character_end)| {
                    (character_end.min(end) - start) as usize
                })
        };
        let body_start = start + leading as u64;
        let trailing = self
            .character_crossing(end)
            .map_or(0, |(character_start, _)| {
                (end - character_start.max(body_start)) as usize
            });
        let body = &slice[leading..slice.len() - trailing];
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
    /// finished command's output is whole before anyone reads it. Cancelling
    /// observation retains unfinished captures for a later wait.
    pub async fn wait(&self) -> std::io::Result<HostExit> {
        let status = self.child.lock().await.wait().await?;
        let mut readers = self.readers.lock().await;
        while let Some(reader) = readers.first_mut() {
            // Borrow the owned handle until join completes: a cancelled wait
            // must leave every unfinished capture available to the next wait.
            reader.await.ok();
            drop(readers.remove(0));
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
    fn malformed_bytes_at_stream_origin_remain_visible() {
        for bytes in [
            vec![0x80],
            vec![0x80, 0xBF, b'a'],
            vec![0xFF],
            vec![0xC0, 0x80],
        ] {
            let page = buffer(&bytes, true).page(0, bytes.len() as u64);
            assert_eq!(page.text, String::from_utf8_lossy(&bytes));
            assert_eq!((page.start, page.end), (0, bytes.len() as u64));
            assert!(page.lossy);
            assert!(!page.leading_fragment && !page.trailing_fragment);
        }
    }

    #[test]
    fn malformed_boundary_bytes_are_not_valid_character_fragments() {
        for bytes in [
            vec![0x80],
            vec![0xFF],
            vec![0xC0, 0x80],
            vec![0xE0, 0x80, 0x80],
            vec![0xED, 0xA0, 0x80],
            vec![0xF4, 0x90, 0x80, 0x80],
            vec![0xE2, 0x82],
        ] {
            let framed = [b"a".as_slice(), &bytes, b"z".as_slice()].concat();
            let page = buffer(&framed, true).page(1, 1 + bytes.len() as u64);
            assert_eq!(page.text, String::from_utf8_lossy(&bytes));
            assert_eq!((page.start, page.end), (1, 1 + bytes.len() as u64));
            assert!(page.lossy);
            assert!(!page.leading_fragment && !page.trailing_fragment);
            let eof = buffer(&bytes, true).page(0, u64::MAX);
            assert_eq!(eof.text, String::from_utf8_lossy(&bytes));
            assert!(eof.lossy && !eof.trailing_fragment);
        }
    }

    #[test]
    fn an_unfinished_valid_prefix_becomes_a_character_or_lossy_at_eof() {
        let mut capture = buffer(&[0xE2, 0x82], false);
        let pending = capture.page(0, 2);
        assert_eq!((pending.start, pending.end), (0, 0));
        assert!(pending.text.is_empty() && pending.trailing_fragment && !pending.lossy);
        capture.finished = true;
        let eof = capture.page(0, 2);
        assert_eq!(eof.text, "\u{FFFD}");
        assert_eq!((eof.start, eof.end), (0, 2));
        assert!(eof.lossy && !eof.trailing_fragment);
        capture.finished = false;
        capture.push(&[0xAC]);
        let complete = capture.page(0, 3);
        assert_eq!(complete.text, "€");
        assert!(!complete.lossy && !complete.trailing_fragment);
    }

    #[test]
    fn a_rotated_unknown_prefix_can_hide_at_most_three_continuation_bytes() {
        let mut capture = buffer(&[0x80, 0x80, 0x80, 0x80, b'z'], true);
        capture.dropped = 10;
        let page = capture.page(0, u64::MAX);
        assert_eq!(page.text, "\u{FFFD}z");
        assert_eq!((page.start, page.end, page.retained_start), (13, 15, 10));
        assert!(page.leading_fragment && page.lossy);
        let suffix = capture.page(11, 15);
        assert_eq!(suffix.text, page.text);
        assert_eq!((suffix.start, suffix.end), (13, 15));
        assert!(suffix.leading_fragment && suffix.lossy);
        for offset in [0, 10, 11, 15, 99] {
            let empty = capture.page(offset, offset);
            assert!(empty.text.is_empty());
            assert_eq!(empty.start, empty.end);
            assert!(!empty.leading_fragment && !empty.trailing_fragment && !empty.lossy);
        }
    }

    // Scan the complete input with the standard decoder to locate only valid
    // scalar spans; this oracle does not use the production boundary decoder.
    fn valid_character_spans(bytes: &[u8]) -> Vec<(u64, u64)> {
        let mut spans = Vec::new();
        let mut offset = 0;
        while offset < bytes.len() {
            let (valid, invalid) = match std::str::from_utf8(&bytes[offset..]) {
                Ok(_) => (bytes.len() - offset, None),
                Err(error) => (error.valid_up_to(), error.error_len()),
            };
            let text = std::str::from_utf8(&bytes[offset..offset + valid]).unwrap();
            for (start, character) in text.char_indices() {
                let start = offset + start;
                spans.push((start as u64, (start + character.len_utf8()) as u64));
            }
            offset += valid;
            let Some(invalid) = invalid else { break };
            offset += invalid;
        }
        spans
    }

    #[test]
    fn byte_windows_match_complete_stream_utf8_spans_and_valid_pages_compose() {
        use proptest::prelude::*;
        use proptest::test_runner::{
            contextualize_config, Config, FileFailurePersistence, TestRunner,
        };
        let mut config = Config::default();
        if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
            config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
        }
        config.source_file = Some(file!());
        config.test_name = Some(concat!(
            module_path!(),
            "::byte_windows_match_complete_stream_utf8_spans_and_valid_pages_compose"
        ));
        let mut runner = TestRunner::new(contextualize_config(config));
        let near_valid = proptest::collection::vec(
            prop_oneof![
                Just(vec![0x80]),
                Just(vec![0xC0, 0xAF]),
                Just(vec![0xE0, 0x80, 0x80]),
                Just(vec![0xED, 0xA0, 0x80]),
                Just(vec![0xF4, 0x90, 0x80, 0x80]),
                Just(vec![0xE2, 0x82]),
                Just("aµ€🦀".as_bytes().to_vec()),
            ],
            0..12,
        )
        .prop_map(|chunks| chunks.concat());
        let strategy = (
            prop_oneof![proptest::collection::vec(any::<u8>(), 0..64), near_valid],
            proptest::collection::vec(any::<char>(), 0..32),
            1_usize..9,
        );
        let callbacks = std::cell::Cell::new(0_usize);
        let windows = std::cell::Cell::new(0_usize);
        let malformed = std::cell::Cell::new(0_usize);
        runner
            .run(&strategy, |(bytes, characters, width)| {
                callbacks.set(callbacks.get() + 1);
                malformed.set(malformed.get() + usize::from(std::str::from_utf8(&bytes).is_err()));
                let capture = buffer(&bytes, true);
                let spans = valid_character_spans(&bytes);
                let available = bytes.len() as u64;
                for boundary in 0..=available + 1 {
                    for (start, end) in [
                        (0, boundary),
                        (boundary, available),
                        (boundary, boundary),
                        (boundary, boundary + 1),
                    ] {
                        let start = start.min(available);
                        let end = end.clamp(start, available);
                        let mut expected_start = start;
                        let mut expected_end = end;
                        for &(a, b) in &spans {
                            if a < start && start < b {
                                expected_start = b.min(end);
                            }
                            if a < end && end < b {
                                expected_end = a.max(expected_start);
                            }
                        }
                        let page = capture.page(start, end);
                        prop_assert_eq!((page.start, page.end), (expected_start, expected_end));
                        prop_assert_eq!(page.leading_fragment, expected_start > start);
                        prop_assert_eq!(page.trailing_fragment, expected_end < end);
                        let text = String::from_utf8_lossy(
                            &bytes[expected_start as usize..expected_end as usize],
                        );
                        prop_assert_eq!(page.lossy, matches!(text, std::borrow::Cow::Owned(_)));
                        prop_assert_eq!(page.text, text);
                        windows.set(windows.get() + 1);
                    }
                }
                let text: String = characters.into_iter().collect();
                let capture = buffer(text.as_bytes(), true);
                let mut stitched = String::new();
                let mut offset = 0;
                while offset < text.len() as u64 {
                    let mut end = offset + width as u64;
                    let page = loop {
                        let page = capture.page(offset, end);
                        if page.end > offset {
                            break page;
                        }
                        end += 1;
                        prop_assert!(end <= offset + 4);
                    };
                    prop_assert!(!page.lossy && !page.leading_fragment);
                    stitched.push_str(&page.text);
                    offset = page.end;
                }
                prop_assert_eq!(stitched, text);
                Ok(())
            })
            .unwrap();
        eprintln!(
            "byte_windows callbacks={} windows={} malformed={}",
            callbacks.get(),
            windows.get(),
            malformed.get()
        );
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

    struct GatedCaptures {
        command: HostCommand,
        stdout: Option<tokio::io::DuplexStream>,
        stderr: Option<tokio::io::DuplexStream>,
    }

    impl GatedCaptures {
        async fn new() -> Self {
            let command = HostCommand::spawn(HostCommandSpec {
                argv: &["sh".into(), "-c".into(), "exit 3".into()],
                directory: Path::new("/tmp"),
                environment: &[],
                stdin: HostStdin::Closed,
                cgroup: None,
                boundary: None,
                bubblewrap: None,
            })
            .unwrap();
            assert_eq!(command.wait().await.unwrap(), HostExit::Exited(3));
            *command.stdout.lock() = StreamBuffer::default();
            *command.stderr.lock() = StreamBuffer::default();
            let (stdout, stdout_pipe) = tokio::io::duplex(32);
            let (stderr, stderr_pipe) = tokio::io::duplex(32);
            *command.readers.lock().await = vec![
                drain(stdout_pipe, Arc::clone(&command.stdout)),
                drain(stderr_pipe, Arc::clone(&command.stderr)),
            ];
            Self {
                command,
                stdout: Some(stdout),
                stderr: Some(stderr),
            }
        }

        async fn finish(&mut self, stream: HostStream) {
            let writer = match stream {
                HostStream::Stdout => &mut self.stdout,
                HostStream::Stderr => &mut self.stderr,
            };
            if let Some(mut writer) = writer.take() {
                writer
                    .write_all(match stream {
                        HostStream::Stdout => b"stdout",
                        HostStream::Stderr => b"stderr",
                    })
                    .await
                    .unwrap();
                drop(writer);
            }
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while !self.command.page(stream, 0, u64::MAX).finished {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("gated capture must reach EOF");
        }

        // Poll exactly once, then drop this observation. Readiness depends on
        // the independently held pipe writers, not the command's handle list.
        async fn observe_and_cancel(&self) {
            let mut wait = std::pin::pin!(self.command.wait());
            let observed = std::future::poll_fn(|cx| {
                std::task::Poll::Ready(std::future::Future::poll(wait.as_mut(), cx))
            })
            .await;
            if self.stdout.is_some() || self.stderr.is_some() {
                assert!(observed.is_pending(), "capture is still open: {observed:?}");
            } else {
                assert!(matches!(
                    observed,
                    std::task::Poll::Ready(Ok(HostExit::Exited(3)))
                ));
                assert_eq!(
                    self.command.page(HostStream::Stdout, 0, u64::MAX).text,
                    "stdout"
                );
                assert_eq!(
                    self.command.page(HostStream::Stderr, 0, u64::MAX).text,
                    "stderr"
                );
            }
        }
    }

    #[tokio::test]
    async fn cancelled_wait_retains_pending_output_readers() {
        let mut captures = GatedCaptures::new().await;
        for _ in 0..3 {
            captures.observe_and_cancel().await;
        }
        captures.finish(HostStream::Stdout).await;
        for _ in 0..3 {
            captures.observe_and_cancel().await;
        }
        captures.finish(HostStream::Stderr).await;
        for _ in 0..3 {
            captures.observe_and_cancel().await;
        }
    }

    #[derive(Clone, Debug)]
    enum CaptureOperation {
        ObserveAndCancel,
        Finish(HostStream),
    }

    #[tokio::test]
    async fn cancelled_wait_before_exit_still_collects_complete_output() {
        let command = HostCommand::spawn(HostCommandSpec {
            argv: &["cat".into()],
            directory: Path::new("/tmp"),
            environment: &[],
            stdin: HostStdin::Piped,
            cgroup: None,
            boundary: None,
            bubblewrap: None,
        })
        .unwrap();
        {
            let mut wait = std::pin::pin!(command.wait());
            std::future::poll_fn(|cx| {
                assert!(std::future::Future::poll(wait.as_mut(), cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
        }
        command.write_stdin("after cancellation").await.unwrap();
        command.close_stdin().await;
        let exit = tokio::time::timeout(std::time::Duration::from_secs(5), command.wait())
            .await
            .expect("closed input permits command completion")
            .unwrap();
        assert_eq!(exit, HostExit::Exited(0));
        let output = command.page(HostStream::Stdout, 0, u64::MAX);
        assert_eq!(output.text, "after cancellation");
        assert!(output.finished);
        assert!(command.page(HostStream::Stderr, 0, u64::MAX).finished);
    }

    #[tokio::test]
    async fn concurrent_waiter_cancellation_preserves_capture_owner() {
        let mut captures = GatedCaptures::new().await;
        for stream in [HostStream::Stdout, HostStream::Stderr] {
            let mut first = Box::pin(captures.command.wait());
            let mut second = Box::pin(captures.command.wait());
            std::future::poll_fn(|cx| {
                assert!(std::future::Future::poll(first.as_mut(), cx).is_pending());
                assert!(std::future::Future::poll(second.as_mut(), cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            drop(first);
            std::future::poll_fn(|cx| {
                assert!(std::future::Future::poll(second.as_mut(), cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            drop(second);
            captures.finish(stream).await;
        }
        captures.observe_and_cancel().await;
    }

    #[test]
    fn capture_wait_histories_preserve_eof_before_terminal() {
        use proptest::prelude::*;
        use proptest::test_runner::{
            contextualize_config, Config, FileFailurePersistence, TestRunner,
        };

        let mut config = Config::default();
        if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
            config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
        }
        config.source_file = Some(file!());
        config.test_name = Some(concat!(
            module_path!(),
            "::capture_wait_histories_preserve_eof_before_terminal"
        ));
        let mut runner = TestRunner::new(contextualize_config(config));
        let strategy = (
            proptest::collection::vec(
                prop_oneof![
                    Just(CaptureOperation::ObserveAndCancel),
                    Just(CaptureOperation::Finish(HostStream::Stdout)),
                    Just(CaptureOperation::Finish(HostStream::Stderr)),
                ],
                0..48,
            ),
            any::<bool>(),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let callbacks = std::cell::Cell::new(0_usize);
        let observations = std::cell::Cell::new([0_usize; 3]);
        let result = runner.run(&strategy, |(operations, stderr_first)| {
            callbacks.set(callbacks.get() + 1);
            runtime.block_on(async {
                let mut captures = GatedCaptures::new().await;
                let observe = |captures: &GatedCaptures| {
                    let mut counts = observations.get();
                    counts[if captures.stdout.is_some() {
                        0
                    } else if captures.stderr.is_some() {
                        1
                    } else {
                        2
                    }] += 1;
                    observations.set(counts);
                };
                for operation in operations {
                    match operation {
                        CaptureOperation::ObserveAndCancel => {
                            observe(&captures);
                            captures.observe_and_cancel().await;
                        }
                        CaptureOperation::Finish(stream) => captures.finish(stream).await,
                    }
                }
                // Every generated prefix finishes both captures and repeats
                // observation of their complete terminal output.
                observe(&captures);
                captures.observe_and_cancel().await;
                captures
                    .finish(if stderr_first {
                        HostStream::Stderr
                    } else {
                        HostStream::Stdout
                    })
                    .await;
                observe(&captures);
                captures.observe_and_cancel().await;
                captures
                    .finish(if stderr_first {
                        HostStream::Stdout
                    } else {
                        HostStream::Stderr
                    })
                    .await;
                observe(&captures);
                captures.observe_and_cancel().await;
                observe(&captures);
                captures.observe_and_cancel().await;
            });
            Ok(())
        });
        let [stdout_pending, stderr_pending, completed] = observations.get();
        eprintln!("capture_wait_histories callbacks={} stdout_pending={stdout_pending} stderr_pending={stderr_pending} completed={completed}", callbacks.get());
        result.unwrap();
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

    /// `kill_on_drop(true)` is `HostCommand`'s die-with-owner contract: a
    /// child outlives the `Child` handle only until that handle drops.
    /// Dropping the whole `HostCommand` here — not calling `wait()` first —
    /// proves the child is reaped even when nothing ever collects its exit
    /// status, matching `tidepool-extract-cmd`'s parent-death test pattern
    /// (poll `/proc/<pid>` until the entry disappears).
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_child_is_reaped_when_its_host_command_is_dropped() {
        let directory = tempfile::tempdir().unwrap();
        let pid_file = directory.path().join("child.pid");
        let command = HostCommand::spawn(HostCommandSpec {
            argv: &[
                "sh".into(),
                "-c".into(),
                format!("echo $$ > {}; sleep 30", pid_file.display()),
            ],
            directory: directory.path(),
            environment: &[],
            stdin: HostStdin::Closed,
            cgroup: None,
            boundary: None,
            bubblewrap: None,
        })
        .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let pid: u32 = loop {
            if let Ok(text) = std::fs::read_to_string(&pid_file) {
                if let Ok(pid) = text.trim().parse() {
                    break pid;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "child never wrote its pid"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };

        drop(command);

        let proc_entry = std::path::PathBuf::from(format!("/proc/{pid}"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while proc_entry.exists() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            !proc_entry.exists(),
            "child {pid} survived after its HostCommand was dropped"
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
