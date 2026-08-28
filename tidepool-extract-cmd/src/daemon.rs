//! Client transport for the resident compile daemon. Owns the wire codec,
//! connect step, and `Output` synthesis. Narrow `pub(crate)` surface —
//! `ExtractCmd::run`/`run_with` are the only callers.
//!
//! Wire (mirrors `haskell/src/Tidepool/DaemonServer.hs` exactly — see that
//! module's doc for the authoritative shape): little-endian, length-prefixed
//! frames over a UNIX domain socket, one request/response per connection.
//!
//! ```text
//! frame     ::= u32-LE length, then that many raw bytes (UTF-8 text)
//! request   ::= frame(cwd) u32-LE(argc) frame(argv[0]) .. frame(argv[n-1])
//! response  ::= i32-LE(exit_code) frame(stdout) frame(stderr)
//! ```
//!
//! EOF (a short read) at any point is the daemon-crashed-mid-request signal
//! both sides rely on — this module turns it into [`DaemonError::Crashed`].
//! Every [`DaemonError`] variant means the same thing to the caller: this ONE
//! request was not served by the daemon, fall back to a direct spawn (design
//! §4.2/§5.2) — never a hang, never a silent retry against the same daemon.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{ExitStatus, Output};
use std::time::{Duration, Instant};

/// Bound on the daemon round-trip's I/O (connect itself is local and
/// near-instant over a UNIX domain socket, so this bounds the READ side — a
/// wedged or overloaded daemon must not hang the caller forever). Generous:
/// a COLD resident-session compile can legitimately take several seconds
/// (design §6's own projection), so this is sized well above that, not
/// tuned to the warm case.
const IO_TIMEOUT: Duration = Duration::from_secs(60);

/// The daemon did not serve this request. Every variant carries the SAME
/// meaning to the caller (fall back to Direct) — the distinction exists only
/// for error messages, never for different fallback behavior.
#[derive(Debug)]
pub(crate) enum DaemonError {
    /// The socket does not exist, or nothing is listening — the ordinary,
    /// expected shape of "no daemon running."
    Connect(io::Error),
    /// A read or write failed for a reason other than a clean EOF (a timeout,
    /// a reset connection, ...).
    Io(io::Error),
    /// EOF before a complete frame/response arrived — the daemon crashed (or
    /// was killed) mid-request. The wire's own framing makes this
    /// unambiguous: a clean response is always a complete, self-describing
    /// byte sequence, so any short read here can only mean the peer is gone.
    Crashed,
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DaemonError::Connect(e) => write!(f, "daemon connect failed: {e}"),
            DaemonError::Io(e) => write!(f, "daemon I/O error: {e}"),
            DaemonError::Crashed => write!(f, "daemon crashed mid-request"),
        }
    }
}

impl std::error::Error for DaemonError {}

/// Connect to `socket_path`, send `(cwd, worker argv)` as one request, and
/// return the synthesized [`Output`] the daemon's response describes. The
/// worker argv includes the versioned typed request payload used by a direct
/// spawn, so both transports reach the same Haskell dispatch path.
pub(crate) fn run_over_daemon(
    socket_path: &Path,
    cwd: &Path,
    argv: &[OsString],
) -> Result<(Output, Duration), DaemonError> {
    let start = Instant::now();
    let mut stream = UnixStream::connect(socket_path).map_err(DaemonError::Connect)?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .map_err(DaemonError::Io)?;
    stream
        .set_write_timeout(Some(IO_TIMEOUT))
        .map_err(DaemonError::Io)?;

    let req = encode_request(cwd, argv);
    stream.write_all(&req).map_err(DaemonError::Io)?;

    let (code, stdout, stderr) = decode_response(&mut stream)?;
    let elapsed = start.elapsed();
    Ok((synthesize_output(code, stdout, stderr), elapsed))
}

fn push_frame(buf: &mut Vec<u8>, bytes: &[u8]) {
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(bytes);
}

