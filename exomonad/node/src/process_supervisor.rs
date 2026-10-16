//! Private, one-launch process supervisor protocol.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::{
    ProcessInvocation, ProcessMountBoundary, ScopeCapability, ScopeObservation, ServiceEnvironment,
    ServiceScopeCleanup, ServiceScopeError, ServiceStdio,
};

pub const PROCESS_SUPERVISOR_VERSION: u32 = 2;
// Per-message transport budget, including the terminating newline.
const MAX_SUPERVISOR_FRAME_BYTES: usize = 64 * 1024;
const MAX_OPERATION_MS: u64 = 120_000;
const UNAUTHENTICATED_READ_TIMEOUT: Duration = Duration::from_millis(250);
const PRIMARY_LEASE: Duration = Duration::from_secs(2);

pub const PROCESS_SUPERVISOR_MANIFEST: &str = "manifest.json";
pub const PROCESS_SUPERVISOR_SOCKET: &str = "scope.sock";
pub const PROCESS_SUPERVISOR_CHECKPOINT: &str = "checkpoint.json";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessSupervisorManifest {
    pub version: u32,
    pub launch_id: String,
    pairing_secret: String,
    recovery_secret: String,
    pub private_directory: PathBuf,
    pub bubblewrap: PathBuf,
    pub boundary: ProcessMountBoundary,
    pub command: ProcessInvocation,
    #[serde(default)]
    pub environment: ServiceEnvironment,
    #[serde(default)]
    pub retained_view: Option<crate::RetainedProcessView>,
}

