use std::ffi::{OsStr, OsString};
use std::fmt::Write as FmtWrite;
use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use crate::request::{PRINT_WORKER_REQUEST_FLAG, WORKER_REQUEST_FLAG};
use crate::{daemon, ExtractRequest};

const WORKER_ENV: &str = "TIDEPOOL_EXTRACT_WORKER";
const OWNED_COMPILER_ENV: [&str; 6] = [
    crate::DAEMON_SOCKET_ENV,
    crate::REQUIRED_DAEMON_ENDPOINT_ENV,
    "TIDEPOOL_EXTRACT_NO_DAEMON",
    "TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PID",
    "TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PRODUCER",
    "TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_EPOCH",
];
const USAGE: &str = "Usage: tidepool-extract [OPTIONS] <file.hs> ...";

pub fn run(args: Vec<OsString>) -> Result<u8, FrontendError> {
    crate::process::current_process_dies_with_parent().map_err(FrontendError::Io)?;
    if args.is_empty() {
        return Err(FrontendError::Usage(USAGE.to_owned()));
    }
    if args.first().is_some_and(|arg| arg == "--owned-daemon-run") {
        return owned_daemon_run(&args[1..]);
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
    if args
        .first()
        .is_some_and(|arg| arg == "--compiler-deployment-manifest")
    {
        let [_, path] = args.as_slice() else {
            return Err(FrontendError::Usage(
                "--compiler-deployment-manifest requires exactly one output path".to_owned(),
            ));
        };
        write_compiler_deployment_manifest(Path::new(path))?;
        return Ok(0);
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
    let mut build_products = daemon::BuildProductsNamespace::direct()?;
    let cwd = std::env::current_dir().map_err(FrontendError::Io)?;
    let (worker_args, diagnostics) = build_products.place(&cwd, &worker_args)?;
    io::stderr()
        .write_all(&diagnostics)
        .map_err(FrontendError::Io)?;
    let worker = prepare_worker()?;
    let mut command = worker.command()?;
    command.args(worker_args);
    let status = command.status().map_err(FrontendError::Io)?;
    build_products.cleanup();
    Ok(exit_code(status))
}

struct OwnedSocketDirectory(PathBuf);

impl OwnedSocketDirectory {
    fn cleanup(&self) -> io::Result<()> {
        // The daemon retains its lock inode by design. This namespace has one
        // owner and can be retired only after that daemon and its children reap.
        std::fs::remove_dir_all(&self.0)
    }

    fn create() -> io::Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..32 {
            let path = PathBuf::from(format!(
                "/tmp/tp-owned-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            use std::os::unix::fs::DirBuilderExt;
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {
                    return Ok(Self(path));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::other(
            "owned compiler socket namespace exhausted",
        ))
    }
}

impl Drop for OwnedSocketDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

struct OwnedDaemonChild(std::process::Child);

impl Drop for OwnedDaemonChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn owned_timing_command(program: impl AsRef<OsStr>, timing: Option<&OsStr>) -> Command {
    let mut command = crate::process::command(program);
    // Owned qualification keeps detailed timing by default. Explicit zero
    // selects ordinary worker execution for a matched measurement control.
    command.env(
        "TIDEPOOL_TIMING",
        if timing == Some(OsStr::new("0")) {
            "0"
        } else {
            "1"
        },
    );
    command
}

/// One owner's preflight facts feed both the child and retained lifecycle.
struct OwnedCompilerEvidence<'a> {
    identity: &'a crate::CompilerIdentity,
    epoch: &'a [u8; 32],
    pid: u32,
    socket: &'a Path,
}

impl OwnedCompilerEvidence<'_> {
    fn configure_child(&self, command: &mut Command) {
        command
            .env(crate::DAEMON_SOCKET_ENV, self.socket)
            .env(crate::REQUIRED_DAEMON_ENDPOINT_ENV, self.identity.to_hex())
            .env(
                "TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PID",
                self.pid.to_string(),
            )
            .env(
                "TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PRODUCER",
                self.identity.producer_hex(),
            )
            .env(
                "TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_EPOCH",
                crate::endpoint::hex(self.epoch),
            );
    }

    fn write_lifecycle(&self, report: &Path, confirmed: bool, code: Option<u8>) -> io::Result<()> {
        atomic_write_manifest(report, format!(
            "{{\"schema\":1,\"producer\":{},\"endpoint\":{},\"daemon_epoch\":{},\"daemon_pid\":{},\"socket_path\":{},\"cleanup_confirmed\":{},\"exit_code\":{}}}\n",
            json_string(&self.identity.producer_hex()), json_string(&self.identity.to_hex()),
            json_string(&crate::endpoint::hex(self.epoch)), self.pid,
            json_string(&self.socket.display().to_string()), confirmed,
            code.map_or("null".into(), |value| value.to_string())
        ).as_bytes())
    }
}

fn worker_count(count: u64) -> Result<usize, FrontendError> {
    let count = usize::try_from(count)
        .map_err(|_| FrontendError::Usage("--workers is too large".to_owned()))?;
    if count == 0 {
        return Err(FrontendError::Usage(
            "--workers must be at least 1".to_owned(),
        ));
    }
    Ok(count)
}

struct OwnedInvocation<'a> {
    root: &'a OsStr,
    workers: usize,
    program: &'a OsStr,
    child_args: &'a [OsString],
}

fn parse_owned_invocation(args: &[OsString]) -> Result<OwnedInvocation<'_>, FrontendError> {
    let usage =
        || FrontendError::Usage("--owned-daemon-run ROOT [--workers N] -- PROGRAM [ARGS]".into());
    let (root, remaining) = args.split_first().ok_or_else(usage)?;
    let (workers, remaining) = if remaining.first().is_some_and(|arg| arg == "--workers") {
        let [_, count, rest @ ..] = remaining else {
            return Err(usage());
        };
        let mut count = std::iter::once(count);
        (worker_count(number(&mut count, "--workers")?)?, rest)
    } else {
        (1, remaining)
    };
    let [separator, program, child_args @ ..] = remaining else {
        return Err(usage());
    };
    if separator != "--" {
        return Err(FrontendError::Usage(
            "owned daemon requires an explicit command separator".into(),
        ));
    }
    Ok(OwnedInvocation {
        root,
        workers,
        program,
        child_args,
    })
}

