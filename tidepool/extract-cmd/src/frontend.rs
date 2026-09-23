use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};

use crate::request::{PRINT_WORKER_REQUEST_FLAG, WORKER_REQUEST_FLAG};
use crate::{daemon, ExtractRequest};

const WORKER_ENV: &str = "TIDEPOOL_EXTRACT_WORKER";
const USAGE: &str = "Usage: tidepool-extract [OPTIONS] <file.hs> ...";

pub fn run(args: Vec<OsString>) -> Result<u8, FrontendError> {
    crate::process::current_process_dies_with_parent().map_err(FrontendError::Io)?;
    if args.is_empty() {
        return Err(FrontendError::Usage(USAGE.to_owned()));
    }
    if args.first().is_some_and(|arg| arg == "--daemon") {
        let config = parse_daemon(&args[1..])?;
        // Named binding: dropping the guard would close the trace appender's
        // flush channel before the daemon serves its first request.
        let _trace_guard = daemon::init_tracing(&config)?;
        let result = prepare_worker().and_then(|worker| daemon::serve(&config, worker));
        match &result {
            Ok(_) => tracing::info!(
                target: "tidepool_extract_cmd::daemon",
                run_id = config.run_id.as_deref().unwrap_or("standalone"),
                "compiler daemon stopped"
            ),
            Err(error) => tracing::error!(
                target: "tidepool_extract_cmd::daemon",
                run_id = config.run_id.as_deref().unwrap_or("standalone"),
                %error,
                "compiler daemon failed"
            ),
        }
        return result;
    }
    if args
        .first()
        .is_some_and(|arg| arg == crate::endpoint::BOUND_ENDPOINT_FLAG)
    {
        return serve_bound_endpoint();
    }
    if args.first().is_some_and(|arg| arg == "--connect") {
        return connect(&args[1..]);
    }
    if args.first().is_some_and(|arg| arg == "--stop-daemon") {
        return stop_daemon(&args[1..]);
    }

    let worker_args = match worker_payload(&args)? {
        Some(payload) => vec![WORKER_REQUEST_FLAG.into(), payload],
        None => {
            let request = ExtractRequest::from_cli(&args)?;
            request.worker_argv()
        }
    };
    let worker = prepare_worker()?;
    let mut command = worker.command();
    command.args(worker_args);
    crate::process::child_dies_with_parent(&mut command);
    let status = command.status().map_err(FrontendError::Io)?;
    Ok(exit_code(status))
}

fn connect(args: &[OsString]) -> Result<u8, FrontendError> {
    let Some((socket, request_args)) = args.split_first() else {
        return Err(FrontendError::Usage(
            "--connect requires a socket path".to_owned(),
        ));
    };
    let request = ExtractRequest::from_cli(request_args)?;
    let cwd = std::env::current_dir().map_err(FrontendError::Io)?;
    let socket = PathBuf::from(socket);
    let binding =
        daemon::preflight(&socket).map_err(|error| FrontendError::Daemon(error.to_string()))?;
    let output = daemon::execute(&socket, &binding.epoch, &cwd, &request.worker_argv())
        .map_err(|error| FrontendError::Daemon(error.to_string()))?;
    io::stdout()
        .write_all(&output.stdout)
        .map_err(FrontendError::Io)?;
    io::stderr()
        .write_all(&output.stderr)
        .map_err(FrontendError::Io)?;
    Ok(exit_code(output.status))
}

/// `--stop-daemon --socket <path>`: request a graceful stop of a running
/// persistent daemon and wait for its ack (or an idempotent no-op if nothing
/// is listening). This is `scripts/lib-extract.sh`'s preferred path before it
/// falls back to signaling the process directly — a signal has no orderly
/// exit here (the daemon does not handle one) and can kill an in-flight
/// compile another caller is waiting on.
fn stop_daemon(args: &[OsString]) -> Result<u8, FrontendError> {
    let mut socket = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let option = arg.to_str().ok_or_else(|| {
            FrontendError::Usage("--stop-daemon options must be UTF-8".to_owned())
        })?;
        match option {
            "--socket" => socket = Some(PathBuf::from(next(&mut args, option)?)),
            _ => {
                return Err(FrontendError::Usage(format!(
                    "unknown --stop-daemon option: {option}"
                )))
            }
        }
    }
    let socket =
        socket.ok_or_else(|| FrontendError::Usage("--stop-daemon requires --socket".to_owned()))?;
    daemon::request_stop(&socket).map_err(|error| FrontendError::Daemon(error.to_string()))?;
    Ok(0)
}