impl ProcessSupervisorManifest {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        launch_id: String,
        pairing_secret: String,
        recovery_secret: String,
        private_directory: PathBuf,
        bubblewrap: PathBuf,
        boundary: ProcessMountBoundary,
        command: ProcessInvocation,
        environment: ServiceEnvironment,
    ) -> Result<Self, ProcessSupervisorError> {
        let manifest = Self {
            version: PROCESS_SUPERVISOR_VERSION,
            launch_id,
            pairing_secret,
            recovery_secret,
            private_directory,
            bubblewrap,
            boundary,
            command,
            environment,
            retained_view: None,
        };
        validate_manifest(&manifest)?;
        Ok(manifest)
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.private_directory.join(PROCESS_SUPERVISOR_MANIFEST)
    }

    pub fn socket_path(&self) -> PathBuf {
        self.private_directory.join(PROCESS_SUPERVISOR_SOCKET)
    }

    fn checkpoint_path(&self) -> PathBuf {
        self.private_directory.join(PROCESS_SUPERVISOR_CHECKPOINT)
    }

    /// Create the immutable manifest without replacing any existing launch path.
    pub fn write_new(&self) -> Result<PathBuf, ProcessSupervisorError> {
        validate_private_directory(&self.private_directory)?;
        for path in [
            self.manifest_path(),
            self.socket_path(),
            self.checkpoint_path(),
        ] {
            if std::fs::symlink_metadata(&path).is_ok() {
                return Err(ProcessSupervisorError::PathCollision(path));
            }
        }
        let path = self.manifest_path();
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        serde_json::to_writer(&mut file, self)?;
        file.sync_all()?;
        Ok(path)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProcessSupervisorCommand {
    Pair,
    Recover,
    Inspect,
    Prepare,
    Pin,
    Release,
    Stop,
    Finalize,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessSupervisorRequest {
    pub version: u32,
    pub launch_id: String,
    pub command: ProcessSupervisorCommand,
    #[serde(default)]
    pub credential: Option<String>,
    #[serde(default)]
    pub deadline_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessSupervisorObservation {
    Reserved,
    NotSpawned,
    Blocked,
    Pinned,
    Released,
    ReleaseUnconfirmed,
    Stopping,
    LaunchFailed,
    ProcessStopped,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessSupervisorResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace_view: Option<crate::NamespaceEntry>,
    pub version: u32,
    pub launch_id: Option<String>,
    pub observation: ProcessSupervisorObservation,
    #[serde(default)]
    pub operation_pending: bool,
    pub error: Option<String>,
}

/// Noncloneable client bound to one immutable launch identity. Each operation
/// reconnects to the same helper; timeouts never manufacture a replacement.
pub struct ProcessSupervisorClient {
    transport: SupervisorTransport,
}

/// Recovery can observe, stop, and finalize but can never prepare or release.
pub struct ProcessSupervisorRecovery {
    transport: SupervisorTransport,
}

struct SupervisorTransport {
    socket_path: PathBuf,
    launch_id: String,
    credential: String,
}

impl ProcessSupervisorClient {
    pub fn pair(
        socket_path: PathBuf,
        launch_id: String,
        pairing_secret: String,
        deadline: Duration,
    ) -> Result<(Self, ProcessSupervisorObservation), ProcessSupervisorError> {
        validate_absolute(&socket_path)?;
        if launch_id.is_empty() || launch_id.len() > 256 || pairing_secret.len() < 32 {
            return Err(ProcessSupervisorError::LaunchIdentity);
        }
        let client = Self {
            transport: SupervisorTransport {
                socket_path,
                launch_id,
                credential: pairing_secret,
            },
        };
        let observation = client.execute(ProcessSupervisorCommand::Pair, deadline)?;
        Ok((client, observation))
    }

    pub fn observe(
        &self,
        deadline: Duration,
    ) -> Result<ProcessSupervisorObservation, ProcessSupervisorError> {
        self.execute(ProcessSupervisorCommand::Inspect, deadline)
    }

    pub fn prepare(
        &mut self,
        deadline: Duration,
    ) -> Result<ProcessSupervisorObservation, ProcessSupervisorError> {
        self.execute_until(ProcessSupervisorCommand::Prepare, deadline, |state| {
            matches!(
                state,
                ProcessSupervisorObservation::Blocked | ProcessSupervisorObservation::LaunchFailed
            )
        })
    }

    pub fn pin(
        &mut self,
        deadline: Duration,
    ) -> Result<ProcessSupervisorObservation, ProcessSupervisorError> {
        self.execute_until(ProcessSupervisorCommand::Pin, deadline, |state| {
            state == ProcessSupervisorObservation::Pinned
        })
    }

    pub fn workspace_view(
        &self,
        deadline: Duration,
    ) -> Result<crate::MountNamespace, ProcessSupervisorError> {
        let reply = self.request(ProcessSupervisorCommand::Inspect, deadline)?;
        if let Some(error) = reply.error {
            return Err(ProcessSupervisorError::Remote(error));
        }
        if reply.operation_pending || reply.observation != ProcessSupervisorObservation::Pinned {
            return Err(ProcessSupervisorError::Remote(
                "workspace acquisition requires pinned init".into(),
            ));
        }
        Ok(reply
            .workspace_view
            .ok_or_else(|| ProcessSupervisorError::Remote("pinned workspace unavailable".into()))?
            .acquire()?)
    }

    pub fn release(
        &mut self,
        deadline: Duration,
    ) -> Result<ProcessSupervisorObservation, ProcessSupervisorError> {
        self.execute_until(ProcessSupervisorCommand::Release, deadline, |state| {
            matches!(
                state,
                ProcessSupervisorObservation::Released
                    | ProcessSupervisorObservation::ReleaseUnconfirmed
            )
        })
    }

    pub fn stop(
        &mut self,
        deadline: Duration,
    ) -> Result<ProcessSupervisorObservation, ProcessSupervisorError> {
        self.execute_until(ProcessSupervisorCommand::Stop, deadline, |state| {
            matches!(
                state,
                ProcessSupervisorObservation::ProcessStopped
                    | ProcessSupervisorObservation::NotSpawned
            )
        })
    }

    pub fn finalize(
        self,
        deadline: Duration,
    ) -> Result<ProcessSupervisorObservation, ProcessSupervisorError> {
        self.execute(ProcessSupervisorCommand::Finalize, deadline)
    }

    fn execute(
        &self,
        command: ProcessSupervisorCommand,
        deadline: Duration,
    ) -> Result<ProcessSupervisorObservation, ProcessSupervisorError> {
        let response = self.request(command, deadline)?;
        if let Some(error) = response.error {
            return Err(ProcessSupervisorError::Remote(error));
        }
        Ok(response.observation)
    }

    fn execute_until(
        &self,
        command: ProcessSupervisorCommand,
        deadline: Duration,
        complete: impl Fn(ProcessSupervisorObservation) -> bool,
    ) -> Result<ProcessSupervisorObservation, ProcessSupervisorError> {
        let started = Instant::now();
        let mut response = self.request(command, deadline)?;
        loop {
            if let Some(error) = response.error {
                return Err(ProcessSupervisorError::Remote(error));
            }
            if !response.operation_pending && complete(response.observation) {
                return Ok(response.observation);
            }
            let remaining = deadline
                .checked_sub(started.elapsed())
                .ok_or(ProcessSupervisorError::Deadline)?;
            #[allow(
                clippy::disallowed_methods,
                reason = "synchronous supervisor client poll loop, not async"
            )]
            std::thread::sleep(Duration::from_millis(10).min(remaining));
            response = self.request(ProcessSupervisorCommand::Inspect, remaining)?;
            if !response.operation_pending
                && !complete(response.observation)
                && response.error.is_none()
            {
                return Err(ProcessSupervisorError::Remote(format!(
                    "scope operation stopped in {:?}",
                    response.observation
                )));
            }
        }
    }

    fn request(
        &self,
        command: ProcessSupervisorCommand,
        deadline: Duration,
    ) -> Result<ProcessSupervisorResponse, ProcessSupervisorError> {
        self.transport.request(command, deadline)
    }
}

impl ProcessSupervisorRecovery {
    pub fn recover(
        socket_path: PathBuf,
        launch_id: String,
        recovery_secret: String,
        deadline: Duration,
    ) -> Result<(Self, ProcessSupervisorObservation), ProcessSupervisorError> {
        validate_absolute(&socket_path)?;
        if launch_id.is_empty() || launch_id.len() > 256 || recovery_secret.len() < 32 {
            return Err(ProcessSupervisorError::LaunchIdentity);
        }
        let client = Self {
            transport: SupervisorTransport {
                socket_path,
                launch_id,
                credential: recovery_secret,
            },
        };
        let observation = client.execute(ProcessSupervisorCommand::Recover, deadline)?;
        Ok((client, observation))
    }

    pub fn observe(
        &self,
        deadline: Duration,
    ) -> Result<ProcessSupervisorObservation, ProcessSupervisorError> {
        self.execute(ProcessSupervisorCommand::Inspect, deadline)
    }

    pub fn stop(
        &mut self,
        deadline: Duration,
    ) -> Result<ProcessSupervisorObservation, ProcessSupervisorError> {
        self.execute_until(ProcessSupervisorCommand::Stop, deadline)
    }

    pub fn finalize(
        self,
        deadline: Duration,
    ) -> Result<ProcessSupervisorObservation, ProcessSupervisorError> {
        self.execute(ProcessSupervisorCommand::Finalize, deadline)
    }

    fn execute(
        &self,
        command: ProcessSupervisorCommand,
        deadline: Duration,
    ) -> Result<ProcessSupervisorObservation, ProcessSupervisorError> {
        let response = self.request(command, deadline)?;
        if let Some(error) = response.error {
            return Err(ProcessSupervisorError::Remote(error));
        }
        Ok(response.observation)
    }

    fn execute_until(
        &self,
        command: ProcessSupervisorCommand,
        deadline: Duration,
    ) -> Result<ProcessSupervisorObservation, ProcessSupervisorError> {
        let started = Instant::now();
        let mut response = self.request(command, deadline)?;
        loop {
            if let Some(error) = response.error {
                return Err(ProcessSupervisorError::Remote(error));
            }
            if !response.operation_pending
                && matches!(
                    response.observation,
                    ProcessSupervisorObservation::ProcessStopped
                        | ProcessSupervisorObservation::NotSpawned
                )
            {
                return Ok(response.observation);
            }
            let remaining = deadline
                .checked_sub(started.elapsed())
                .ok_or(ProcessSupervisorError::Deadline)?;
            #[allow(
                clippy::disallowed_methods,
                reason = "synchronous supervisor client poll loop, not async"
            )]
            std::thread::sleep(Duration::from_millis(10).min(remaining));
            response = self.request(ProcessSupervisorCommand::Inspect, remaining)?;
        }
    }

    fn request(
        &self,
        command: ProcessSupervisorCommand,
        deadline: Duration,
    ) -> Result<ProcessSupervisorResponse, ProcessSupervisorError> {
        self.transport.request(command, deadline)
    }
}