/// Qualify one isolated process against its own exact persistent compiler.
/// The frontend owns all process edges; test helpers do not launch compilers.
#[allow(
    clippy::disallowed_methods,
    reason = "bounded synchronous owned compiler readiness and acknowledged shutdown"
)]
fn owned_daemon_run(args: &[OsString]) -> Result<u8, FrontendError> {
    let OwnedInvocation {
        root,
        workers,
        program,
        child_args,
    } = parse_owned_invocation(args)?;
    let timing = std::env::var_os("TIDEPOOL_TIMING");
    for key in OWNED_COMPILER_ENV {
        if std::env::var_os(key).is_some() {
            return Err(FrontendError::Usage(format!(
                "owned daemon refuses inherited {key}"
            )));
        }
    }
    let frontend = std::env::current_exe().map_err(FrontendError::Io)?;
    let selected = crate::resolve_bin().map_err(|error| FrontendError::Io(error.into()))?;
    if !same_file_path(&frontend, &selected.path)
        || std::env::var_os("TIDEPOOL_EXTRACT").is_none()
        || std::env::var_os("TIDEPOOL_COMPILER_DEPLOYMENT").is_none()
        || std::env::var_os(WORKER_ENV).is_none()
    {
        return Err(FrontendError::Usage(
            "owned daemon requires its explicitly selected frontend, worker and deployment".into(),
        ));
    }
    let direct = crate::ExtractCmd::new()
        .map_err(|error| FrontendError::Io(error.into()))?
        .bind_direct()
        .map_err(|error| FrontendError::Io(error.source))?;
    let expected = direct.identity().clone();
    drop(direct);
    let root = Path::new(root);
    if !root.is_absolute() {
        return Err(FrontendError::Usage(
            "owned daemon root must be absolute and fresh".into(),
        ));
    }
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(root)
        .map_err(FrontendError::Io)?;
    let outcome_path = root.join("owned-compiler-outcome.json");
    atomic_write_manifest(
        &outcome_path,
        b"{\"schema\":1,\"cleanup\":{\"status\":\"unconfirmed\"}}\n",
    )
    .map_err(FrontendError::Io)?;
    let cache = root.join("cache");
    std::fs::create_dir(&cache).map_err(FrontendError::Io)?;
    // Keep the socket within Unix path bounds independently of a long evidence path.
    let socket_directory = OwnedSocketDirectory::create().map_err(FrontendError::Io)?;
    let socket = socket_directory.0.join("compiler.sock");
    let daemon_log = File::create(root.join("daemon.stderr.log")).map_err(FrontendError::Io)?;
    let arguments = crate::persistent_daemon_arguments(
        &socket,
        &root.join("compiler.log"),
        "isolated-qualification",
        workers,
        Some(crate::SESSION_WORKER_RSS_CEILING_MB),
        None,
    );
    let mut command = owned_timing_command(&frontend, timing.as_deref());
    command
        .args(&arguments)
        .env("XDG_CACHE_HOME", &cache)
        .env("TIDEPOOL_CACHE_DIR", cache.join("tidepool"))
        .env("TIDEPOOL_COMPILE_CACHE_DIR", cache.join("artifacts"))
        .env("TIDEPOOL_BUILD_PRODUCTS_DIR", cache.join("products"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(daemon_log));
    let mut daemon = OwnedDaemonChild(command.spawn().map_err(FrontendError::Io)?);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let binding = loop {
        if let Some(status) = daemon.0.try_wait().map_err(FrontendError::Io)? {
            return Err(FrontendError::Daemon(format!(
                "owned compiler exited before readiness: {status}"
            )));
        }
        if let Ok(binding) = daemon::preflight_until(&socket, deadline) {
            break binding;
        }
        if std::time::Instant::now() >= deadline {
            return Err(FrontendError::Daemon(
                "owned compiler readiness timed out; direct fallback forbidden".into(),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    let identity =
        crate::CompilerIdentity::daemon(binding.producer, binding.consumed_worker, binding.epoch);
    if identity.producer_bytes() != expected.producer_bytes()
        || identity.consumed_worker_bytes() != expected.consumed_worker_bytes()
    {
        return Err(FrontendError::Daemon(
            "owned compiler differs from selected direct producer".into(),
        ));
    }
    let report = root.join("lifecycle.json");
    let daemon_pid = daemon.0.id();
    let evidence = OwnedCompilerEvidence {
        identity: &identity,
        epoch: &binding.epoch,
        pid: daemon_pid,
        socket: &socket,
    };
    evidence
        .write_lifecycle(&report, false, None)
        .map_err(FrontendError::Io)?;
    let mut descendants =
        crate::process::descendant_snapshot(daemon_pid).map_err(FrontendError::Io)?;
    let mut child_command = owned_timing_command(program, timing.as_deref());
    evidence.configure_child(&mut child_command);
    let child = child_command
        .args(child_args)
        .env(
            "TIDEPOOL_PERFORMANCE_COMPILER_TRACE",
            root.join("compiler.jsonl"),
        )
        .env("XDG_CACHE_HOME", &cache)
        .env("TIDEPOOL_CACHE_DIR", cache.join("tidepool"))
        .env("TIDEPOOL_COMPILE_CACHE_DIR", cache.join("artifacts"))
        .env("TIDEPOOL_BUILD_PRODUCTS_DIR", cache.join("products"))
        .status()
        .map_err(FrontendError::Io);
    descendants.extend(crate::process::descendant_snapshot(daemon_pid).map_err(FrontendError::Io)?);
    daemon::request_stop(&socket).map_err(|error| FrontendError::Daemon(error.to_string()))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let stopped = loop {
        if let Some(status) = daemon.0.try_wait().map_err(FrontendError::Io)? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            return Err(FrontendError::Daemon(
                "owned compiler shutdown remains unconfirmed".into(),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    if !stopped.success() || socket.exists() {
        return Err(FrontendError::Daemon(format!(
            "owned compiler cleanup remains unconfirmed: {stopped}"
        )));
    }
    for (pid, start) in descendants {
        if crate::process::process_start_ticks(pid).map_err(FrontendError::Io)? == Some(start) {
            return Err(FrontendError::Daemon(format!(
                "owned compiler descendant {pid} remains live or unreaped"
            )));
        }
    }
    socket_directory.cleanup().map_err(FrontendError::Io)?;
    let code = exit_code(child?);
    if code == 0 {
        std::fs::remove_dir_all(&cache).map_err(FrontendError::Io)?;
    }
    evidence
        .write_lifecycle(&report, true, Some(code))
        .map_err(FrontendError::Io)?;
    atomic_write_manifest(
        &outcome_path,
        b"{\"schema\":1,\"cleanup\":{\"status\":\"confirmed\"}}\n",
    )
    .map_err(FrontendError::Io)?;
    Ok(code)
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
        let selection = std::fs::canonicalize(worker_bin()?).map_err(FrontendError::Io)?;
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
        let mut command = self.command()?;
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
            &self.bytes,
            &self.ghc_libdir,
        ))
    }

    pub(crate) fn consumed_worker_identity(&self) -> [u8; 32] {
        *blake3::hash(&self.bytes).as_bytes()
    }

    fn deployment_manifest(&self) -> Result<String, FrontendError> {
        let frontend_bytes = std::fs::read("/proc/self/exe").map_err(FrontendError::Io)?;
        let frontend_path = std::env::current_exe()
            .and_then(|path| std::fs::canonicalize(path))
            .map_err(FrontendError::Io)?;
        let producer_identity =
            crate::endpoint::producer_identity(&frontend_bytes, &self.bytes, &self.ghc_libdir);
        let frontend_path = json_path(&frontend_path)?;
        let worker_path = json_path(&self.selection)?;
        let ghc_libdir = json_os_str(&self.ghc_libdir)?;
        Ok(format!(
            "{{\n  \"schema\": 1,\n  \"producer_identity\": {producer_identity:?},\n  \"consumed_worker_identity\": {:?},\n  \"frontend_path\": {frontend_path},\n  \"worker_path\": {worker_path},\n  \"ghc_libdir\": {ghc_libdir}\n}}\n",
            self.consumed_worker_identity()
        ))
    }

    pub(crate) fn command(&self) -> Result<Command, FrontendError> {
        // The opened descriptor pins the selected inode. `execve` resolves
        // this path before applying close-on-exec, so an atomic replacement of
        // the worker path cannot change which bytes execute.
        let executable = format!("/proc/self/fd/{}", self.file.as_raw_fd());
        let mut command = crate::process::command(executable);
        command.env("TIDEPOOL_GHC_LIBDIR", &self.ghc_libdir);
        // Candidate manifests cannot choose their own compiler authority. The
        // pinned process owner supplies it independently at worker launch.
        command.env(
            "TIDEPOOL_COMPILER_PRODUCER",
            crate::endpoint::hex(&self.producer_identity()?),
        );
        Ok(command)
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

fn write_compiler_deployment_manifest(path: &Path) -> Result<(), FrontendError> {
    let prepared = PreparedWorker::prepare()?;
    let frontend_path = std::env::current_exe()
        .and_then(|path| std::fs::canonicalize(path))
        .map_err(FrontendError::Io)?;
    if same_file_path(path, &frontend_path) || same_file_path(path, &prepared.selection) {
        return Err(FrontendError::Usage(
            "deployment manifest path must not replace the configured frontend or worker"
                .to_owned(),
        ));
    }
    let contents = prepared.deployment_manifest()?;
    atomic_write_manifest(path, contents.as_bytes()).map_err(FrontendError::Io)
}

fn same_file_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    left.exists()
        && right.exists()
        && std::fs::canonicalize(left).ok() == std::fs::canonicalize(right).ok()
}

fn json_path(path: &Path) -> Result<String, FrontendError> {
    let text = path.to_str().ok_or_else(|| {
        FrontendError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "compiler deployment paths must be UTF-8",
        ))
    })?;
    Ok(json_string(text))
}

fn json_os_str(path: &OsStr) -> Result<String, FrontendError> {
    let text = path.to_str().ok_or_else(|| {
        FrontendError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "compiler deployment paths must be UTF-8",
        ))
    })?;
    Ok(json_string(text))
}

fn json_string(text: &str) -> String {
    let mut output = String::with_capacity(text.len() + 2);
    output.push('"');
    for character in text.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character.is_control() => {
                let _ = write!(output, "\\u{:04x}", character as u32);
            }
            character => output.push(character),
        }
    }
    output.push('"');
    output
}