fn worker_payload(args: &[OsString]) -> Result<Option<OsString>, FrontendError> {
    if args.first().is_none_or(|arg| arg != WORKER_REQUEST_FLAG) {
        return Ok(None);
    }
    match args {
        [_, payload] => {
            ExtractRequest::decode_worker_argv(args)?;
            Ok(Some(payload.clone()))
        }
        [_] => Err(FrontendError::Usage(format!(
            "{WORKER_REQUEST_FLAG} requires a payload"
        ))),
        _ => Err(FrontendError::Usage(format!(
            "{WORKER_REQUEST_FLAG} accepts exactly one payload"
        ))),
    }
}

/// Resolve the worker paired with a selected frontend before relocating it.
pub fn worker_for_frontend(frontend: &std::path::Path) -> PathBuf {
    if let Some(path) = std::env::var_os(WORKER_ENV) {
        return path.into();
    }
    frontend.with_file_name("tidepool-extract-bin")
}

fn worker_bin() -> Result<PathBuf, FrontendError> {
    let current = std::env::current_exe().map_err(FrontendError::Io)?;
    Ok(worker_for_frontend(&current))
}

pub(crate) struct PreparedWorker {
    file: File,
    selection: PathBuf,
    bytes: Vec<u8>,
    ghc_libdir: OsString,
}

impl PreparedWorker {
    pub(crate) fn prepare() -> Result<Self, FrontendError> {
        let selection = worker_bin()?;
        let mut file = File::open(&selection).map_err(FrontendError::Io)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(FrontendError::Io)?;
        file.rewind().map_err(FrontendError::Io)?;
        let ghc_libdir = resolve_ghc_libdir()?;
        let prepared = Self {
            file,
            selection,
            bytes,
            ghc_libdir,
        };
        prepared.check_request_protocol()?;
        Ok(prepared)
    }

    /// Ask the resolved worker binary, once, what request protocol it speaks
    /// (`--print-worker-request-flag`, outside the versioned request grammar)
    /// and compare it against this frontend's own. A stale built worker after
    /// a protocol bump otherwise fails every real request with an obscure
    /// argv-parse rejection instead of naming the mismatch; an old worker
    /// that predates the probe flag (unknown flag, or any nonzero exit)
    /// is reported the same way, as speaking an older protocol.
    fn check_request_protocol(&self) -> Result<(), FrontendError> {
        let mut command = self.command();
        command
            .arg(PRINT_WORKER_REQUEST_FLAG)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let worker_flag = match command.output() {
            Ok(output) if output.status.success() => std::str::from_utf8(&output.stdout)
                .ok()
                .map(|text| text.trim().to_owned()),
            _ => None,
        };
        if worker_flag.as_deref() == Some(WORKER_REQUEST_FLAG) {
            return Ok(());
        }
        Err(FrontendError::WorkerVersionMismatch {
            path: self.selection.clone(),
            worker_flag,
        })
    }

    pub(crate) fn producer_identity(&self) -> Result<[u8; 32], FrontendError> {
        let frontend = std::fs::read("/proc/self/exe").map_err(FrontendError::Io)?;
        Ok(crate::endpoint::producer_identity(
            &frontend,
            self.selection.as_os_str(),
            &self.bytes,
            &self.ghc_libdir,
        ))
    }

    pub(crate) fn command(&self) -> Command {
        // The opened descriptor pins the selected inode. `execve` resolves
        // this path before applying close-on-exec, so an atomic replacement of
        // the worker path cannot change which bytes execute.
        let executable = format!("/proc/self/fd/{}", self.file.as_raw_fd());
        let mut command = crate::process::command(executable);
        command.env("TIDEPOOL_GHC_LIBDIR", &self.ghc_libdir);
        command
    }

    pub(crate) fn selection(&self) -> &std::path::Path {
        &self.selection
    }