impl SupervisorTransport {
    fn request(
        &self,
        command: ProcessSupervisorCommand,
        deadline: Duration,
    ) -> Result<ProcessSupervisorResponse, ProcessSupervisorError> {
        if deadline.is_zero() {
            return Err(ProcessSupervisorError::Deadline);
        }
        let deadline = deadline.min(Duration::from_millis(MAX_OPERATION_MS));
        let mut stream = UnixStream::connect(&self.socket_path)?;
        stream.set_read_timeout(Some(deadline))?;
        stream.set_write_timeout(Some(deadline))?;
        write_frame(
            &mut stream,
            &ProcessSupervisorRequest {
                version: PROCESS_SUPERVISOR_VERSION,
                launch_id: self.launch_id.clone(),
                command,
                credential: Some(self.credential.clone()),
                deadline_ms: Some(deadline.as_millis().try_into().unwrap_or(MAX_OPERATION_MS)),
            },
        )?;
        let response: ProcessSupervisorResponse = read_frame(&mut stream)?;
        if response.version != PROCESS_SUPERVISOR_VERSION {
            return Err(ProcessSupervisorError::Version(response.version));
        }
        match response.launch_id.as_deref() {
            None => return Err(ProcessSupervisorError::Unauthorized),
            Some(identity) if identity != self.launch_id => {
                return Err(ProcessSupervisorError::LaunchIdentity)
            }
            Some(_) => {}
        }
        Ok(response)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProcessSupervisorError {
    #[error("process supervisor path must be absolute: {0}")]
    RelativePath(PathBuf),
    #[error("process supervisor private directory is unsafe")]
    UnsafePrivateDirectory,
    #[error("process supervisor manifest is not a private regular file")]
    UnsafeManifest,
    #[error("unsupported process supervisor version {0}")]
    Version(u32),
    #[error("invalid process supervisor launch identity")]
    LaunchIdentity,
    #[error("process supervisor path already exists: {0}")]
    PathCollision(PathBuf),
    #[error("process supervisor request is unauthorized")]
    Unauthorized,
    #[error("process supervisor frame exceeds its bound")]
    FrameTooLarge,
    #[error("process supervisor already has a scope operation pending")]
    Busy,
    #[error("process supervisor operation deadline elapsed")]
    Deadline,
    #[error("process supervisor I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("process supervisor JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("process supervisor rejected operation: {0}")]
    Remote(String),
    #[error(transparent)]
    Scope(#[from] ServiceScopeError),
}

enum OwnedLaunch {
    Reserved(Option<crate::LaunchReservation>),
    NotSpawned,
    Scope(ScopeCapability),
}

#[derive(Clone)]
struct WorkerObservation {
    workspace_view: Option<crate::MountNamespace>,
    state: ProcessSupervisorObservation,
    pending: bool,
    error: Option<String>,
}

enum ScopeCommand {
    Prepare,
    Pin(Duration),
    Release,
    Stop(Duration),
    PrimaryLost,
}

struct ScopeWorker {
    commands: std::sync::mpsc::SyncSender<ScopeCommand>,
    observation: std::sync::Arc<std::sync::Mutex<WorkerObservation>>,
}

impl ScopeWorker {
    fn submit(&self, command: ScopeCommand) -> Result<(), ProcessSupervisorError> {
        let mut observed = self
            .observation
            .lock()
            .map_err(|_| ProcessSupervisorError::Unauthorized)?;
        if observed.pending {
            return Err(ProcessSupervisorError::Busy);
        }
        observed.pending = true;
        observed.error = None;
        self.commands.try_send(command).map_err(|_| {
            observed.pending = false;
            ProcessSupervisorError::Busy
        })
    }

    fn observed(&self) -> Result<WorkerObservation, ProcessSupervisorError> {
        self.observation
            .lock()
            .map(|value| value.clone())
            .map_err(|_| ProcessSupervisorError::Unauthorized)
    }
}

/// Run the single-launch helper. The manifest is read once and never used to
/// rediscover a process. Socket collision fails closed without unlinking.
pub fn run_process_supervisor(path: &Path) -> Result<(), ProcessSupervisorError> {
    validate_absolute(path)?;
    let parent = path
        .parent()
        .ok_or(ProcessSupervisorError::UnsafePrivateDirectory)?;
    validate_private_directory(parent)?;
    if path.file_name().and_then(|name| name.to_str()) != Some(PROCESS_SUPERVISOR_MANIFEST) {
        return Err(ProcessSupervisorError::UnsafeManifest);
    }
    let fd = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| ProcessSupervisorError::Io(error.into()))?;
    let file = std::fs::File::from(fd);
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(ProcessSupervisorError::UnsafeManifest);
    }
    let mut manifest: ProcessSupervisorManifest = serde_json::from_reader(file)?;
    validate_manifest(&manifest)?;
    if manifest.private_directory != parent {
        return Err(ProcessSupervisorError::UnsafePrivateDirectory);
    }
    let socket_path = manifest.socket_path();
    let checkpoint_path = manifest.checkpoint_path();
    for path in [&socket_path, &checkpoint_path] {
        if std::fs::symlink_metadata(path).is_ok() {
            return Err(ProcessSupervisorError::PathCollision(path.clone()));
        }
    }
    let listener = UnixListener::bind(&socket_path)?;
    listener.set_nonblocking(true)?;
    let reservation = manifest
        .boundary
        .reserve_service_scope(manifest.bubblewrap.clone(), manifest.command.clone())?;
    let reservation = match manifest.retained_view.take() {
        Some(view) => reservation.in_view(view)?,
        None => reservation,
    };
    let observed = std::sync::Arc::new(std::sync::Mutex::new(WorkerObservation {
        workspace_view: None,
        state: ProcessSupervisorObservation::Reserved,
        pending: false,
        error: None,
    }));
    let (commands, receive) = std::sync::mpsc::sync_channel(8);
    let worker_observed = observed.clone();
    let environment = manifest.environment.clone();
    let worker = std::thread::spawn(move || {
        scope_worker(reservation, environment, receive, worker_observed)
    });
    let scope = ScopeWorker {
        commands,
        observation: observed,
    };
    publish_checkpoint(&manifest, &scope.observed()?)?;
    let result = serve(&listener, &manifest, &scope);
    drop(scope);
    // best-effort: worker's panic payload isn't actionable here; `result`
    // from serve() is what's reported.
    worker.join().ok();
    result
}

fn serve(
    listener: &UnixListener,
    manifest: &ProcessSupervisorManifest,
    scope: &ScopeWorker,
) -> Result<(), ProcessSupervisorError> {
    let mut paired = false;
    let mut last_primary = None;
    loop {
        let (mut stream, _) = match listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                submit_expired_primary_loss(scope, &mut last_primary);
                #[allow(
                    clippy::disallowed_methods,
                    reason = "synchronous accept-loop poll, not async"
                )]
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        stream.set_read_timeout(Some(UNAUTHENTICATED_READ_TIMEOUT))?;
        stream.set_write_timeout(Some(UNAUTHENTICATED_READ_TIMEOUT))?;
        let request = match read_frame::<ProcessSupervisorRequest>(&mut stream) {
            Ok(request) => request,
            Err(error) => {
                let response = unauthorized_response(Some(error.to_string()));
                // best-effort: the client may already have disconnected.
                write_frame(&mut stream, &response).ok();
                continue;
            }
        };
        if request.version != PROCESS_SUPERVISOR_VERSION || request.launch_id != manifest.launch_id
        {
            let response = unauthorized_response(None);
            // best-effort: the client may already have disconnected.
            write_frame(&mut stream, &response).ok();
            continue;
        }
        enum Authority {
            Primary,
            Recovery,
        }
        let authority = match request.command {
            ProcessSupervisorCommand::Pair
                if !paired && request.credential.as_ref() == Some(&manifest.pairing_secret) =>
            {
                paired = true;
                last_primary = Some(Instant::now());
                Authority::Primary
            }
            ProcessSupervisorCommand::Pair => {
                // best-effort: the client may already have disconnected.
                write_frame(&mut stream, &unauthorized_response(None)).ok();
                continue;
            }
            ProcessSupervisorCommand::Recover
                if request.credential.as_ref() == Some(&manifest.recovery_secret) =>
            {
                Authority::Recovery
            }
            _ if paired && request.credential.as_ref() == Some(&manifest.pairing_secret) => {
                last_primary = Some(Instant::now());
                Authority::Primary
            }
            _ if request.credential.as_ref() == Some(&manifest.recovery_secret) => {
                Authority::Recovery
            }
            _ => {
                // best-effort: the client may already have disconnected.
                write_frame(&mut stream, &unauthorized_response(None)).ok();
                continue;
            }
        };
        let duration =
            Duration::from_millis(request.deadline_ms.unwrap_or(30_000).min(MAX_OPERATION_MS));
        let mut finalize = false;
        let result: Result<(), ProcessSupervisorError> = match request.command {
            ProcessSupervisorCommand::Pair | ProcessSupervisorCommand::Recover => Ok(()),
            ProcessSupervisorCommand::Inspect => Ok(()),
            ProcessSupervisorCommand::Prepare if matches!(authority, Authority::Primary) => {
                scope.submit(ScopeCommand::Prepare)
            }
            ProcessSupervisorCommand::Pin if matches!(authority, Authority::Primary) => {
                scope.submit(ScopeCommand::Pin(duration))
            }
            ProcessSupervisorCommand::Release if matches!(authority, Authority::Primary) => {
                scope.submit(ScopeCommand::Release)
            }
            ProcessSupervisorCommand::Stop => scope.submit(ScopeCommand::Stop(duration)),
            ProcessSupervisorCommand::Finalize => {
                let observed = scope.observed()?;
                if !observed.pending
                    && matches!(
                        observed.state,
                        ProcessSupervisorObservation::ProcessStopped
                            | ProcessSupervisorObservation::NotSpawned
                    )
                {
                    finalize = true;
                    Ok(())
                } else {
                    Err(ProcessSupervisorError::Remote(
                        "finalize requires exact terminal observation".into(),
                    ))
                }
            }
            ProcessSupervisorCommand::Prepare
            | ProcessSupervisorCommand::Pin
            | ProcessSupervisorCommand::Release => Err(ProcessSupervisorError::Unauthorized),
        };
        let observed = scope.observed()?;
        let mut error = result
            .err()
            .map(|error| error.to_string())
            .or(observed.error.clone());
        if let Err(checkpoint_error) = publish_checkpoint(manifest, &observed) {
            error.get_or_insert_with(|| checkpoint_error.to_string());
        }
        let mut reply = response(manifest, &observed, error);
        if matches!(authority, Authority::Primary)
            && !observed.pending
            && observed.state == ProcessSupervisorObservation::Pinned
        {
            if let Some(view) = &observed.workspace_view {
                match view.entry() {
                    Ok(entry) => reply.workspace_view = Some(entry),
                    Err(error) => reply.error = Some(error.to_string()),
                }
            }
        }
        let response_written = write_frame(&mut stream, &reply).is_ok();
        // Lost operation replies leave the same helper and scope addressable.
        // Finalization exits only after its response was actually written.
        if finalize && response_written {
            return Ok(());
        }
    }
}