fn atomic_write_manifest(path: &Path, bytes: &[u8]) -> io::Result<()> {
    tidepool_atomic_write::write_durable(path, bytes).map_err(Into::into)
}

fn prepare_worker() -> Result<PreparedWorker, FrontendError> {
    PreparedWorker::prepare()
}

fn resolve_ghc_libdir() -> Result<OsString, FrontendError> {
    if let Some(value) = std::env::var_os("TIDEPOOL_GHC_LIBDIR") {
        return Ok(value);
    }
    let output = crate::process::command("ghc")
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
    let consumed_worker = prepared.consumed_worker_identity();
    crate::endpoint::write_identity(io::stdout().lock(), &identity, &consumed_worker)
        .map_err(FrontendError::Io)?;
    let mut stdin = io::stdin().lock();
    let mut prefix = [0u8; 8];
    stdin.read_exact(&mut prefix).map_err(FrontendError::Io)?;
    if &prefix == daemon::DIRECT_TRANSACTION {
        let mut worker = daemon::Worker::spawn(&prepared)?;
        let result = (|| {
            worker.begin_transaction()?;
            io::stdout().write_all(&[1]).map_err(FrontendError::Io)?;
            io::stdout().flush().map_err(FrontendError::Io)?;
            loop {
                let mut command = [0u8; 1];
                // EOF is abandonment, not an implicit successful END.
                stdin.read_exact(&mut command).map_err(FrontendError::Io)?;
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
            worker.end_transaction()
        })();
        let result = match result {
            Ok(()) => worker.shutdown_confirmed(),
            Err(primary) => {
                worker.abort();
                Err(primary)
            }
        };
        if let Err(error) = &result {
            if let Some(report) = error.close_report() {
                if let Err(report_error) =
                    crate::endpoint::write_failure_end(&mut io::stdout().lock(), &report)
                {
                    tracing::warn!(%report_error, "could not deliver compiler failure END evidence");
                }
            }
        }
        return result.map(|()| 0);
    }
    let mut request = io::Cursor::new(prefix).chain(stdin);
    let (cwd, argv) = daemon::read_request(&mut request)?;
    let worker_argv = daemon::normalize_worker_argv(argv)?;
    let mut worker = daemon::Worker::spawn(&prepared)?;
    let result: Result<_, FrontendError> = (|| {
        worker.begin_transaction()?;
        let (code, stdout, stderr) = worker.request(&cwd, &worker_argv)?;
        worker.end_transaction()?;
        Ok((code, stdout, stderr))
    })();
    if result.is_ok() {
        worker.shutdown();
    } else {
        // A rejected frame can leave the worker blocked writing its unread
        // body. Waiting for stdin EOF would not make that worker exit.
        worker.abort();
    }
    // A successful response releases the caller's direct endpoint, which can
    // immediately terminate this frontend. Reap and retire scratch ownership
    // before publishing that final response.
    drop(worker);
    let (code, stdout, stderr) = result?;
    daemon::write_response(io::stdout().lock(), code, &stdout, &stderr)?;
    Ok(0)
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
    pub foreground_jobs: Option<usize>,
    pub preparation_jobs: Option<usize>,
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
    let mut foreground_jobs = None;
    let mut preparation_jobs = None;
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
            "--foreground-jobs" | "--preparation-jobs" => {
                let count = number(&mut args, option)?;
                if count == 0 || count > u64::from(u32::MAX) {
                    return Err(FrontendError::Usage(format!(
                        "{option} requires a positive 32-bit job count"
                    )));
                }
                let count = usize::try_from(count)
                    .map_err(|_| FrontendError::Usage(format!("{option} is too large")))?;
                if option == "--foreground-jobs" {
                    foreground_jobs = Some(count);
                } else {
                    preparation_jobs = Some(count);
                }
            }
            "--workers" => {
                let count = number(&mut args, option)?;
                workers = Some(worker_count(count)?);
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
        foreground_jobs,
        preparation_jobs,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScratchCleanupPhase {
    Products,
    EmptyNamespace,
}

#[derive(Debug)]
pub struct ScratchCleanupFailure {
    pub path: PathBuf,
    pub phase: ScratchCleanupPhase,
    pub source: io::Error,
}

#[derive(Debug)]
pub enum FrontendError {
    Usage(String),
    ScratchCleanup {
        status: std::process::ExitStatus,
        failures: Vec<ScratchCleanupFailure>,
    },
    WorkerWait(io::Error),
    WorkerClose {
        status: std::process::ExitStatus,
        scratch: Vec<ScratchCleanupFailure>,
    },
    WorkerProtocol(crate::request::ProtocolError),
    /// The resolved worker binary's `--print-worker-request-flag` output
    /// (`None` if it doesn't understand the probe or exited nonzero) doesn't
    /// match [`WORKER_REQUEST_FLAG`]. Checked once per resolved worker binary
    /// in [`PreparedWorker::prepare`], before any real request is sent.
    WorkerVersionMismatch {
        path: PathBuf,
        worker_flag: Option<String>,
    },
    /// The accepted caller disconnected, so the daemon retired its pinned worker.
    /// The request remains indeterminate and must not be replayed.
    WorkerClientDisconnected,
    Io(io::Error),
    Daemon(String),
}

impl FrontendError {
    fn close_report(&self) -> Option<crate::endpoint::CompilerFrontendCloseReport> {
        use crate::endpoint::{
            CompilerFrontendCloseReport, CompilerIoCause, CompilerScratchFailure,
            CompilerScratchRetirement, CompilerWorkerRetirement,
        };
        let scratch_failures = |failures: &[ScratchCleanupFailure]| {
            CompilerScratchRetirement::Unconfirmed(
                failures
                    .iter()
                    .map(|failure| CompilerScratchFailure {
                        path: failure.path.clone(),
                        phase: failure.phase,
                        cause: CompilerIoCause::from(&failure.source),
                    })
                    .collect(),
            )
        };
        match self {
            Self::ScratchCleanup { status, failures } => Some(CompilerFrontendCloseReport {
                worker: CompilerWorkerRetirement::Reaped(*status),
                scratch: scratch_failures(failures),
            }),
            Self::WorkerClose { status, scratch } => Some(CompilerFrontendCloseReport {
                worker: CompilerWorkerRetirement::Reaped(*status),
                scratch: if scratch.is_empty() {
                    CompilerScratchRetirement::Confirmed
                } else {
                    scratch_failures(scratch)
                },
            }),
            Self::WorkerWait(cause) => Some(CompilerFrontendCloseReport {
                worker: CompilerWorkerRetirement::WaitUnconfirmed(CompilerIoCause::from(cause)),
                scratch: CompilerScratchRetirement::NotObserved,
            }),
            _ => None,
        }
    }
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
            Self::ScratchCleanup { status, failures } => write!(f, "compiler worker reaped with status {status}; scratch cleanup unconfirmed: {failures:?}"),
            Self::WorkerWait(error) => write!(f, "compiler worker wait unconfirmed: {error}"),
            Self::WorkerClose { status, scratch } => write!(f, "compiler worker reaped with status {status}; scratch cleanup failures: {scratch:?}"),
            Self::WorkerClientDisconnected => {
                f.write_str("compiler worker retired after client disconnected")
            }
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
            foreground_jobs: None,
            preparation_jobs: None,
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
            foreground_jobs: None,
            preparation_jobs: None,
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
    fn owned_compiler_requires_an_explicit_child_command() {
        assert!(matches!(
            owned_daemon_run(&[]),
            Err(FrontendError::Usage(_))
        ));
        assert!(matches!(
            owned_daemon_run(&["/tmp/fresh".into(), "--other".into(), "test".into()]),
            Err(FrontendError::Usage(_))
        ));
    }

    fn owned_evidence_property_config() -> proptest::test_runner::Config {
        let mut config = proptest::test_runner::Config::default();
        if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
            config.failure_persistence = Some(Box::new(
                proptest::test_runner::FileFailurePersistence::Direct(path),
            ));
        }
        config
    }

    proptest::proptest! {
        #![proptest_config(owned_evidence_property_config())]

        #[test]
        fn owned_measurement_handoff_preserves_identity_across_lifecycle_transitions(
            producer in proptest::prelude::any::<[u8; 32]>(),
            worker in proptest::prelude::any::<[u8; 32]>(),
            epoch in proptest::prelude::any::<[u8; 32]>(),
            pid in 1_u32..u32::MAX,
            code in proptest::prelude::any::<u8>(),
            suffix in "[a-z\"\\\\]{0,12}",
        ) {
            let directory = tempfile::tempdir().unwrap();
            let report = directory.path().join("lifecycle.json");
            let socket = directory.path().join(format!("{suffix}.sock"));
            let identity = crate::CompilerIdentity::daemon(producer, worker, epoch);
            let evidence = OwnedCompilerEvidence { identity: &identity, epoch: &epoch, pid, socket: &socket };
            let mut command = Command::new("unused-child");
            for key in OWNED_COMPILER_ENV { command.env(key, "foreign-owner"); }
            evidence.configure_child(&mut command);
            // Readiness and settled reports must carry the same issued facts;
            // only terminal cleanup and exit evidence change.
            for (confirmed, exit) in [(false, None), (true, Some(code))] {
                evidence.write_lifecycle(&report, confirmed, exit).unwrap();
                let lifecycle: serde_json::Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
                let environment: std::collections::BTreeMap<_, _> = command.get_envs()
                    .filter_map(|(key, value)| value.map(|value| (key.to_string_lossy().into_owned(), value.to_string_lossy().into_owned())))
                    .collect();
                proptest::prop_assert_eq!(lifecycle["daemon_pid"].as_u64(), Some(u64::from(pid)));
                proptest::prop_assert_eq!(environment["TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PID"].as_str(), pid.to_string());
                for (key, field, expected) in [
                    ("TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PRODUCER", "producer", producer.iter().map(|byte| format!("{byte:02x}")).collect::<String>()),
                    ("TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_EPOCH", "daemon_epoch", epoch.iter().map(|byte| format!("{byte:02x}")).collect::<String>()),
                    (crate::DAEMON_SOCKET_ENV, "socket_path", socket.display().to_string()),
                ] {
                    proptest::prop_assert_eq!(&environment[key], &expected);
                    proptest::prop_assert_eq!(lifecycle[field].as_str(), Some(expected.as_str()));
                }
                proptest::prop_assert_eq!(lifecycle["cleanup_confirmed"].as_bool(), Some(confirmed));
                proptest::prop_assert_eq!(lifecycle["exit_code"].as_u64(), exit.map(u64::from));
                proptest::prop_assert_eq!(lifecycle["endpoint"].as_str(), Some(environment[crate::REQUIRED_DAEMON_ENDPOINT_ENV].as_str()));
            }
        }
    }

    #[test]
    fn owned_compiler_workers_flow_to_existing_admission_command() {
        for (options, expected) in [
            (vec![], 1),
            (vec!["--workers", "1"], 1),
            (vec!["--workers", "2"], 2),
        ] {
            let mut args = vec![OsString::from("/tmp/owned")];
            args.extend(options.into_iter().map(OsString::from));
            args.extend(["--", "/selected/child", "--child-option"].map(OsString::from));
            let invocation = parse_owned_invocation(&args).unwrap();
            assert_eq!(invocation.root, OsStr::new("/tmp/owned"));
            assert_eq!(invocation.program, OsStr::new("/selected/child"));
            assert_eq!(invocation.child_args, &[OsString::from("--child-option")]);
            let arguments = crate::persistent_daemon_arguments(
                Path::new("/tmp/owned.sock"),
                Path::new("/tmp/compiler.jsonl"),
                "isolated-qualification",
                invocation.workers,
                Some(crate::SESSION_WORKER_RSS_CEILING_MB),
                None,
            );
            let config = parse_daemon(&arguments[1..]).unwrap();
            assert_eq!(config.workers, Some(expected));
            assert!(config.persistent);
            assert_eq!(
                config.rss_ceiling_mb,
                Some(crate::SESSION_WORKER_RSS_CEILING_MB)
            );
        }
        for options in [
            vec!["--workers", "0", "--", "child"],
            vec!["--workers", "-1", "--", "child"],
            vec!["--workers", "18446744073709551616", "--", "child"],
            vec!["--workers", "invalid", "--", "child"],
            vec!["--workers"],
            vec!["--workers", "2", "--"],
            vec!["--workers", "2", "--workers", "2", "--", "child"],
        ] {
            let mut args = vec![OsString::from("/tmp/owned")];
            args.extend(options.into_iter().map(OsString::from));
            assert!(parse_owned_invocation(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn owned_compiler_timing_controls_daemon_and_child_commands() {
        let daemon_arguments = crate::persistent_daemon_arguments(
            Path::new("/tmp/owned.sock"),
            Path::new("/tmp/compiler.jsonl"),
            "isolated-qualification",
            1,
            Some(crate::SESSION_WORKER_RSS_CEILING_MB),
            None,
        );
        for (timing, expected) in [
            (None, "1"),
            (Some(OsStr::new("0")), "0"),
            (Some(OsStr::new("1")), "1"),
        ] {
            let mut daemon = owned_timing_command("/selected/frontend", timing);
            daemon.args(&daemon_arguments);
            let mut child = owned_timing_command("/selected/libtest", timing);
            child.args(["--exact", "selected_case"]);
            for command in [&daemon, &child] {
                assert_eq!(
                    command
                        .get_envs()
                        .find(|(key, _)| *key == OsStr::new("TIDEPOOL_TIMING"))
                        .and_then(|(_, value)| value),
                    Some(OsStr::new(expected))
                );
            }
            assert_eq!(
                daemon.get_args().collect::<Vec<_>>(),
                daemon_arguments
                    .iter()
                    .map(OsString::as_os_str)
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                child.get_args().collect::<Vec<_>>(),
                [OsStr::new("--exact"), OsStr::new("selected_case")]
            );
            let configuration = parse_daemon(&daemon_arguments[1..]).unwrap();
            assert!(configuration.persistent);
            assert_eq!(configuration.workers, Some(1));
            assert_eq!(
                configuration.rss_ceiling_mb,
                Some(crate::SESSION_WORKER_RSS_CEILING_MB)
            );
        }
    }

    #[test]
    fn shared_persistent_command_keeps_production_worker_limits() {
        let arguments = crate::persistent_daemon_arguments(
            Path::new("/tmp/owned.sock"),
            Path::new("/tmp/compiler.jsonl"),
            "case",
            1,
            Some(7168),
            None,
        );
        let configuration = parse_daemon(&arguments[1..]).unwrap();
        assert!(configuration.persistent);
        assert_eq!(configuration.workers, Some(1));
        assert_eq!(configuration.rss_ceiling_mb, Some(7168));
    }

    #[test]
    fn persistent_daemon_command_forwards_only_an_explicit_foreground_job_limit() {
        for (jobs, expected, expected_width) in [
            (None, None, 2),
            (std::num::NonZeroUsize::new(8), Some(8), 8),
        ] {
            let arguments = crate::persistent_daemon_arguments(
                Path::new("/tmp/owned.sock"),
                Path::new("/tmp/compiler.jsonl"),
                "case",
                2,
                Some(10 * 1024),
                jobs,
            );
            let configuration = parse_daemon(&arguments[1..]).unwrap();
            assert_eq!(configuration.foreground_jobs, expected);
            assert_eq!(
                configuration
                    .foreground_jobs
                    .unwrap_or(daemon::DEFAULT_FOREGROUND_JOBS),
                expected_width
            );
            assert_eq!(configuration.workers, Some(2));
            assert_eq!(configuration.rss_ceiling_mb, Some(10 * 1024));
        }
    }

    #[test]
    fn compiler_job_limits_are_explicit_positive_allowances() {
        for count in [2, 4, 8, 16] {
            let config = parse_daemon(&[
                "--socket".into(),
                "/tmp/compiler.sock".into(),
                "--foreground-jobs".into(),
                count.to_string().into(),
                "--preparation-jobs".into(),
                count.to_string().into(),
            ])
            .unwrap();
            assert_eq!(config.foreground_jobs, Some(count));
            assert_eq!(config.preparation_jobs, Some(count));
        }
        assert!(parse_daemon(&[
            "--socket".into(),
            "/tmp/compiler.sock".into(),
            "--foreground-jobs".into(),
            "0".into()
        ])
        .is_err());
        assert!(parse_daemon(&[
            "--socket".into(),
            "/tmp/compiler.sock".into(),
            "--preparation-jobs".into(),
            "0".into()
        ])
        .is_err());
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
        let old_worker_identity = prepared.consumed_worker_identity();
        let command = prepared.command().unwrap();
        let configured_producer = command
            .get_envs()
            .find(|(name, _)| *name == OsStr::new("TIDEPOOL_COMPILER_PRODUCER"))
            .and_then(|(_, value)| value);
        let expected_producer = crate::endpoint::hex(&old_identity);
        assert_eq!(configured_producer, Some(OsStr::new(&expected_producer)));
        let manifest_path = dir.join("compiler-deployment.json");
        write_compiler_deployment_manifest(&manifest_path).unwrap();
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["schema"], 1);
        assert_eq!(
            manifest["producer_identity"],
            serde_json::to_value(old_identity).unwrap()
        );
        assert_eq!(
            manifest["consumed_worker_identity"],
            serde_json::to_value(old_worker_identity).unwrap()
        );
        assert_eq!(
            manifest["frontend_path"],
            std::fs::canonicalize(std::env::current_exe().unwrap())
                .unwrap()
                .to_string_lossy()
                .as_ref()
        );
        assert_eq!(manifest["worker_path"], worker.to_string_lossy().as_ref());
        assert_eq!(manifest["ghc_libdir"], "/ghc/lib-a");

        let alias = dir.join("worker-alias");
        std::os::unix::fs::symlink(&worker, &alias).unwrap();
        let mut relative_alias: PathBuf = std::env::current_dir()
            .unwrap()
            .ancestors()
            .skip(1)
            .map(|_| "..")
            .collect();
        relative_alias.push(alias.strip_prefix("/").unwrap());
        std::env::set_var(WORKER_ENV, relative_alias);
        let aliased = PreparedWorker::prepare().unwrap();
        std::fs::remove_file(&alias).unwrap();
        assert_eq!(aliased.selection(), prepared.selection());
        assert_eq!(aliased.producer_identity().unwrap(), old_identity);
        let aliased_manifest: serde_json::Value =
            serde_json::from_str(&aliased.deployment_manifest().unwrap()).unwrap();
        assert_eq!(aliased_manifest["worker_path"], manifest["worker_path"]);

        let retained_path = dir.join("run-worker");
        std::fs::copy(&worker, &retained_path).unwrap();
        std::env::set_var(WORKER_ENV, &retained_path);
        let retained = PreparedWorker::prepare().unwrap();
        assert_eq!(retained.producer_identity().unwrap(), old_identity);
        assert_eq!(retained.consumed_worker_identity(), old_worker_identity);
        let retained_manifest: serde_json::Value =
            serde_json::from_str(&retained.deployment_manifest().unwrap()).unwrap();
        assert_ne!(retained_manifest["worker_path"], manifest["worker_path"]);
        assert_eq!(
            retained_manifest["producer_identity"],
            manifest["producer_identity"]
        );
        let output = retained
            .command()
            .unwrap()
            .env("TIDEPOOL_PREPARED_WORKER_CHILD", "1")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"old-worker");
        std::env::set_var(WORKER_ENV, &worker);

        compile_fake_worker(&dir.join("replacement.rs"), &replacement, "new-worker");
        std::fs::rename(&replacement, &worker).unwrap();
        assert_eq!(
            prepared.producer_identity().unwrap(),
            old_identity,
            "a boot-bound worker must retain immutable identity after path replacement"
        );
        assert_eq!(
            prepared.consumed_worker_identity(),
            old_worker_identity,
            "a boot-bound worker must retain its exact consumed digest after path replacement"
        );

        let output = prepared
            .command()
            .unwrap()
            .env("TIDEPOOL_PREPARED_WORKER_CHILD", "1")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("old-worker"));

        let replacement_worker = PreparedWorker::prepare().unwrap();
        let new_identity = replacement_worker.producer_identity().unwrap();
        assert_ne!(old_identity, new_identity);
        assert_ne!(
            old_worker_identity,
            replacement_worker.consumed_worker_identity()
        );

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

    #[cfg(target_os = "linux")]
    #[test]
    fn protocol_probe_uses_the_owned_parent_death_contract() {
        let work = tempfile::tempdir().unwrap();
        let source = work.path().join("worker.rs");
        let binary = work.path().join("worker");
        std::fs::write(
            &source,
            format!(
                r#"
unsafe extern "C" {{
    fn prctl(option: i32, ...) -> i32;
}}
fn main() {{
    assert_eq!(std::env::args().nth(1).as_deref(), Some("{probe}"));
    let mut signal = 0i32;
    assert_eq!(unsafe {{ prctl(2, &mut signal as *mut i32, 0usize, 0usize, 0usize) }}, 0);
    assert_eq!(signal, 9, "protocol probe lacks its owned parent-death signal");
    print!("{flag}");
}}
"#,
                probe = PRINT_WORKER_REQUEST_FLAG,
                flag = WORKER_REQUEST_FLAG,
            ),
        )
        .unwrap();
        #[allow(
            clippy::disallowed_methods,
            reason = "test fixture: compile a worker that checks its actual Linux parent-death signal"
        )]
        let status = Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .status()
            .unwrap();
        assert!(status.success(), "protocol probe fixture failed to compile");
        PreparedWorker::for_test(binary)
            .unwrap()
            .check_request_protocol()
            .unwrap();
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