    /// Build a `PreparedWorker` around an arbitrary executable, for tests
    /// outside this module that need to fake the resident GHC worker (this
    /// struct's fields are private, matching production's strict binary
    /// resolution — `daemon.rs`'s own daemon-loop tests use this instead).
    #[cfg(test)]
    pub(crate) fn for_test(selection: PathBuf) -> Result<Self, FrontendError> {
        let mut file = File::open(&selection).map_err(FrontendError::Io)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(FrontendError::Io)?;
        file.rewind().map_err(FrontendError::Io)?;
        Ok(Self {
            file,
            selection,
            bytes,
            ghc_libdir: "unused".into(),
        })
    }
}

fn prepare_worker() -> Result<PreparedWorker, FrontendError> {
    PreparedWorker::prepare()
}

fn resolve_ghc_libdir() -> Result<OsString, FrontendError> {
    if let Some(value) = std::env::var_os("TIDEPOOL_GHC_LIBDIR") {
        return Ok(value);
    }
    #[allow(
        clippy::disallowed_methods,
        reason = "short synchronous probe: `ghc --print-libdir` exits immediately and is not a long-lived child"
    )]
    let output = Command::new("ghc")
        .arg("--print-libdir")
        .stdin(Stdio::null())
        .output()
        .map_err(FrontendError::Io)?;
    if !output.status.success() {
        return Err(FrontendError::Io(io::Error::other(format!(
            "ghc --print-libdir exited with {}",
            output.status
        ))));
    }
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|error| FrontendError::Io(io::Error::new(io::ErrorKind::InvalidData, error)))?;
    let libdir = text.trim();
    if libdir.is_empty() {
        return Err(FrontendError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "ghc --print-libdir returned an empty path",
        )));
    }
    Ok(OsString::from(libdir))
}

fn serve_bound_endpoint() -> Result<u8, FrontendError> {
    let prepared = prepare_worker()?;
    let identity = prepared.producer_identity()?;
    crate::endpoint::write_identity(io::stdout().lock(), &identity).map_err(FrontendError::Io)?;
    let mut stdin = io::stdin().lock();
    let mut prefix = [0u8; 8];
    stdin.read_exact(&mut prefix).map_err(FrontendError::Io)?;
    if &prefix == daemon::TRANSACTION {
        let mut worker = daemon::Worker::spawn(&prepared)?;
        worker.begin_transaction()?;
        io::stdout().write_all(&[1]).map_err(FrontendError::Io)?;
        io::stdout().flush().map_err(FrontendError::Io)?;
        loop {
            let mut command = [0u8; 1];
            if stdin.read_exact(&mut command).is_err() {
                break;
            }
            match command[0] {
                daemon::TRANSACTION_END => break,
                daemon::TRANSACTION_REQUEST => {
                    let (cwd, argv) = daemon::read_request(&mut stdin)?;
                    let worker_argv = daemon::normalize_worker_argv(argv)?;
                    let (code, out, err) = worker.request(&cwd, &worker_argv)?;
                    daemon::write_response(io::stdout().lock(), code, &out, &err)?;
                    io::stdout().flush().map_err(FrontendError::Io)?;
                }
                other => {
                    return Err(FrontendError::Daemon(format!(
                        "unknown compiler transaction command {other}"
                    )))
                }
            }
        }
        let result = worker.end_transaction();
        worker.shutdown();
        return result.map(|()| 0);
    }
    let mut request = io::Cursor::new(prefix).chain(stdin);
    let (cwd, argv) = daemon::read_request(&mut request)?;
    let worker_argv = daemon::normalize_worker_argv(argv)?;
    let mut worker = daemon::Worker::spawn(&prepared)?;
    let result = worker
        .begin_transaction()
        .and_then(|()| worker.request(&cwd, &worker_argv))
        .and_then(|response| worker.end_transaction().map(|()| response));
    if let Ok((code, stdout, stderr)) = &result {
        daemon::write_response(io::stdout().lock(), *code, stdout, stderr)?;
    }
    worker.shutdown();
    result.map(|_| 0)
}

