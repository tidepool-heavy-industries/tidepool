use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, ExitStatus};

use crate::request::WORKER_REQUEST_FLAG;
use crate::{daemon, ExtractRequest};

const WORKER_ENV: &str = "TIDEPOOL_EXTRACT_WORKER";
const USAGE: &str = "Usage: tidepool-extract [OPTIONS] <file.hs> ...";

pub fn run(args: Vec<OsString>) -> Result<u8, FrontendError> {
    if args.is_empty() {
        eprintln!("{USAGE}");
        println!("{{\"version\":1,\"diagnostics\":[]}}");
        return Ok(0);
    }
    if args.first().is_some_and(|arg| arg == "--daemon") {
        return daemon::serve(parse_daemon(&args[1..])?, worker_bin()?);
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
    let status = Command::new(worker_bin()?)
        .args(worker_args)
        .status()
        .map_err(FrontendError::Io)?;
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
    let (output, _) = daemon::run_over_daemon(
        PathBuf::from(socket).as_path(),
        &cwd,
        &request.worker_argv(),
    )
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
        [_, payload] => Ok(Some(payload.clone())),
        [_] => Err(FrontendError::Usage(format!(
            "{WORKER_REQUEST_FLAG} requires a payload"
        ))),
        _ => Err(FrontendError::Usage(format!(
            "{WORKER_REQUEST_FLAG} accepts exactly one payload"
        ))),
    }
}

fn worker_bin() -> Result<PathBuf, FrontendError> {
    if let Some(path) = std::env::var_os(WORKER_ENV) {
        return Ok(path.into());
    }
    let current = std::env::current_exe().map_err(FrontendError::Io)?;
    Ok(current.with_file_name("tidepool-extract-bin"))
}

pub(crate) struct DaemonConfig {
    pub socket: PathBuf,
    pub rotate_after: Option<u64>,
    pub rss_ceiling_mb: Option<u64>,
    pub watch_stamp: Option<PathBuf>,
}

fn parse_daemon(args: &[OsString]) -> Result<DaemonConfig, FrontendError> {
    let mut socket = None;
    let mut rotate_after = None;
    let mut rss_ceiling_mb = None;
    let mut watch_stamp = None;
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
    Io(io::Error),
    Daemon(String),
}

impl From<crate::request::CliError> for FrontendError {
    fn from(error: crate::request::CliError) -> Self {
        Self::Usage(error.to_string())
    }
}

impl std::fmt::Display for FrontendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(message) | Self::Daemon(message) => f.write_str(message),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for FrontendError {}
