use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};

use crate::request::WORKER_REQUEST_FLAG;
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
        daemon::init_tracing(&config)?;
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
        Ok(Self {
            file,
            selection,
            bytes,
            ghc_libdir,
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
        let mut command = Command::new(executable);
        command.env("TIDEPOOL_GHC_LIBDIR", &self.ghc_libdir);
        command
    }

    pub(crate) fn selection(&self) -> &std::path::Path {
        &self.selection
    }
}

fn prepare_worker() -> Result<PreparedWorker, FrontendError> {
    PreparedWorker::prepare()
}

fn resolve_ghc_libdir() -> Result<OsString, FrontendError> {
    if let Some(value) = std::env::var_os("TIDEPOOL_GHC_LIBDIR") {
        return Ok(value);
    }
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
    let (cwd, argv) = daemon::read_request(&mut stdin)?;
    let worker_argv = daemon::normalize_worker_argv(argv)?;
    let mut worker = daemon::Worker::spawn(&prepared)?;
    let result = worker.request(&cwd, &worker_argv);
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
    pub watch_stamp: Option<PathBuf>,
    pub persistent: bool,
    pub run_id: Option<String>,
    pub log_path: Option<PathBuf>,
}

fn parse_daemon(args: &[OsString]) -> Result<DaemonConfig, FrontendError> {
    let mut socket = None;
    let mut rotate_after = None;
    let mut rss_ceiling_mb = None;
    let mut watch_stamp = None;
    let mut persistent = false;
    let mut run_id = None;
    let mut log_path = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let option = arg
            .to_str()
            .ok_or_else(|| FrontendError::Usage("daemon options must be UTF-8".to_owned()))?;
        match option {
            "--socket" => socket = Some(PathBuf::from(next(&mut args, option)?)),
            "--rotate-after" => rotate_after = Some(number(&mut args, option)?),
            "--rss-ceiling-mb" => rss_ceiling_mb = Some(number(&mut args, option)?),
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
        watch_stamp,
        persistent,
        run_id,
        log_path,
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
        let error = worker_payload(&[
            WORKER_REQUEST_FLAG.into(),
            "54505245513030360100000009".into(),
        ])
        .unwrap_err();
        assert!(matches!(
            error,
            FrontendError::WorkerProtocol(crate::request::ProtocolError::RetiredFieldTag(9))
        ));
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

    #[test]
    fn prepared_worker_identity_and_execution_survive_path_replacement() {
        use std::os::unix::fs::PermissionsExt;

        let _env = ENV.lock().unwrap();
        let dir =
            std::env::temp_dir().join(format!("tidepool-prepared-worker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let worker = dir.join("worker");
        let replacement = dir.join("replacement");
        std::fs::copy(std::env::current_exe().unwrap(), &worker).unwrap();
        std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var(WORKER_ENV, &worker);
        std::env::set_var("TIDEPOOL_GHC_LIBDIR", "/ghc/lib-a");

        let prepared = PreparedWorker::prepare().unwrap();
        let old_identity = prepared.producer_identity().unwrap();

        std::fs::write(&replacement, b"#!/bin/sh\nprintf new-worker").unwrap();
        std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(&replacement, &worker).unwrap();
        assert_eq!(
            prepared.producer_identity().unwrap(),
            old_identity,
            "a boot-bound worker must retain immutable identity after path replacement"
        );

        let output = prepared
            .command()
            .args([
                "--exact",
                "frontend::tests::prepared_worker_child",
                "--nocapture",
            ])
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
    fn prepared_worker_child() {
        if std::env::var_os("TIDEPOOL_PREPARED_WORKER_CHILD").is_some() {
            print!("old-worker");
        }
    }
}