pub(crate) struct DaemonConfig {
    pub socket: PathBuf,
    pub rotate_after: Option<u64>,
    pub rss_ceiling_mb: Option<u64>,
    pub request_deadline_secs: Option<u64>,
    pub watch_stamp: Option<PathBuf>,
    pub persistent: bool,
    pub run_id: Option<String>,
    pub log_path: Option<PathBuf>,
    /// Concurrent GHC worker slots (`daemon::DEFAULT_WORKER_COUNT` when
    /// unset). Only `--persistent` runs more than one; see that constant's
    /// doc comment.
    pub workers: Option<usize>,
}

fn parse_daemon(args: &[OsString]) -> Result<DaemonConfig, FrontendError> {
    let mut socket = None;
    let mut rotate_after = None;
    let mut rss_ceiling_mb = None;
    let mut request_deadline_secs = None;
    let mut watch_stamp = None;
    let mut persistent = false;
    let mut run_id = None;
    let mut log_path = None;
    let mut workers = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let option = arg
            .to_str()
            .ok_or_else(|| FrontendError::Usage("daemon options must be UTF-8".to_owned()))?;
        match option {
            "--socket" => socket = Some(PathBuf::from(next(&mut args, option)?)),
            "--rotate-after" => rotate_after = Some(number(&mut args, option)?),
            "--rss-ceiling-mb" => rss_ceiling_mb = Some(number(&mut args, option)?),
            "--request-deadline-secs" => request_deadline_secs = Some(number(&mut args, option)?),
            "--watch-stamp" => watch_stamp = Some(PathBuf::from(next(&mut args, option)?)),
            "--persistent" => persistent = true,
            "--run-id" => {
                run_id = Some(
                    next(&mut args, option)?
                        .to_str()
                        .ok_or_else(|| FrontendError::Usage("--run-id must be UTF-8".to_owned()))?
                        .to_owned(),
                )
            }
            "--log-path" => log_path = Some(PathBuf::from(next(&mut args, option)?)),
            "--workers" => {
                let count = number(&mut args, option)?;
                let count = usize::try_from(count)
                    .map_err(|_| FrontendError::Usage("--workers is too large".to_owned()))?;
                if count == 0 {
                    return Err(FrontendError::Usage(
                        "--workers must be at least 1".to_owned(),
                    ));
                }
                workers = Some(count);
            }
            _ => {
                return Err(FrontendError::Usage(format!(
                    "unknown daemon option: {option}"
                )))
            }
        }
    }
    Ok(DaemonConfig {
        socket: socket.ok_or_else(|| FrontendError::Usage("--socket is required".to_owned()))?,
        rotate_after,
        rss_ceiling_mb,
        request_deadline_secs,
        watch_stamp,
        persistent,
        run_id,
        log_path,
        workers,
    })
}

fn next<'a>(
    args: &mut impl Iterator<Item = &'a OsString>,
    option: &str,
) -> Result<&'a OsStr, FrontendError> {
    args.next()
        .map(OsString::as_os_str)
        .ok_or_else(|| FrontendError::Usage(format!("{option} requires a value")))
}

fn number<'a>(
    args: &mut impl Iterator<Item = &'a OsString>,
    option: &str,
) -> Result<u64, FrontendError> {
    next(args, option)?
        .to_str()
        .and_then(|raw| raw.parse().ok())
        .ok_or_else(|| FrontendError::Usage(format!("{option} requires an unsigned integer")))
}

fn exit_code(status: ExitStatus) -> u8 {
    status.code().unwrap_or(1).clamp(0, 255) as u8
}

#[derive(Debug)]
pub enum FrontendError {
    Usage(String),
    WorkerProtocol(crate::request::ProtocolError),
    /// The resolved worker binary's `--print-worker-request-flag` output
    /// (`None` if it doesn't understand the probe or exited nonzero) doesn't
    /// match [`WORKER_REQUEST_FLAG`]. Checked once per resolved worker binary
    /// in [`PreparedWorker::prepare`], before any real request is sent.
    WorkerVersionMismatch {
        path: PathBuf,
        worker_flag: Option<String>,
    },
    Io(io::Error),
    Daemon(String),
}

impl From<crate::request::CliError> for FrontendError {
    fn from(error: crate::request::CliError) -> Self {
        Self::Usage(error.to_string())
    }
}

impl From<crate::request::ProtocolError> for FrontendError {
    fn from(error: crate::request::ProtocolError) -> Self {
        Self::WorkerProtocol(error)
    }
}