fn submit_expired_primary_loss(scope: &ScopeWorker, last_primary: &mut Option<Instant>) {
    if last_primary.is_some_and(|seen| seen.elapsed() >= PRIMARY_LEASE) {
        // A scope operation may still be finishing when the primary lease
        // expires. Keep retrying until the worker accepts the loss command;
        // clearing the lease first could strand a pinned, blocked payload.
        if scope.submit(ScopeCommand::PrimaryLost).is_ok() {
            *last_primary = None;
        }
    }
}

fn scope_worker(
    reservation: crate::LaunchReservation,
    environment: ServiceEnvironment,
    commands: std::sync::mpsc::Receiver<ScopeCommand>,
    observed: std::sync::Arc<std::sync::Mutex<WorkerObservation>>,
) {
    let mut launch = OwnedLaunch::Reserved(Some(reservation));
    let mut stopped = None;
    loop {
        let command = match commands.recv_timeout(Duration::from_millis(100)) {
            Ok(command) => Some(command),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        };
        let mut error = match command {
            Some(ScopeCommand::Prepare) => prepare(
                &mut launch,
                &environment,
                Instant::now() + Duration::from_secs(30),
            ),
            Some(ScopeCommand::Pin(duration)) => pin(&mut launch, Instant::now() + duration),
            Some(ScopeCommand::Release) => release(&mut launch),
            Some(ScopeCommand::Stop(duration)) => {
                stop(&mut launch, &mut stopped, Instant::now() + duration)
            }
            Some(ScopeCommand::PrimaryLost) => {
                cleanup_after_primary_loss(
                    &mut launch,
                    &mut stopped,
                    Instant::now() + Duration::from_secs(10),
                );
                Ok(())
            }
            None => {
                refresh_terminal(&mut launch, &mut stopped);
                if let Ok(mut state) = observed.lock() {
                    state.state = observation(&launch, &stopped);
                }
                continue;
            }
        }
        .err()
        .map(|error| error.to_string());
        if let Ok(mut state) = observed.lock() {
            state.state = observation(&launch, &stopped);
            state.workspace_view = match &launch {
                OwnedLaunch::Scope(scope)
                    if state.state == ProcessSupervisorObservation::Pinned =>
                {
                    match scope.workspace_view() {
                        Ok(view) => Some(view),
                        Err(failure) => {
                            error.get_or_insert_with(|| failure.to_string());
                            None
                        }
                    }
                }
                _ => None,
            };
            state.pending = false;
            state.error = error;
        }
    }
}

fn prepare(
    launch: &mut OwnedLaunch,
    environment: &ServiceEnvironment,
    _deadline: Instant,
) -> Result<(), ServiceScopeError> {
    if matches!(launch, OwnedLaunch::Scope(_)) {
        return Ok(());
    }
    let reservation = match launch {
        OwnedLaunch::Reserved(reservation) => {
            reservation.take().ok_or(ServiceScopeError::WrongPhase)?
        }
        OwnedLaunch::NotSpawned => return Err(ServiceScopeError::WrongPhase),
        OwnedLaunch::Scope(_) => return Ok(()),
    };
    let scope =
        match reservation.spawn_with_stdio(environment.clone(), ServiceStdio::InheritedTerminal) {
            Ok(scope) => scope,
            Err(error) => {
                *launch = OwnedLaunch::NotSpawned;
                return Err(error);
            }
        };
    *launch = OwnedLaunch::Scope(scope);
    Ok(())
}

fn pin(launch: &mut OwnedLaunch, deadline: Instant) -> Result<(), ServiceScopeError> {
    match launch {
        OwnedLaunch::Scope(scope) => scope.pin_init(deadline),
        OwnedLaunch::Reserved(_) | OwnedLaunch::NotSpawned => Err(ServiceScopeError::WrongPhase),
    }
}

