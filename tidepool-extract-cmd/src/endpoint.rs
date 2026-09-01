use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Instant;

use crate::{daemon, process, ExtractCmd, ExtractRun, SpawnError};

pub(crate) const BOUND_ENDPOINT_FLAG: &str = "--compiler-endpoint-v1";
pub(crate) const IDENTITY_MAGIC: &[u8; 8] = b"TPCID001";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompilerIdentity {
    producer: [u8; 32],
    endpoint: [u8; 32],
}

impl CompilerIdentity {
    pub(crate) fn direct(producer: [u8; 32]) -> Self {
        Self {
            producer,
            endpoint: producer,
        }
    }

    pub(crate) fn daemon(producer: [u8; 32], epoch: [u8; 32]) -> Self {
        let mut hasher = blake3::Hasher::new();
        frame(&mut hasher, b"tidepool-daemon-endpoint-v1");
        frame(&mut hasher, &producer);
        frame(&mut hasher, &epoch);
        Self {
            producer,
            endpoint: *hasher.finalize().as_bytes(),
        }
    }

    /// Identity of the exact bound endpoint, including a daemon's boot epoch.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.endpoint
    }

    /// Stable producer identity used by the deploy handshake.
    pub fn producer_bytes(&self) -> &[u8; 32] {
        &self.producer
    }

    pub fn to_hex(&self) -> String {
        hex(&self.endpoint)
    }

    pub fn producer_hex(&self) -> String {
        hex(&self.producer)
    }
}

impl std::fmt::Display for CompilerIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

pub(crate) fn producer_identity(
    frontend: &[u8],
    worker_selection: &OsStr,
    worker: &[u8],
    ghc_libdir: &OsStr,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    frame(&mut hasher, b"tidepool-compiler-producer-v1");
    frame(&mut hasher, frontend);
    frame(&mut hasher, worker_selection.as_encoded_bytes());
    frame(&mut hasher, worker);
    frame(&mut hasher, ghc_libdir.as_encoded_bytes());
    *hasher.finalize().as_bytes()
}