/// Encode the wire's `request` shape. `pub(crate)` (not private) so the unit
/// tests below can pin its exact byte layout.
pub(crate) fn encode_request(cwd: &Path, argv: &[OsString]) -> Vec<u8> {
    let mut buf = Vec::new();
    push_frame(&mut buf, cwd.as_os_str().as_bytes());
    buf.extend_from_slice(&(argv.len() as u32).to_le_bytes());
    for a in argv {
        push_frame(&mut buf, a.as_bytes());
    }
    buf
}

fn read_exact_or_crash<R: Read>(r: &mut R, n: usize) -> Result<Vec<u8>, DaemonError> {
    let mut buf = vec![0u8; n];
    match r.read_exact(&mut buf) {
        Ok(()) => Ok(buf),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err(DaemonError::Crashed),
        Err(e) => Err(DaemonError::Io(e)),
    }
}

fn read_u32<R: Read>(r: &mut R) -> Result<u32, DaemonError> {
    let b = read_exact_or_crash(r, 4)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn read_frame<R: Read>(r: &mut R) -> Result<Vec<u8>, DaemonError> {
    let n = read_u32(r)? as usize;
    read_exact_or_crash(r, n)
}

/// Decode the wire's `response` shape from any [`Read`] — a real
/// [`UnixStream`] in production, a plain byte slice in the unit tests below
/// (pinning the codec's round-trip and truncated-input behavior without a
/// real socket).
pub(crate) fn decode_response<R: Read>(r: &mut R) -> Result<(i32, Vec<u8>, Vec<u8>), DaemonError> {
    let code_bytes = read_exact_or_crash(r, 4)?;
    let code = i32::from_le_bytes([code_bytes[0], code_bytes[1], code_bytes[2], code_bytes[3]]);
    let stdout = read_frame(r)?;
    let stderr = read_frame(r)?;
    Ok((code, stdout, stderr))
}

/// Encode a plain exit code as the wait(2)-style status
/// [`ExitStatusExt::from_raw`] expects — NOT the bare code. On Linux, a
/// normal exit encodes as `code << 8` (the low byte clear signals
/// `WIFEXITED`, the next byte is `WEXITSTATUS`). Masking through `u8` first
/// mirrors how a real process's exit code is truncated to one byte by the
/// OS itself (e.g. `exit(-1)` becomes exit code 255 to a waiting shell) —
/// this is not a bug workaround, it is what `wait(2)` actually encodes.
fn encode_wait_status(code: i32) -> i32 {
    ((code as u8) as i32) << 8
}

fn synthesize_output(code: i32, stdout: Vec<u8>, stderr: Vec<u8>) -> Output {
    Output {
        status: ExitStatus::from_raw(encode_wait_status(code)),
        stdout,
        stderr,
    }
}

/// A `PathBuf` from raw wire bytes — used only by the fake-daemon test
/// harness (never on the hot path; every real caller builds `Path`/`PathBuf`
/// from Rust-side values, never from decoded wire bytes).
#[cfg(test)]
fn path_from_bytes(bytes: Vec<u8>) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStringExt;
    std::path::PathBuf::from(OsString::from_vec(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn encode_request_matches_the_documented_wire_shape() {
        let cwd = Path::new("/tmp/work");
        let argv = vec![OsString::from("Expr.hs"), OsString::from("--target")];
        let bytes = encode_request(cwd, &argv);

        // frame(cwd)
        let mut expected = Vec::new();
        expected.extend_from_slice(&9u32.to_le_bytes());
        expected.extend_from_slice(b"/tmp/work");
        // argc
        expected.extend_from_slice(&2u32.to_le_bytes());
        // frame(argv[0])
        expected.extend_from_slice(&7u32.to_le_bytes());
        expected.extend_from_slice(b"Expr.hs");
        // frame(argv[1])
        expected.extend_from_slice(&8u32.to_le_bytes());
        expected.extend_from_slice(b"--target");

        assert_eq!(bytes, expected);
    }

    #[test]
    fn encode_request_round_trips_through_a_hand_rolled_decoder() {
        // Mirrors the shape Tidepool.DaemonServer.decodeRequest parses on
        // the Haskell side — decoded here with a small inline parser (this
        // crate never needs to DECODE a request in production; only the
        // Haskell side does) purely to pin that encode_request produces
        // exactly what that decoder expects.
        let cwd = Path::new("/a/b c/d");
        let argv = vec![
            OsString::from(""),
            OsString::from("x y"),
            OsString::from("z"),
        ];
        let bytes = encode_request(cwd, &argv);

        let mut cur = Cursor::new(bytes);
        let cwd_len = {
            let mut b = [0u8; 4];
            cur.read_exact(&mut b).unwrap();
            u32::from_le_bytes(b) as usize
        };
        let mut cwd_bytes = vec![0u8; cwd_len];
        cur.read_exact(&mut cwd_bytes).unwrap();
        assert_eq!(path_from_bytes(cwd_bytes), cwd);

        let mut argc_b = [0u8; 4];
        cur.read_exact(&mut argc_b).unwrap();
        let argc = u32::from_le_bytes(argc_b);
        assert_eq!(argc as usize, argv.len());

        let mut decoded_argv = Vec::new();
        for _ in 0..argc {
            let mut len_b = [0u8; 4];
            cur.read_exact(&mut len_b).unwrap();
            let len = u32::from_le_bytes(len_b) as usize;
            let mut s = vec![0u8; len];
            cur.read_exact(&mut s).unwrap();
            decoded_argv.push(OsString::from_vec(s));
        }
        assert_eq!(decoded_argv, argv);
        // The whole buffer was consumed — no trailing bytes.
        assert_eq!(cur.position() as usize, cur.get_ref().len());
    }

    fn encode_response_bytes(code: i32, stdout: &[u8], stderr: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&code.to_le_bytes());
        push_frame(&mut buf, stdout);
        push_frame(&mut buf, stderr);
        buf
    }

    #[test]
    fn decode_response_round_trips() {
        let bytes = encode_response_bytes(2, b"out text", b"err text");
        let mut cur = Cursor::new(bytes);
        let (code, out, err) = decode_response(&mut cur).unwrap();
        assert_eq!(code, 2);
        assert_eq!(out, b"out text");
        assert_eq!(err, b"err text");
    }

    #[test]
    fn decode_response_negative_exit_code_round_trips() {
        let bytes = encode_response_bytes(-1, b"", b"");
        let mut cur = Cursor::new(bytes);
        let (code, _, _) = decode_response(&mut cur).unwrap();
        assert_eq!(code, -1);
    }

    #[test]
    fn decode_response_truncated_length_prefix_is_crashed() {
        // Only 2 of the 4 exit-code bytes.
        let bytes = vec![0u8, 1u8];
        let mut cur = Cursor::new(bytes);
        match decode_response(&mut cur) {
            Err(DaemonError::Crashed) => {}
            other => panic!("expected Crashed, got {other:?}"),
        }
    }

    #[test]
    fn decode_response_truncated_frame_body_is_crashed() {
        // Exit code is complete; stdout claims 100 bytes but only 3 follow.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0i32.to_le_bytes());
        bytes.extend_from_slice(&100u32.to_le_bytes());
        bytes.extend_from_slice(b"abc");
        let mut cur = Cursor::new(bytes);
        match decode_response(&mut cur) {
            Err(DaemonError::Crashed) => {}
            other => panic!("expected Crashed, got {other:?}"),
        }
    }

    #[test]
    fn decode_response_empty_stream_is_crashed() {
        let mut cur = Cursor::new(Vec::<u8>::new());
        match decode_response(&mut cur) {
            Err(DaemonError::Crashed) => {}
            other => panic!("expected Crashed, got {other:?}"),
        }
    }

    #[test]
    fn exit_status_round_trips_0_1_2() {
        for code in [0i32, 1, 2] {
            let status = ExitStatus::from_raw(encode_wait_status(code));
            assert_eq!(status.code(), Some(code), "code {code} did not round-trip");
            assert_eq!(status.success(), code == 0);
        }
    }

    #[test]
    fn exit_status_negative_code_truncates_like_a_real_process() {
        // A real process's exit(-1) is observed as exit code 255 by a
        // waiting shell/parent — encode_wait_status must match that, not
        // invent a different truncation.
        let status = ExitStatus::from_raw(encode_wait_status(-1));
        assert_eq!(status.code(), Some(255));
    }
}