fn release(launch: &mut OwnedLaunch) -> Result<(), ServiceScopeError> {
    match launch {
        OwnedLaunch::Scope(scope) => match scope.observation()? {
            ScopeObservation::Released => Ok(()),
            _ => scope.release_command().map(|_| ()),
        },
        OwnedLaunch::Reserved(_) | OwnedLaunch::NotSpawned => Err(ServiceScopeError::WrongPhase),
    }
}

fn stop(
    launch: &mut OwnedLaunch,
    stopped: &mut Option<ServiceScopeCleanup>,
    deadline: Instant,
) -> Result<(), ServiceScopeError> {
    if stopped.is_some() {
        return Ok(());
    }
    match launch {
        OwnedLaunch::Scope(scope) => {
            *stopped = Some(scope.terminate_and_wait(deadline)?);
            Ok(())
        }
        OwnedLaunch::Reserved(reservation) => {
            reservation.take();
            *launch = OwnedLaunch::NotSpawned;
            Ok(())
        }
        OwnedLaunch::NotSpawned => Ok(()),
    }
}

fn observation(
    launch: &OwnedLaunch,
    stopped: &Option<ServiceScopeCleanup>,
) -> ProcessSupervisorObservation {
    if stopped.is_some() {
        return ProcessSupervisorObservation::ProcessStopped;
    }
    match launch {
        OwnedLaunch::Reserved(Some(_)) => ProcessSupervisorObservation::Reserved,
        OwnedLaunch::Reserved(None) => ProcessSupervisorObservation::LaunchFailed,
        OwnedLaunch::NotSpawned => ProcessSupervisorObservation::NotSpawned,
        OwnedLaunch::Scope(scope) => match scope.observation() {
            Ok(ScopeObservation::Blocked) => ProcessSupervisorObservation::Blocked,
            Ok(ScopeObservation::Pinned) => ProcessSupervisorObservation::Pinned,
            Ok(ScopeObservation::Released) => ProcessSupervisorObservation::Released,
            Ok(ScopeObservation::ReleaseUnconfirmed) => {
                ProcessSupervisorObservation::ReleaseUnconfirmed
            }
            Ok(ScopeObservation::Stopping) => ProcessSupervisorObservation::Stopping,
            Ok(ScopeObservation::ProcessStopped(_)) => ProcessSupervisorObservation::ProcessStopped,
            Err(_) => ProcessSupervisorObservation::Stopping,
        },
    }
}

fn read_frame<T: serde::de::DeserializeOwned>(
    stream: impl Read,
) -> Result<T, ProcessSupervisorError> {
    let mut bytes = Vec::new();
    BufReader::new(stream)
        .take((MAX_SUPERVISOR_FRAME_BYTES + 1) as u64)
        .read_until(b'\n', &mut bytes)?;
    if bytes.len() > MAX_SUPERVISOR_FRAME_BYTES {
        return Err(ProcessSupervisorError::FrameTooLarge);
    }
    if bytes.last() != Some(&b'\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "unterminated supervisor frame",
        )
        .into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}

/// Bound serialization before publishing any bytes to the socket, including
/// one reserved byte for the framing newline.
fn write_frame<T: Serialize>(
    mut stream: impl Write,
    value: &T,
) -> Result<(), ProcessSupervisorError> {
    struct FrameBuffer {
        bytes: Vec<u8>,
        exceeded: bool,
    }
    impl Write for FrameBuffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_SUPERVISOR_FRAME_BYTES - 1 - self.bytes.len() {
                self.exceeded = true;
                return Err(std::io::Error::other("supervisor frame exceeds limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut frame = FrameBuffer {
        bytes: Vec::new(),
        exceeded: false,
    };
    let result = serde_json::to_writer(&mut frame, value);
    if frame.exceeded {
        return Err(ProcessSupervisorError::FrameTooLarge);
    }
    result?;
    frame.bytes.push(b'\n');
    stream.write_all(&frame.bytes)?;
    Ok(())
}

fn response(
    manifest: &ProcessSupervisorManifest,
    observation: &WorkerObservation,
    error: Option<String>,
) -> ProcessSupervisorResponse {
    ProcessSupervisorResponse {
        workspace_view: None,
        version: PROCESS_SUPERVISOR_VERSION,
        launch_id: Some(manifest.launch_id.clone()),
        observation: observation.state,
        operation_pending: observation.pending,
        error,
    }
}

fn unauthorized_response(error: Option<String>) -> ProcessSupervisorResponse {
    ProcessSupervisorResponse {
        workspace_view: None,
        version: PROCESS_SUPERVISOR_VERSION,
        launch_id: None,
        observation: ProcessSupervisorObservation::Reserved,
        operation_pending: false,
        error: Some(error.unwrap_or_else(|| "unauthorized".into())),
    }
}

fn publish_checkpoint(
    manifest: &ProcessSupervisorManifest,
    observation: &WorkerObservation,
) -> Result<(), ProcessSupervisorError> {
    #[derive(Serialize)]
    struct Checkpoint<'a> {
        version: u32,
        launch_id: &'a str,
        observation: ProcessSupervisorObservation,
        operation_pending: bool,
        error: Option<&'a str>,
    }
    let bytes = serde_json::to_vec(&Checkpoint {
        version: PROCESS_SUPERVISOR_VERSION,
        launch_id: &manifest.launch_id,
        observation: observation.state,
        operation_pending: observation.pending,
        error: observation.error.as_deref(),
    })?;
    let path = manifest.checkpoint_path();
    if let Ok(metadata) = std::fs::symlink_metadata(&path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(ProcessSupervisorError::PathCollision(path));
        }
    }
    tidepool_atomic_write::write_durable(&path, &bytes).map_err(std::io::Error::from)?;
    Ok(())
}

fn validate_manifest(manifest: &ProcessSupervisorManifest) -> Result<(), ProcessSupervisorError> {
    if manifest.version != PROCESS_SUPERVISOR_VERSION {
        return Err(ProcessSupervisorError::Version(manifest.version));
    }
    if manifest.launch_id.is_empty() || manifest.launch_id.len() > 256 {
        return Err(ProcessSupervisorError::LaunchIdentity);
    }
    if manifest.pairing_secret.len() < 32
        || manifest.recovery_secret.len() < 32
        || manifest.pairing_secret == manifest.recovery_secret
    {
        return Err(ProcessSupervisorError::LaunchIdentity);
    }
    validate_absolute(&manifest.private_directory)?;
    validate_absolute(&manifest.bubblewrap)?;
    Ok(())
}

