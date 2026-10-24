//! Finite observations from the compiler service process owner. Serialized
//! observations never issue the private retirement capability.
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) const REPORT: &str = "owned-service.json";
const MAX_FRAME: usize = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ServiceReference {
    pub path: PathBuf,
    pub sha256: [u8; 32],
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceSettlement {
    NoRootsIssued,
    PrebootReleased,
    RetainedUnknown,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum AcquisitionObservation {
    NotConfigured,
    Acquired {
        attempt: ServiceReference,
        receipt: ServiceReference,
        profile: ServiceReference,
        publication: Vec<String>,
    },
    Refused {
        attempt: ServiceReference,
        primary: String,
        settlement: SourceSettlement,
        cleanup: Vec<String>,
        publication: Vec<String>,
    },
    Unknown {
        attempt: Option<ServiceReference>,
        failure: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServiceTerminal {
    pub epoch: [u8; 32],
    pub workers_retired: bool,
    pub socket_retired: bool,
    pub failures: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "message", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ServiceMessage {
    Boot { epoch: [u8; 32] },
    Acquisition { outcome: AcquisitionObservation },
    Ready { endpoint: String },
    Terminal { observation: ServiceTerminal },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    schema: u32,
    operation: [u8; 32],
    message: ServiceMessage,
}

pub(crate) fn send(
    stream: &mut UnixStream,
    operation: [u8; 32],
    message: ServiceMessage,
    deadline: Instant,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(&Envelope {
        schema: 2,
        operation,
        message,
    })?;
    if bytes.len() > MAX_FRAME {
        return Err(invalid("service control frame exceeds budget"));
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "service control deadline expired",
        ));
    }
    stream.set_nonblocking(true)?;
    let mut frame = (bytes.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(&bytes);
    crate::process::write_all_until(stream, &frame, deadline)
}

/// Nonblocking incremental decoder, retained across startup, action and shutdown.
/// Partial reads never renew the caller-owned deadline.
pub(crate) struct ServicePump {
    stream: UnixStream,
    operation: [u8; 32],
    daemon_pid: u32,
    pending: Vec<u8>,
    epoch: Option<[u8; 32]>,
    acquisition: Option<AcquisitionObservation>,
    endpoint: Option<String>,
    terminal: Option<ServiceTerminal>,
    ended: bool,
    broken: bool,
    partial_since: Option<Instant>,
}
impl ServicePump {
    pub fn new(stream: UnixStream, operation: [u8; 32], daemon_pid: u32) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            operation,
            daemon_pid,
            pending: Vec::new(),
            epoch: None,
            acquisition: None,
            endpoint: None,
            terminal: None,
            ended: false,
            broken: false,
            partial_since: None,
        })
    }
    pub fn epoch(&self) -> Option<[u8; 32]> {
        self.epoch
    }
    pub fn acquisition(&self) -> Option<&AcquisitionObservation> {
        self.acquisition.as_ref()
    }
    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }
    pub fn terminal(&self) -> Option<&ServiceTerminal> {
        self.terminal.as_ref()
    }
    pub fn poll(&mut self) -> io::Result<()> {
        if self.broken {
            return Err(invalid("service control stream is broken"));
        }
        let result = self.poll_inner();
        if result.is_err() {
            self.broken = true;
        }
        result
    }
    fn poll_inner(&mut self) -> io::Result<()> {
        if self.ended {
            return Ok(());
        }
        let mut chunk = [0u8; 4096];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    self.ended = true;
                    if !self.pending.is_empty() || self.terminal.is_none() {
                        return Err(invalid("truncated service control stream"));
                    }
                    break;
                }
                Ok(n) => self.pending.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
            self.decode()?;
            if self.pending.len() > MAX_FRAME + 4 {
                return Err(invalid("service control buffer exceeds budget"));
            }
        }
        self.decode()?;
        if self.pending.is_empty() {
            self.partial_since = None;
        } else {
            let since = self.partial_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= Duration::from_secs(5) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "partial service control frame deadline expired",
                ));
            }
        }
        Ok(())
    }
    pub fn retirement(
        &self,
        owner: &Path,
        reaped: &crate::process::OwnedChildReaped,
    ) -> io::Result<CompleteServiceRetirement> {
        let terminal = self
            .terminal
            .as_ref()
            .ok_or_else(|| invalid("terminal service observation absent"))?;
        if self.broken
            || !self.pending.is_empty()
            || self.daemon_pid != reaped.pid()
            || self.epoch != Some(terminal.epoch)
            || !terminal.workers_retired
            || !terminal.socket_retired
        {
            return Err(invalid("service retirement obligations remain unconfirmed"));
        }
        Ok(CompleteServiceRetirement {
            owner: owner.to_owned(),
            epoch: terminal.epoch,
        })
    }
    fn decode(&mut self) -> io::Result<()> {
        while self.pending.len() >= 4 {
            let length = u32::from_le_bytes(self.pending[..4].try_into().unwrap()) as usize;
            if length > MAX_FRAME {
                return Err(invalid("service control frame exceeds budget"));
            }
            if self.pending.len() < 4 + length {
                break;
            }
            let frame: Envelope = serde_json::from_slice(&self.pending[4..4 + length])?;
            if frame.schema != 2 || frame.operation != self.operation || self.terminal.is_some() {
                return Err(invalid("foreign or late service control frame"));
            }
            match frame.message {
                ServiceMessage::Boot { epoch } if self.epoch.is_none() => self.epoch = Some(epoch),
                ServiceMessage::Acquisition { outcome }
                    if self.epoch.is_some()
                        && self.acquisition.is_none()
                        && self.endpoint.is_none() =>
                {
                    self.acquisition = Some(outcome)
                }
                ServiceMessage::Ready { endpoint }
                    if self.epoch.is_some()
                        && self.acquisition.is_some()
                        && self.endpoint.is_none() =>
                {
                    if endpoint.len() != 64 || !endpoint.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Err(invalid("invalid service endpoint identity"));
                    }
                    self.endpoint = Some(endpoint);
                }
                ServiceMessage::Terminal { observation }
                    if self.epoch == Some(observation.epoch) =>
                {
                    self.terminal = Some(observation)
                }
                _ => return Err(invalid("duplicate or out-of-order service control frame")),
            }
            self.pending.drain(..4 + length);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ObservationStatus {
    NotStarted,
    Running,
    Exited,
    Failed,
    Pending,
    Reaped,
    Unconfirmed,
    Settled,
    NotConfigured,
    Retained,
    Removed,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Observation {
    pub status: ObservationStatus,
    pub exit_code: Option<u8>,
    pub success: Option<bool>,
    pub signal: Option<i32>,
    pub failure: Option<String>,
}
impl Observation {
    pub fn state(status: ObservationStatus) -> Self {
        Self {
            status,
            exit_code: None,
            success: None,
            signal: None,
            failure: None,
        }
    }
    pub fn failed(status: ObservationStatus, error: impl std::fmt::Display) -> Self {
        Self {
            failure: Some(error.to_string()),
            ..Self::state(status)
        }
    }
    pub fn exited(status: ObservationStatus, value: std::process::ExitStatus) -> Self {
        use std::os::unix::process::ExitStatusExt;
        Self {
            exit_code: Some(value.code().unwrap_or(1).clamp(0, 255) as u8),
            success: Some(value.success()),
            signal: value.signal(),
            ..Self::state(status)
        }
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OwnedServiceReport {
    pub schema: u32,
    pub kind: String,
    pub owner_directory: PathBuf,
    pub operation: String,
    pub daemon_epoch: Option<String>,
    pub daemon_pid: Option<u32>,
    pub producer: Option<String>,
    pub endpoint: Option<String>,
    pub socket_path: Option<PathBuf>,
    pub action: Observation,
    pub acquisition: AcquisitionObservation,
    pub daemon_reap: Observation,
    pub service_terminal: Option<ServiceTerminal>,
    pub source_settlement: Observation,
    pub artifact_retention: Observation,
    pub settlement_failures: Vec<String>,
    pub publication_failures: Vec<String>,
    pub complete: bool,
}
impl OwnedServiceReport {
    pub fn new(owner: PathBuf, operation: [u8; 32], configured: bool) -> Self {
        Self {
            schema: 2,
            kind: "owned-compiler-service".into(),
            owner_directory: owner,
            operation: crate::endpoint::hex(&operation),
            daemon_epoch: None,
            daemon_pid: None,
            producer: None,
            endpoint: None,
            socket_path: None,
            action: Observation::state(ObservationStatus::NotStarted),
            acquisition: if configured {
                AcquisitionObservation::Unknown {
                    attempt: None,
                    failure: "acquisition not observed".into(),
                }
            } else {
                AcquisitionObservation::NotConfigured
            },
            daemon_reap: Observation::state(ObservationStatus::NotStarted),
            service_terminal: None,
            source_settlement: Observation::state(if configured {
                ObservationStatus::Pending
            } else {
                ObservationStatus::NotConfigured
            }),
            artifact_retention: Observation::state(ObservationStatus::Pending),
            settlement_failures: vec![],
            publication_failures: vec![],
            complete: false,
        }
    }
    pub fn publish(&mut self) -> io::Result<()> {
        let bytes = serde_json::to_vec(self)?;
        tidepool_atomic_write::write_durable(&self.owner_directory.join(REPORT), &bytes)
            .map_err(io::Error::other)
    }
}

/// Affine authority produced from the authenticated child observation and the
/// actual owned daemon reap. This type deliberately has no decoder/serializer.
pub(crate) struct CompleteServiceRetirement {
    owner: PathBuf,
    epoch: [u8; 32],
}
impl CompleteServiceRetirement {
    pub fn owner(&self) -> &Path {
        &self.owner
    }
    pub fn epoch(&self) -> [u8; 32] {
        self.epoch
    }
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(crate) fn monotonic_nanos() -> io::Result<u64> {
    let mut time: libc::timespec = unsafe { std::mem::zeroed() };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((time.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(time.tv_nsec as u64))
}
pub(crate) fn instant_deadline(absolute: u64) -> io::Result<Instant> {
    let left = absolute
        .checked_sub(monotonic_nanos()?)
        .filter(|n| *n > 0)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::TimedOut, "service startup deadline expired")
        })?;
    Ok(Instant::now() + Duration::from_nanos(left))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn child(code: u8) -> crate::process::OwnedChild {
        let mut command = crate::process::command(
            std::env::var_os("TIDEPOOL_TEST_BASH").expect("declared test Bash"),
        );
        command.args(["-c", &format!("exit {code}")]);
        crate::process::OwnedChild(command.spawn().unwrap())
    }
    fn reap(child: &mut crate::process::OwnedChild) -> crate::process::OwnedChildReaped {
        let result = crate::process::observe_child_until(
            || child.0.try_wait(),
            Instant::now() + Duration::from_secs(5),
        );
        assert!(matches!(result, crate::CompilerWorkerRetirement::Reaped(_)));
        child.observe().unwrap().unwrap()
    }
    fn terminal(epoch: [u8; 32]) -> ServiceMessage {
        ServiceMessage::Terminal {
            observation: ServiceTerminal {
                epoch,
                workers_retired: true,
                socket_retired: true,
                failures: vec!["startup refused".into()],
            },
        }
    }
    #[test]
    fn terminal_before_ready_requires_matching_reap_and_accepts_nonzero_exit() {
        let owner = tempfile::tempdir().unwrap();
        let mut daemon = child(37);
        let mut foreign = child(0);
        let (read, mut write) = UnixStream::pair().unwrap();
        let mut pump = ServicePump::new(read, [1; 32], daemon.0.id()).unwrap();
        send(
            &mut write,
            [1; 32],
            ServiceMessage::Boot { epoch: [2; 32] },
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
        send(
            &mut write,
            [1; 32],
            terminal([2; 32]),
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
        pump.poll().unwrap();
        let foreign = reap(&mut foreign);
        assert!(pump.retirement(owner.path(), &foreign).is_err());
        let actual = reap(&mut daemon);
        assert_eq!(actual.status().code(), Some(37));
        let proof = pump.retirement(owner.path(), &actual).unwrap();
        assert_eq!(proof.owner(), owner.path());
        assert_eq!(proof.epoch(), [2; 32]);
    }
    #[test]
    fn process_reap_without_terminal_cannot_issue_retirement() {
        let owner = tempfile::tempdir().unwrap();
        let mut daemon = child(0);
        let actual = reap(&mut daemon);
        let (read, write) = UnixStream::pair().unwrap();
        let mut pump = ServicePump::new(read, [1; 32], actual.pid()).unwrap();
        drop(write);
        assert!(pump.poll().is_err());
        assert!(pump.retirement(owner.path(), &actual).is_err());
    }
    #[test]
    fn carrier_refuses_foreign_duplicate_late_and_crossed_terminal() {
        for messages in [
            vec![([9; 32], ServiceMessage::Boot { epoch: [2; 32] })],
            vec![
                ([1; 32], ServiceMessage::Boot { epoch: [2; 32] }),
                ([1; 32], ServiceMessage::Boot { epoch: [2; 32] }),
            ],
            vec![
                ([1; 32], ServiceMessage::Boot { epoch: [2; 32] }),
                ([1; 32], terminal([3; 32])),
            ],
            vec![(
                [1; 32],
                ServiceMessage::Ready {
                    endpoint: "a".repeat(64),
                },
            )],
            vec![
                ([1; 32], ServiceMessage::Boot { epoch: [2; 32] }),
                ([1; 32], terminal([2; 32])),
                (
                    [1; 32],
                    ServiceMessage::Acquisition {
                        outcome: AcquisitionObservation::NotConfigured,
                    },
                ),
            ],
        ] {
            let (read, mut write) = UnixStream::pair().unwrap();
            let mut pump = ServicePump::new(read, [1; 32], 123).unwrap();
            for (operation, message) in messages {
                send(
                    &mut write,
                    operation,
                    message,
                    Instant::now() + Duration::from_secs(1),
                )
                .unwrap();
            }
            assert!(pump.poll().is_err());
            assert!(pump.poll().is_err(), "protocol break must persist");
        }
    }
    #[test]
    fn carrier_partial_deadline_truncation_and_oversize_are_bounded() {
        for mode in 0..3 {
            let (read, mut write) = UnixStream::pair().unwrap();
            let mut pump = ServicePump::new(read, [1; 32], 123).unwrap();
            if mode == 2 {
                write
                    .write_all(&((MAX_FRAME + 1) as u32).to_le_bytes())
                    .unwrap();
                assert!(pump.poll().is_err());
                continue;
            }
            write.write_all(&[10, 0]).unwrap();
            pump.poll().unwrap();
            if mode == 0 {
                drop(write);
            } else {
                pump.partial_since = Some(Instant::now() - Duration::from_secs(6));
            }
            assert!(pump.poll().is_err());
        }
    }
    fn emit_owned_service_report_fixtures() {
        let Some(root) = std::env::var_os("TIDEPOOL_SERVICE_REPORT_FIXTURES") else {
            return;
        };
        let root = PathBuf::from(root);
        std::fs::create_dir_all(&root).unwrap();
        for name in [
            "complete",
            "failed-action",
            "startup-failed",
            "source-retained",
            "unknown",
        ] {
            let directory = root.join(name);
            std::fs::create_dir_all(&directory).unwrap();
            let directory = std::fs::canonicalize(directory).unwrap();
            let mut report = OwnedServiceReport::new(
                directory,
                [1; 32],
                name == "source-retained" || name == "unknown",
            );
            report.daemon_pid = Some(1234);
            report.daemon_epoch = Some(crate::endpoint::hex(&[2; 32]));
            report.producer = Some("a".repeat(64));
            report.endpoint = Some("b".repeat(64));
            report.socket_path = Some(root.join(name).join("compiler.sock"));
            report.action = Observation {
                exit_code: Some(if name == "failed-action" { 17 } else { 0 }),
                success: Some(name != "failed-action"),
                ..Observation::state(ObservationStatus::Exited)
            };
            report.daemon_reap = Observation {
                exit_code: Some(0),
                success: Some(true),
                ..Observation::state(ObservationStatus::Reaped)
            };
            report.service_terminal = Some(ServiceTerminal {
                epoch: [2; 32],
                workers_retired: true,
                socket_retired: true,
                failures: vec![],
            });
            report.artifact_retention = Observation::state(ObservationStatus::Retained);
            report.complete = true;
            if name == "startup-failed" {
                report.action = Observation::failed(ObservationStatus::Failed, "startup refusal");
                report.daemon_reap.exit_code = Some(1);
                report.daemon_reap.success = Some(false);
                report
                    .service_terminal
                    .as_mut()
                    .unwrap()
                    .failures
                    .push("startup refusal".into());
            }
            if name == "source-retained" || name == "unknown" {
                report.source_settlement = Observation::state(ObservationStatus::Retained);
                report.complete = false;
            }
            report.publish().unwrap();
        }
    }

    #[test]
    fn report_writer_keeps_primary_and_independent_observations() {
        emit_owned_service_report_fixtures();
        let owner = tempfile::tempdir().unwrap();
        let mut report = OwnedServiceReport::new(owner.path().to_owned(), [1; 32], true);
        report.action = Observation::failed(ObservationStatus::Failed, "primary startup failure");
        report.daemon_reap = Observation::failed(ObservationStatus::Unconfirmed, "wait failure");
        report.settlement_failures = vec!["worker retirement".into(), "source settlement".into()];
        report.publication_failures = vec!["earlier publication".into()];
        report.publish().unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(owner.path().join(REPORT)).unwrap()).unwrap();
        assert_eq!(value["schema"], 2);
        assert_eq!(value["action"]["failure"], "primary startup failure");
        assert_eq!(value["settlement_failures"].as_array().unwrap().len(), 2);
        assert_eq!(value["publication_failures"].as_array().unwrap().len(), 1);
        assert_eq!(value["complete"], false);
        assert_eq!(value["acquisition"]["status"], "unknown");
    }
}