impl std::fmt::Display for FrontendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(message) | Self::Daemon(message) => f.write_str(message),
            Self::WorkerProtocol(error) => error.fmt(f),
            Self::WorkerVersionMismatch { path, worker_flag } => {
                let worker_version = worker_flag.as_deref().unwrap_or("an older protocol");
                write!(
                    f,
                    "compiler worker {} speaks {}, this frontend speaks {}: rebuild the worker (see bridge/haskell/CLAUDE.md)",
                    path.display(),
                    worker_version,
                    WORKER_REQUEST_FLAG,
                )
            }
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for FrontendError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV: Mutex<()> = Mutex::new(());

    #[test]
    fn daemon_rejects_unaccepted_requests_when_it_exits_on_an_error() {
        use std::time::{Duration, Instant};

        let dir = std::env::temp_dir().join(format!("tp-exit-reject-{}", std::process::id()));
        // best-effort: test cleanup of a temp path from a prior run.
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("daemon.sock");
        let stamp = dir.join("stamp");
        std::fs::write(&stamp, b"boot").unwrap();
        let selection = std::env::current_exe().unwrap();
        let prepared = PreparedWorker {
            file: File::open(&selection).unwrap(),
            bytes: std::fs::read(&selection).unwrap(),
            selection,
            ghc_libdir: "unused".into(),
        };
        let config = DaemonConfig {
            socket: socket.clone(),
            rotate_after: None,
            rss_ceiling_mb: None,
            request_deadline_secs: None,
            watch_stamp: Some(stamp.clone()),
            persistent: false,
            run_id: None,
            log_path: None,
            workers: None,
        };
        let server = std::thread::spawn(move || crate::daemon::serve(&config, prepared));
        let deadline = Instant::now() + Duration::from_secs(10);
        let binding = loop {
            if let Ok(binding) = crate::daemon::preflight(&socket) {
                break binding;
            }
            assert!(Instant::now() < deadline, "daemon did not become ready");
            #[allow(
                clippy::disallowed_methods,
                reason = "test: sync polling loop waiting for the daemon to become ready"
            )]
            std::thread::sleep(Duration::from_millis(10));
        };
        // An unreadable stamp makes the acceptance fence itself fail.
        std::fs::remove_file(&stamp).unwrap();
        std::fs::create_dir(&stamp).unwrap();
        let clients: Vec<_> = (0..3)
            .map(|_| {
                let socket = socket.clone();
                let dir = dir.clone();
                let epoch = binding.epoch;
                std::thread::spawn(move || {
                    crate::daemon::execute(&socket, &epoch, &dir, &["Expr.hs".into()])
                })
            })
            .collect();
        for client in clients {
            let error = client.join().unwrap().unwrap_err();
            assert!(
                error.is_not_accepted(),
                "a request the daemon never accepted must prove it: {error}"
            );
        }
        assert!(server.join().unwrap().is_err());
        assert!(!socket.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn persistent_daemon_recovers_worker_crashes_without_replaying_requests() {
        use std::time::{Duration, Instant};

        let dir = std::env::temp_dir().join(format!("tp-crash-worker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("daemon.sock");
        let stamp = dir.join("stamp");
        std::fs::write(&stamp, b"boot").unwrap();
        // The test executable exits on the worker-only argument. It cannot
        // emit a valid response; every accepted request exercises child failure.
        let selection = std::env::current_exe().unwrap();
        let prepared = PreparedWorker {
            file: File::open(&selection).unwrap(),
            bytes: std::fs::read(&selection).unwrap(),
            selection,
            ghc_libdir: "unused".into(),
        };
        let config = DaemonConfig {
            socket: socket.clone(),
            rotate_after: None,
            rss_ceiling_mb: None,
            request_deadline_secs: None,
            watch_stamp: Some(stamp.clone()),
            persistent: true,
            run_id: None,
            log_path: None,
            workers: None,
        };
        let server = std::thread::spawn(move || crate::daemon::serve(&config, prepared));
        let deadline = Instant::now() + Duration::from_secs(10);
        let binding = loop {
            if let Ok(binding) = crate::daemon::preflight(&socket) {
                break binding;
            }
            assert!(Instant::now() < deadline, "daemon did not become ready");
            #[allow(
                clippy::disallowed_methods,
                reason = "test: sync polling loop waiting for the daemon to become ready"
            )]
            std::thread::sleep(Duration::from_millis(10));
        };
        for _ in 0..2 {
            let error = crate::daemon::execute(&socket, &binding.epoch, &dir, &["Expr.hs".into()])
                .unwrap_err();
            assert!(error.was_accepted());
            assert!(!error.is_not_accepted());
            let next = crate::daemon::preflight(&socket).unwrap();
            assert_eq!(next.epoch, binding.epoch);
        }
        std::fs::write(&stamp, b"changed").unwrap();
        assert!(crate::daemon::preflight(&socket).is_err());
        assert_eq!(server.join().unwrap().unwrap(), 0);
        assert!(!socket.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn relocation_resolves_worker_from_original_frontend() {
        let _env = ENV.lock().unwrap();
        let previous = std::env::var_os(WORKER_ENV);
        std::env::remove_var(WORKER_ENV);
        assert_eq!(
            worker_for_frontend(std::path::Path::new("/selected/bin/tidepool-extract")),
            PathBuf::from("/selected/bin/tidepool-extract-bin")
        );
        std::env::set_var(WORKER_ENV, "/explicit/worker");
        assert_eq!(
            worker_for_frontend(std::path::Path::new("/selected/bin/tidepool-extract")),
            PathBuf::from("/explicit/worker")
        );
        match previous {
            Some(value) => std::env::set_var(WORKER_ENV, value),
            None => std::env::remove_var(WORKER_ENV),
        }
    }

    #[test]
    fn no_arguments_are_the_exact_usage_error() {
        assert!(matches!(run(Vec::new()), Err(FrontendError::Usage(message)) if message == USAGE));
    }

    #[test]
    fn typed_worker_payload_must_decode() {
        assert!(worker_payload(&[
            WORKER_REQUEST_FLAG.into(),
            "54505245513031320100000009".into(),
        ])
        .is_err());
    }

    #[test]
    fn persistent_daemon_is_an_explicit_lifecycle_mode() {
        let config = parse_daemon(&[
            "--socket".into(),
            "/tmp/compiler.sock".into(),
            "--persistent".into(),
            "--run-id".into(),
            "run-1".into(),
            "--log-path".into(),
            "/tmp/run-1-compiler.log".into(),
        ])
        .unwrap();
        assert!(config.persistent);
        assert_eq!(config.run_id.as_deref(), Some("run-1"));
        assert_eq!(
            config.log_path.as_deref(),
            Some(std::path::Path::new("/tmp/run-1-compiler.log"))
        );

        let rotating = parse_daemon(&[
            "--socket".into(),
            "/tmp/compiler.sock".into(),
            "--rotate-after".into(),
            "1".into(),
        ])
        .unwrap();
        assert!(!rotating.persistent);
    }

    /// Compile a tiny ELF fake worker (rustc, matching the style of
    /// `daemon.rs`'s hung-worker fixture) that answers this frontend's own
    /// `--print-worker-request-flag` probe correctly and otherwise prints
    /// `label` when `TIDEPOOL_PREPARED_WORKER_CHILD` is set. Compiled rather
    /// than a shebang script: `PreparedWorker::command` execs the selected
    /// binary via `/proc/self/fd/N`, which only resolves for a real ELF.
    fn compile_fake_worker(source_path: &std::path::Path, bin_path: &std::path::Path, label: &str) {
        std::fs::write(
            source_path,
            format!(
                r#"
fn main() {{
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["{probe}"] {{
        print!("{flag}");
        return;
    }}
    if std::env::var_os("TIDEPOOL_PREPARED_WORKER_CHILD").is_some() {{
        print!("{label}");
    }}
}}
"#,
                probe = PRINT_WORKER_REQUEST_FLAG,
                flag = WORKER_REQUEST_FLAG,
                label = label,
            ),
        )
        .unwrap();
        #[allow(
            clippy::disallowed_methods,
            reason = "test fixture: compiles a throwaway fake worker binary, not a production launch site"
        )]
        let rustc = std::process::Command::new("rustc")
            .arg(source_path)
            .arg("-o")
            .arg(bin_path)
            .status()
            .unwrap();
        assert!(rustc.success(), "fake worker failed to compile");
    }

    #[test]
    fn prepared_worker_identity_and_execution_survive_path_replacement() {
        let _env = ENV.lock().unwrap();
        let dir =
            std::env::temp_dir().join(format!("tidepool-prepared-worker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let worker = dir.join("worker");
        let replacement = dir.join("replacement");
        compile_fake_worker(&dir.join("worker.rs"), &worker, "old-worker");
        std::env::set_var(WORKER_ENV, &worker);
        std::env::set_var("TIDEPOOL_GHC_LIBDIR", "/ghc/lib-a");

        let prepared = PreparedWorker::prepare().unwrap();
        let old_identity = prepared.producer_identity().unwrap();

        compile_fake_worker(&dir.join("replacement.rs"), &replacement, "new-worker");
        std::fs::rename(&replacement, &worker).unwrap();
        assert_eq!(
            prepared.producer_identity().unwrap(),
            old_identity,
            "a boot-bound worker must retain immutable identity after path replacement"
        );

        let output = prepared
            .command()
            .env("TIDEPOOL_PREPARED_WORKER_CHILD", "1")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("old-worker"));

        let new_identity = PreparedWorker::prepare()
            .unwrap()
            .producer_identity()
            .unwrap();
        assert_ne!(old_identity, new_identity);

        std::env::set_var("TIDEPOOL_GHC_LIBDIR", "/ghc/lib-b");
        let ghc_identity = PreparedWorker::prepare()
            .unwrap()
            .producer_identity()
            .unwrap();
        assert_ne!(new_identity, ghc_identity);

        std::env::remove_var(WORKER_ENV);
        std::env::remove_var("TIDEPOOL_GHC_LIBDIR");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_worker_speaking_a_different_protocol_is_reported_by_name_and_version() {
        let dir = std::env::temp_dir().join(format!(
            "tidepool-worker-protocol-mismatch-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("worker.rs");
        let bin = dir.join("worker");
        std::fs::write(
            &source,
            r#"
fn main() {
    print!("--worker-request-v11");
}
"#,
        )
        .unwrap();
        #[allow(
            clippy::disallowed_methods,
            reason = "test fixture: compiles a throwaway fake worker binary, not a production launch site"
        )]
        let rustc = std::process::Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&bin)
            .status()
            .unwrap();
        assert!(rustc.success(), "fake worker failed to compile");

        let prepared = PreparedWorker::for_test(bin.clone()).unwrap();
        let error = prepared.check_request_protocol().unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains(&format!("compiler worker {}", bin.display())),
            "{message}"
        );
        assert!(message.contains("speaks --worker-request-v11"), "{message}");
        assert!(
            message.contains(&format!("this frontend speaks {WORKER_REQUEST_FLAG}")),
            "{message}"
        );
        assert!(
            message.contains("rebuild the worker (see bridge/haskell/CLAUDE.md)"),
            "{message}"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_worker_that_predates_the_probe_flag_is_reported_as_speaking_an_older_protocol() {
        let dir = std::env::temp_dir().join(format!(
            "tidepool-worker-protocol-unknown-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("worker.rs");
        let bin = dir.join("worker");
        // An old worker that has never heard of the probe flag: it doesn't
        // recognize `--print-worker-request-flag` and exits nonzero, the way
        // an argv-parse rejection from before the probe existed would.
        std::fs::write(
            &source,
            r#"
fn main() {
    std::process::exit(2);
}
"#,
        )
        .unwrap();
        #[allow(
            clippy::disallowed_methods,
            reason = "test fixture: compiles a throwaway fake worker binary, not a production launch site"
        )]
        let rustc = std::process::Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&bin)
            .status()
            .unwrap();
        assert!(rustc.success(), "fake worker failed to compile");

        let prepared = PreparedWorker::for_test(bin.clone()).unwrap();
        let error = prepared.check_request_protocol().unwrap_err();
        let message = error.to_string();
        assert!(message.contains("speaks an older protocol"), "{message}");
        assert!(
            message.contains(&format!("this frontend speaks {WORKER_REQUEST_FLAG}")),
            "{message}"
        );

        std::fs::remove_dir_all(dir).ok();
    }
}