fn validate_private_directory(path: &Path) -> Result<(), ProcessSupervisorError> {
    validate_absolute(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || std::fs::canonicalize(path)? != path
    {
        return Err(ProcessSupervisorError::UnsafePrivateDirectory);
    }
    Ok(())
}

fn cleanup_after_primary_loss(
    launch: &mut OwnedLaunch,
    stopped: &mut Option<ServiceScopeCleanup>,
    deadline: Instant,
) {
    match launch {
        OwnedLaunch::Reserved(reservation) => {
            reservation.take();
            *launch = OwnedLaunch::NotSpawned;
        }
        OwnedLaunch::Scope(scope) => match scope.observation() {
            Ok(ScopeObservation::Blocked) => {
                // best-effort: this is a primary-loss cleanup path with no
                // caller to report to; `stopped` carries what succeeded.
                scope.pin_init(deadline).ok();
                stop(launch, stopped, deadline).ok();
            }
            Ok(ScopeObservation::Pinned) => {
                stop(launch, stopped, deadline).ok();
            }
            Ok(
                ScopeObservation::Released
                | ScopeObservation::ReleaseUnconfirmed
                | ScopeObservation::Stopping
                | ScopeObservation::ProcessStopped(_),
            )
            | Err(_) => {}
        },
        OwnedLaunch::NotSpawned => {}
    }
}

fn refresh_terminal(launch: &mut OwnedLaunch, stopped: &mut Option<ServiceScopeCleanup>) {
    let OwnedLaunch::Scope(scope) = launch else {
        return;
    };
    if let Ok(Some(receipt)) = scope.refresh_terminal(Instant::now() + Duration::from_millis(50)) {
        *stopped = Some(receipt);
    }
}

fn validate_absolute(path: &Path) -> Result<(), ProcessSupervisorError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(ProcessSupervisorError::RelativePath(path.to_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply() -> ProcessSupervisorResponse {
        ProcessSupervisorResponse {
            workspace_view: None,
            version: PROCESS_SUPERVISOR_VERSION,
            launch_id: Some("launch".into()),
            observation: ProcessSupervisorObservation::Reserved,
            operation_pending: false,
            error: None,
        }
    }

    fn fake_pair(
        bytes: Vec<u8>,
        recovery: bool,
    ) -> Result<ProcessSupervisorObservation, ProcessSupervisorError> {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("peer.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        std::thread::scope(|scope| {
            let (finished, hold) = std::sync::mpsc::channel();
            let peer = scope.spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let _: ProcessSupervisorRequest = read_frame(&mut stream).unwrap();
                // Small fragments exercise the public client's incremental decoder.
                for chunk in bytes.chunks(17) {
                    if stream.write_all(chunk).is_err() {
                        break;
                    }
                }
                if bytes.last() != Some(&b'\n') {
                    stream.shutdown(std::net::Shutdown::Write).unwrap();
                }
                hold.recv_timeout(Duration::from_secs(3)).unwrap();
            });
            let result = if recovery {
                ProcessSupervisorRecovery::recover(
                    socket,
                    "launch".into(),
                    "r".repeat(64),
                    Duration::from_secs(1),
                )
                .map(|(_, state)| state)
            } else {
                ProcessSupervisorClient::pair(
                    socket,
                    "launch".into(),
                    "p".repeat(64),
                    Duration::from_secs(1),
                )
                .map(|(_, state)| state)
            };
            finished.send(()).unwrap();
            peer.join().unwrap();
            result
        })
    }

    #[test]
    fn public_clients_frame_and_authentication_contract() {
        for recovery in [false, true] {
            let mut bytes = serde_json::to_vec(&reply()).unwrap();
            bytes.push(b'\n');
            assert_eq!(
                fake_pair(bytes, recovery).unwrap(),
                ProcessSupervisorObservation::Reserved
            );
            for (version, identity, expected) in
                [(99, None, 0), (2, None, 1), (2, Some("other"), 2)]
            {
                let mut response = reply();
                response.version = version;
                response.launch_id = identity.map(str::to_owned);
                let mut bytes = serde_json::to_vec(&response).unwrap();
                bytes.push(b'\n');
                let error = fake_pair(bytes, recovery).unwrap_err();
                assert!(matches!(
                    (expected, error),
                    (0, ProcessSupervisorError::Version(99))
                        | (1, ProcessSupervisorError::Unauthorized)
                        | (2, ProcessSupervisorError::LaunchIdentity)
                ));
            }
            let mut response = reply();
            response.error = Some("authenticated failure".into());
            response.operation_pending = true;
            let mut bytes = serde_json::to_vec(&response).unwrap();
            bytes.push(b'\n');
            assert!(
                matches!(fake_pair(bytes, recovery), Err(ProcessSupervisorError::Remote(message)) if message == "authenticated failure")
            );
            for bytes in [vec![], b"{}".to_vec(), b"{\n".to_vec(), b"\n".to_vec()] {
                let terminated = bytes.last() == Some(&b'\n');
                let error = fake_pair(bytes, recovery).unwrap_err();
                assert!(if terminated {
                    matches!(error, ProcessSupervisorError::Json(_))
                } else {
                    matches!(error, ProcessSupervisorError::Io(ref error) if error.kind() == std::io::ErrorKind::UnexpectedEof)
                });
            }
        }
    }

    #[test]
    fn frame_byte_boundaries_and_atomic_writer() {
        // JSON string quotes and the newline occupy three bytes, independently
        // of the transport's admission predicate. Include multibyte UTF-8.
        for size in [
            MAX_SUPERVISOR_FRAME_BYTES - 1,
            MAX_SUPERVISOR_FRAME_BYTES,
            MAX_SUPERVISOR_FRAME_BYTES + 1,
        ] {
            let value = format!("λ{}", "x".repeat(size - 5));
            let mut bytes = serde_json::to_vec(&value).unwrap();
            bytes.push(b'\n');
            assert_eq!(bytes.len(), size);
            let mut written = Vec::new();
            let result = write_frame(&mut written, &value);
            let read = read_frame::<String>(bytes.as_slice());
            if size <= MAX_SUPERVISOR_FRAME_BYTES {
                result.unwrap();
                assert_eq!(written, bytes);
                assert_eq!(read.unwrap(), value);
            } else {
                assert!(matches!(result, Err(ProcessSupervisorError::FrameTooLarge)));
                assert!(written.is_empty());
                assert!(matches!(read, Err(ProcessSupervisorError::FrameTooLarge)));
            }
        }
        for recovery in [false, true] {
            for size in [
                MAX_SUPERVISOR_FRAME_BYTES - 1,
                MAX_SUPERVISOR_FRAME_BYTES,
                MAX_SUPERVISOR_FRAME_BYTES + 1,
            ] {
                let response = reply();
                let base = serde_json::to_vec(&response).unwrap().len() + 1;
                // JSON permits trailing whitespace, preserving the response schema.
                let mut bytes = serde_json::to_vec(&response).unwrap();
                bytes.extend(std::iter::repeat_n(b' ', size - base));
                bytes.push(b'\n');
                let result = fake_pair(bytes, recovery);
                if size <= MAX_SUPERVISOR_FRAME_BYTES {
                    result.unwrap();
                } else {
                    assert!(matches!(result, Err(ProcessSupervisorError::FrameTooLarge)));
                }
            }
        }
    }

    #[test]
    fn serialization_error_is_not_a_size_error_and_emits_nothing() {
        struct Invalid;
        impl Serialize for Invalid {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("invalid value"))
            }
        }
        let mut bytes = Vec::new();
        assert!(matches!(
            write_frame(&mut bytes, &Invalid),
            Err(ProcessSupervisorError::Json(_))
        ));
        assert!(bytes.is_empty());
    }

    #[test]
    fn zero_deadline_precedes_socket_io() {
        let socket = PathBuf::from("/no/such/supervisor.sock");
        assert!(matches!(
            ProcessSupervisorClient::pair(
                socket.clone(),
                "launch".into(),
                "p".repeat(64),
                Duration::ZERO
            ),
            Err(ProcessSupervisorError::Deadline)
        ));
        assert!(matches!(
            ProcessSupervisorRecovery::recover(
                socket,
                "launch".into(),
                "r".repeat(64),
                Duration::ZERO
            ),
            Err(ProcessSupervisorError::Deadline)
        ));
    }

    proptest::proptest! {
        #[test]
        fn generated_fragmentation_preserves_json(
            value in ".{0,200}",
            fragment in 1usize..32,
        ) {
            struct Fragments<'a> { bytes: &'a [u8], fragment: usize }
            impl Read for Fragments<'_> {
                fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                    let count = output.len().min(self.fragment).min(self.bytes.len());
                    output[..count].copy_from_slice(&self.bytes[..count]);
                    self.bytes = &self.bytes[count..];
                    Ok(count)
                }
            }
            let mut encoded = serde_json::to_vec(&value).unwrap(); encoded.push(b'\n');
            let decoded: String = read_frame(Fragments { bytes: &encoded, fragment }).unwrap();
            proptest::prop_assert_eq!(decoded, value);
        }
    }

    #[test]
    fn public_clients_timeout_without_peer_eof() {
        for recovery in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let socket = directory.path().join("peer.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            std::thread::scope(|scope| {
                let (finished, hold) = std::sync::mpsc::channel();
                let peer = scope.spawn(move || {
                    let (mut stream, _) = listener.accept().unwrap();
                    let _: ProcessSupervisorRequest = read_frame(&mut stream).unwrap();
                    hold.recv_timeout(Duration::from_secs(3)).unwrap();
                });
                let result = if recovery {
                    ProcessSupervisorRecovery::recover(
                        socket,
                        "launch".into(),
                        "r".repeat(64),
                        Duration::from_millis(25),
                    )
                    .map(|(_, state)| state)
                } else {
                    ProcessSupervisorClient::pair(
                        socket,
                        "launch".into(),
                        "p".repeat(64),
                        Duration::from_millis(25),
                    )
                    .map(|(_, state)| state)
                };
                assert!(
                    matches!(result, Err(ProcessSupervisorError::Io(error)) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
                );
                finished.send(()).unwrap();
                peer.join().unwrap();
            });
        }
    }

    #[test]
    fn oversized_finalize_reply_keeps_helper_addressable() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = fixture(directory.path(), "true");
        let listener = UnixListener::bind(manifest.socket_path()).unwrap();
        let (commands, _receive) = std::sync::mpsc::sync_channel(8);
        let observed = std::sync::Arc::new(std::sync::Mutex::new(WorkerObservation {
            workspace_view: None,
            state: ProcessSupervisorObservation::NotSpawned,
            pending: false,
            error: Some("x".repeat(MAX_SUPERVISOR_FRAME_BYTES)),
        }));
        let worker = ScopeWorker {
            commands,
            observation: observed.clone(),
        };
        std::thread::scope(|scope| {
            let server = scope.spawn(|| serve(&listener, &manifest, &worker).unwrap());
            let mut stream = UnixStream::connect(manifest.socket_path()).unwrap();
            write_frame(
                &mut stream,
                &ProcessSupervisorRequest {
                    version: PROCESS_SUPERVISOR_VERSION,
                    launch_id: manifest.launch_id.clone(),
                    command: ProcessSupervisorCommand::Finalize,
                    credential: Some(manifest.recovery_secret.clone()),
                    deadline_ms: None,
                },
            )
            .unwrap();
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).unwrap();
            assert!(bytes.is_empty(), "oversized reply must publish no bytes");
            observed.lock().unwrap().error = None;
            let (recovery, state) = ProcessSupervisorRecovery::recover(
                manifest.socket_path(),
                manifest.launch_id.clone(),
                manifest.recovery_secret.clone(),
                Duration::from_secs(1),
            )
            .unwrap();
            assert_eq!(state, ProcessSupervisorObservation::NotSpawned);
            recovery.finalize(Duration::from_secs(1)).unwrap();
            server.join().unwrap();
        });
    }

    fn bwrap() -> PathBuf {
        std::env::var_os("SERVICE_SCOPE_BWRAP")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(
                    "/nix/store/dqzmpjz70l4lzg7lmc3x8wih74nh5bpc-bubblewrap-0.11.0/bin/bwrap",
                )
            })
    }

    fn fixture(directory: &Path, script: &str) -> ProcessSupervisorManifest {
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let boundary =
            ProcessMountBoundary::new(directory, [directory.to_owned()], [directory.to_owned()])
                .unwrap();
        ProcessSupervisorManifest::new(
            "launch-exact".into(),
            "p".repeat(64),
            "r".repeat(64),
            directory.to_owned(),
            bwrap(),
            boundary,
            ProcessInvocation {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), script.into()],
            },
            ServiceEnvironment::default(),
        )
        .unwrap()
    }

    fn wait_socket(path: &Path) {
        let limit = Instant::now() + Duration::from_secs(10);
        while !path.exists() {
            assert!(Instant::now() < limit, "supervisor socket startup");
            std::thread::yield_now();
        }
    }

    #[test]
    fn fixed_private_paths_reject_collisions() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = fixture(directory.path(), "true");
        std::fs::write(manifest.checkpoint_path(), b"foreign").unwrap();
        assert!(matches!(
            manifest.write_new(),
            Err(ProcessSupervisorError::PathCollision(_))
        ));
        std::fs::remove_file(manifest.checkpoint_path()).unwrap();
        assert_eq!(
            manifest.write_new().unwrap(),
            directory.path().join(PROCESS_SUPERVISOR_MANIFEST)
        );
        assert!(matches!(
            manifest.write_new(),
            Err(ProcessSupervisorError::PathCollision(_))
        ));
    }

    #[test]
    fn one_time_pair_and_recovery_has_no_release_surface() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = fixture(directory.path(), "echo started > started; exec sleep 30");
        let socket = manifest.socket_path();
        let path = manifest.write_new().unwrap();
        std::thread::scope(|scope| {
            let server = scope.spawn(|| run_process_supervisor(&path).unwrap());
            wait_socket(&socket);
            let (mut client, _) = ProcessSupervisorClient::pair(
                socket.clone(),
                manifest.launch_id.clone(),
                "p".repeat(64),
                Duration::from_secs(10),
            )
            .unwrap();
            assert!(ProcessSupervisorClient::pair(
                socket.clone(),
                manifest.launch_id.clone(),
                "p".repeat(64),
                Duration::from_secs(1),
            )
            .is_err());
            client.prepare(Duration::from_secs(10)).unwrap();
            client.pin(Duration::from_secs(10)).unwrap();
            let (mut recovery, observed) = ProcessSupervisorRecovery::recover(
                socket,
                manifest.launch_id.clone(),
                "r".repeat(64),
                Duration::from_secs(10),
            )
            .unwrap();
            assert_eq!(observed, ProcessSupervisorObservation::Pinned);
            for command in [
                ProcessSupervisorCommand::Prepare,
                ProcessSupervisorCommand::Pin,
                ProcessSupervisorCommand::Release,
            ] {
                let reply = recovery.request(command, Duration::from_secs(1)).unwrap();
                assert!(reply.error.is_some());
                assert!(!reply.operation_pending);
                assert_eq!(reply.observation, ProcessSupervisorObservation::Pinned);
            }
            assert!(!directory.path().join("started").exists());
            recovery.stop(Duration::from_secs(10)).unwrap();
            recovery.finalize(Duration::from_secs(10)).unwrap();
            drop(client);
            server.join().unwrap();
        });
    }

    #[test]
    fn owner_loss_before_release_stops_without_starting() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = fixture(directory.path(), "echo started > started; exec sleep 30");
        let socket = manifest.socket_path();
        let path = manifest.write_new().unwrap();
        std::thread::scope(|scope| {
            let server = scope.spawn(|| run_process_supervisor(&path).unwrap());
            wait_socket(&socket);
            let (mut client, _) = ProcessSupervisorClient::pair(
                socket.clone(),
                manifest.launch_id.clone(),
                "p".repeat(64),
                Duration::from_secs(10),
            )
            .unwrap();
            client.prepare(Duration::from_secs(10)).unwrap();
            client.pin(Duration::from_secs(10)).unwrap();
            drop(client);
            #[allow(
                clippy::disallowed_methods,
                reason = "sync test waits out the primary lease"
            )]
            std::thread::sleep(PRIMARY_LEASE + Duration::from_millis(200));
            let (recovery, observed) = ProcessSupervisorRecovery::recover(
                socket,
                manifest.launch_id.clone(),
                "r".repeat(64),
                Duration::from_secs(10),
            )
            .unwrap();
            assert_eq!(observed, ProcessSupervisorObservation::ProcessStopped);
            assert!(!directory.path().join("started").exists());
            recovery.finalize(Duration::from_secs(10)).unwrap();
            server.join().unwrap();
        });
    }

    #[test]
    fn owner_loss_after_release_preserves_exact_scope_for_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = fixture(directory.path(), "echo started > started; exec sleep 30");
        let socket = manifest.socket_path();
        let path = manifest.write_new().unwrap();
        std::thread::scope(|scope| {
            let server = scope.spawn(|| run_process_supervisor(&path).unwrap());
            wait_socket(&socket);
            let (mut client, _) = ProcessSupervisorClient::pair(
                socket.clone(),
                manifest.launch_id.clone(),
                "p".repeat(64),
                Duration::from_secs(10),
            )
            .unwrap();
            client.prepare(Duration::from_secs(10)).unwrap();
            client.pin(Duration::from_secs(10)).unwrap();
            client.release(Duration::from_secs(10)).unwrap();
            drop(client);
            #[allow(
                clippy::disallowed_methods,
                reason = "sync test waits out the primary lease"
            )]
            std::thread::sleep(PRIMARY_LEASE + Duration::from_millis(200));
            let (mut recovery, observed) = ProcessSupervisorRecovery::recover(
                socket,
                manifest.launch_id.clone(),
                "r".repeat(64),
                Duration::from_secs(10),
            )
            .unwrap();
            assert_eq!(observed, ProcessSupervisorObservation::Released);
            assert!(directory.path().join("started").exists());
            recovery.stop(Duration::from_secs(10)).unwrap();
            recovery.finalize(Duration::from_secs(10)).unwrap();
            server.join().unwrap();
        });
    }

    #[test]
    fn inspect_publishes_natural_terminal_observation() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = fixture(directory.path(), "exit 0");
        let socket = manifest.socket_path();
        let path = manifest.write_new().unwrap();
        std::thread::scope(|scope| {
            let server = scope.spawn(|| run_process_supervisor(&path).unwrap());
            wait_socket(&socket);
            let (mut client, _) = ProcessSupervisorClient::pair(
                socket,
                manifest.launch_id.clone(),
                "p".repeat(64),
                Duration::from_secs(10),
            )
            .unwrap();
            client.prepare(Duration::from_secs(10)).unwrap();
            client.pin(Duration::from_secs(10)).unwrap();
            client.release(Duration::from_secs(10)).unwrap();
            let limit = Instant::now() + Duration::from_secs(10);
            loop {
                if client.observe(Duration::from_secs(1)).unwrap()
                    == ProcessSupervisorObservation::ProcessStopped
                {
                    break;
                }
                assert!(Instant::now() < limit, "natural exit remained unobserved");
                #[allow(clippy::disallowed_methods, reason = "sync test poll loop")]
                std::thread::sleep(Duration::from_millis(20));
            }
            client.finalize(Duration::from_secs(10)).unwrap();
            server.join().unwrap();
        });
    }

    #[test]
    fn idle_unauthenticated_peer_does_not_block_owner() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = fixture(directory.path(), "true");
        let socket = manifest.socket_path();
        let path = manifest.write_new().unwrap();
        std::thread::scope(|scope| {
            let server = scope.spawn(|| run_process_supervisor(&path).unwrap());
            wait_socket(&socket);
            let _idle = UnixStream::connect(&socket).unwrap();
            let (mut client, observed) = ProcessSupervisorClient::pair(
                socket,
                manifest.launch_id.clone(),
                "p".repeat(64),
                Duration::from_secs(10),
            )
            .unwrap();
            assert_eq!(observed, ProcessSupervisorObservation::Reserved);
            client.stop(Duration::from_secs(10)).unwrap();
            client.finalize(Duration::from_secs(10)).unwrap();
            server.join().unwrap();
        });
    }

    #[test]
    fn primary_loss_is_retried_after_pending_scope_operation() {
        let (commands, receive) = std::sync::mpsc::sync_channel(1);
        let observation = std::sync::Arc::new(std::sync::Mutex::new(WorkerObservation {
            workspace_view: None,
            state: ProcessSupervisorObservation::Pinned,
            pending: true,
            error: None,
        }));
        let scope = ScopeWorker {
            commands,
            observation: observation.clone(),
        };
        let mut last_primary = Some(Instant::now() - PRIMARY_LEASE);
        submit_expired_primary_loss(&scope, &mut last_primary);
        assert!(last_primary.is_some(), "busy loss must remain pending");
        observation.lock().unwrap().pending = false;
        submit_expired_primary_loss(&scope, &mut last_primary);
        assert!(last_primary.is_none(), "accepted loss clears the lease");
        assert!(matches!(receive.try_recv(), Ok(ScopeCommand::PrimaryLost)));
    }
}