fn frame(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

#[derive(Debug)]
pub(crate) struct LaunchSpec {
    pub(crate) program: OsString,
    pub(crate) prefix: Vec<OsString>,
}

impl LaunchSpec {
    pub(crate) fn direct(program: OsString) -> Self {
        Self {
            program,
            prefix: Vec::new(),
        }
    }

    pub(crate) fn nix(flake_root: &Path) -> Self {
        Self {
            program: "nix".into(),
            prefix: vec![
                "run".into(),
                format!("{}#{}", flake_root.display(), crate::DEFAULT_BIN).into(),
                "--".into(),
            ],
        }
    }
}

#[derive(Debug)]
pub struct CompilerEndpoint {
    identity: CompilerIdentity,
    transport: Transport,
}

#[derive(Debug)]
enum Transport {
    Direct(DirectEndpoint),
    Daemon { socket: PathBuf, epoch: [u8; 32] },
}

#[derive(Debug)]
struct DirectEndpoint {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    program: OsString,
}

impl Drop for DirectEndpoint {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl CompilerEndpoint {
    pub(crate) fn bind(cmd: &ExtractCmd) -> Result<Self, SpawnError> {
        if let Some(socket) = std::env::var_os("TIDEPOOL_EXTRACT_DAEMON_SOCKET") {
            let socket = PathBuf::from(socket);
            if let Ok(binding) = daemon::preflight(&socket) {
                return Ok(Self {
                    identity: CompilerIdentity::daemon(binding.producer, binding.epoch),
                    transport: Transport::Daemon {
                        socket,
                        epoch: binding.epoch,
                    },
                });
            }
        }
        Self::bind_launch(LaunchSpec::direct(cmd.program.clone()))
    }

    pub(crate) fn bind_nix(flake_root: &Path) -> Result<Self, SpawnError> {
        Self::bind_launch(LaunchSpec::nix(flake_root))
    }

    fn bind_launch(spec: LaunchSpec) -> Result<Self, SpawnError> {
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.prefix)
            .arg(BOUND_ENDPOINT_FLAG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        process::child_dies_with_parent(&mut command);
        let mut child = command
            .spawn()
            .map_err(|source| SpawnError::not_submitted(spec.program.clone(), source))?;
        let stdin = child.stdin.take().ok_or_else(|| {
            SpawnError::not_submitted(
                spec.program.clone(),
                io::Error::other("bound endpoint stdin was not piped"),
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            SpawnError::not_submitted(
                spec.program.clone(),
                io::Error::other("bound endpoint stdout was not piped"),
            )
        })?;
        // Construct the owner before reading the handshake so every failure
        // path closes and reaps the child rather than leaking a half-bound
        // compiler process.
        let mut direct = DirectEndpoint {
            child,
            stdin: Some(stdin),
            stdout,
            program: spec.program.clone(),
        };
        let mut magic = [0u8; 8];
        let mut producer = [0u8; 32];
        direct
            .stdout
            .read_exact(&mut magic)
            .map_err(|source| SpawnError::not_submitted(spec.program.clone(), source))?;
        if &magic != IDENTITY_MAGIC {
            return Err(SpawnError::not_submitted(
                spec.program.clone(),
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid compiler endpoint identity",
                ),
            ));
        }
        direct
            .stdout
            .read_exact(&mut producer)
            .map_err(|source| SpawnError::not_submitted(spec.program.clone(), source))?;
        Ok(Self {
            identity: CompilerIdentity::direct(producer),
            transport: Transport::Direct(direct),
        })
    }

    pub fn identity(&self) -> &CompilerIdentity {
        &self.identity
    }

    /// Execute `cmd` through the producer captured by this endpoint.
    pub fn execute(mut self, cmd: &ExtractCmd) -> Result<ExtractRun, SpawnError> {
        let cwd = std::env::current_dir()
            .map_err(|source| SpawnError::not_submitted("current directory", source))?;
        let start = Instant::now();
        let output = match &mut self.transport {
            Transport::Direct(endpoint) => {
                let request = daemon::encode_request(&cwd, &cmd.request.worker_argv());
                let mut stdin = endpoint.stdin.take().ok_or_else(|| {
                    SpawnError::indeterminate(
                        endpoint.program.clone(),
                        io::Error::new(io::ErrorKind::BrokenPipe, "bound endpoint is closed"),
                    )
                })?;
                stdin.write_all(&request).map_err(|source| {
                    SpawnError::indeterminate(endpoint.program.clone(), source)
                })?;
                stdin.flush().map_err(|source| {
                    SpawnError::indeterminate(endpoint.program.clone(), source)
                })?;
                drop(stdin);
                crate::EXTRACT_SPAWNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                daemon::decode_output(&mut endpoint.stdout).map_err(|error| {
                    SpawnError::indeterminate(
                        endpoint.program.clone(),
                        io::Error::other(error.to_string()),
                    )
                })?
            }
            Transport::Daemon { socket, epoch } => {
                match daemon::execute(socket, epoch, &cwd, &cmd.request.worker_argv()) {
                    Ok(output) => {
                        crate::EXTRACT_SPAWNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        output
                    }
                    Err(error) => {
                        if error.was_accepted() {
                            crate::EXTRACT_SPAWNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                        let source = io::Error::other(error.to_string());
                        if error.is_not_accepted() {
                            return Err(SpawnError::not_submitted(socket.as_os_str(), source));
                        } else {
                            return Err(SpawnError::indeterminate(socket.as_os_str(), source));
                        }
                    }
                }
            }
        };
        Ok(ExtractRun {
            output,
            elapsed: start.elapsed(),
        })
    }
}

pub(crate) fn write_identity(mut writer: impl Write, producer: &[u8; 32]) -> io::Result<()> {
    writer.write_all(IDENTITY_MAGIC)?;
    writer.write_all(producer)?;
    writer.flush()
}
