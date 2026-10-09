//! `exomonad proxy`: submit a Haskell cell into a running Exomonad session through
//! one resident operator workbench per run, so bindings persist between
//! invocations of this CLI.
use std::io::Read;
use std::path::{Path, PathBuf};

use rustix::fs::FlockOperation;
use serde::{Deserialize, Serialize};

use super::wire::{Block, SessionInfo, SubmitRequest, SubmitResponse};
use super::{Attachment, ProvisionRequest, ServiceIdentity};

pub struct ProxyOptions {
    pub session: String,
    pub file: Option<PathBuf>,
    pub json: bool,
    pub fresh: bool,
    pub actors: bool,
    pub runs_dir: Option<PathBuf>,
}

#[derive(Debug)]
pub struct ProxyError(pub String);
impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ProxyError {}
impl From<String> for ProxyError {
    fn from(value: String) -> Self {
        Self(value)
    }
}
impl From<&str> for ProxyError {
    fn from(value: &str) -> Self {
        Self(value.into())
    }
}

/// Resolve the run root for a live Exomonad session, by scanning `runs_dir` for
/// `status.json` files naming that session with a not-yet-terminal phase and
/// a still-present operator socket.
pub fn run_root_for_session(runs_dir: &Path, session: &str) -> Result<PathBuf, ProxyError> {
    let matches = live_run_roots(runs_dir, session)?;
    match matches.len() {
        0 => Err(format!(
            "no live run for session {session:?} (looked in {})",
            runs_dir.display()
        )
        .into()),
        1 => Ok(matches[0].clone()),
        _ => Err(format!(
            "session {session:?} matches multiple live runs: {}",
            matches
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
        .into()),
    }
}

fn live_run_roots(runs_dir: &Path, session: &str) -> Result<Vec<PathBuf>, ProxyError> {
    let mut matches = Vec::new();
    let entries = std::fs::read_dir(runs_dir).map_err(|e| {
        format!(
            "cannot read Exomonad runs directory {}: {e}",
            runs_dir.display()
        )
    })?;
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let run_root = entry.path();
        let status_path = run_root.join("status.json");
        let Ok(bytes) = std::fs::read(&status_path) else {
            continue;
        };
        let Ok(status) = crate::exomonad::decode_run_status(&bytes) else {
            continue;
        };
        if status.session != session {
            continue;
        }
        if matches!(
            status.phase,
            crate::exomonad::RunPhase::Failed { .. } | crate::exomonad::RunPhase::Exited
        ) {
            continue;
        }
        if !run_root.join("operator/operator.sock").exists() {
            continue;
        }
        matches.push(run_root);
    }
    Ok(matches)
}

fn default_runs_dirs() -> Result<Vec<PathBuf>, ProxyError> {
    let state = tidepool_toolchain::paths::state_dir()
        .map_err(|error| ProxyError(format!("cannot locate durable Exomonad state: {error}")))?
        .join("exomonad/runs");
    let legacy = tidepool_toolchain::paths::cache_dir().join("exomonad/runs");
    let mut roots = vec![state];
    if !roots.contains(&legacy) {
        roots.push(legacy);
    }
    Ok(roots)
}

fn run_root_for_session_in_roots(
    runs_dirs: &[PathBuf],
    session: &str,
) -> Result<PathBuf, ProxyError> {
    let mut matches = Vec::new();
    for runs_dir in runs_dirs {
        if !runs_dir.exists() {
            continue;
        }
        matches.extend(live_run_roots(runs_dir, session)?);
    }
    match matches.len() {
        0 => Err(format!(
            "no live run for session {session:?} (looked in {})",
            runs_dirs
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
        .into()),
        1 => Ok(matches.remove(0)),
        _ => Err(format!(
            "session {session:?} matches multiple live runs across state and legacy cache: {}",
            matches
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
        .into()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProxyRecord {
    version: u32,
    selection: ProxySelection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum ProxySelection {
    Pending {
        request: ProvisionRequest,
    },
    Ready {
        service: ServiceIdentity,
        session: String,
        operation: Option<uuid::Uuid>,
    },
}

impl ProxyRecord {
    fn new(selection: ProxySelection) -> Self {
        Self {
            version: 1,
            selection,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyProxyRecord {
    session: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum StoredProxyRecord {
    Current(ProxyRecord),
    Legacy(LegacyProxyRecord),
}

/// What to do with a stored proxy session, given whether the caller asked for
/// `--fresh` and whether the stored session is still alive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Reuse(String),
    StopThenProvision(String),
    Provision,
}

/// The stored proxy session, if any, and whether it still answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredSession {
    Absent,
    Alive(String),
    Dead(String),
}

pub fn decide(stored: StoredSession, fresh: bool) -> Decision {
    match (stored, fresh) {
        (StoredSession::Alive(session) | StoredSession::Dead(session), true) => {
            Decision::StopThenProvision(session)
        }
        (StoredSession::Alive(session), false) => Decision::Reuse(session),
        (StoredSession::Dead(_) | StoredSession::Absent, false) | (StoredSession::Absent, true) => {
            Decision::Provision
        }
    }
}

fn read_proxy_record(path: &Path) -> Result<Option<StoredProxyRecord>, ProxyError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display()).into()),
    };
    let record = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "cannot decode {}: {error}; inspect the saved selection before provisioning",
            path.display()
        )
    })?;
    if let StoredProxyRecord::Current(ProxyRecord { version, .. }) = &record {
        if *version != 1 {
            return Err(format!("unsupported proxy record version {version}").into());
        }
    }
    Ok(Some(record))
}

fn write_proxy_record(path: &Path, record: &ProxyRecord) -> Result<(), ProxyError> {
    let bytes = serde_json::to_vec(record)
        .map_err(|error| format!("cannot encode proxy record: {error}"))?;
    tidepool_atomic_write::write_durable(path, &bytes)
        .map_err(|error| format!("cannot durably write {}: {error}; retry the saved operation, not a new provisioning request", path.display()).into())
}

/// Poll nonblocking flock on workers; cancellation drops the descriptor even
/// when it races the completion of one worker attempt.
#[derive(Debug)]
struct ProxyLock(std::fs::File);
impl ProxyLock {
    async fn acquire(run_root: &Path) -> Result<Self, ProxyError> {
        let run_root = run_root.to_owned();
        let mut file = tokio::task::spawn_blocking(move || -> Result<_, ProxyError> {
            let anchor = tidepool_atomic_write::DirectoryAnchor::open_existing(&run_root)
                .map_err(|error| format!("cannot open run root: {error}"))?;
            let operator_dir = anchor
                .create_dir_all("operator")
                .map_err(|error| format!("cannot establish operator directory: {error}"))?;
            let lock_path = operator_dir.join("proxy.lock");
            std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&lock_path)
                .map_err(|error| format!("cannot open {}: {error}", lock_path.display()).into())
        })
        .await
        .map_err(|error| format!("cannot acquire proxy lock: {error}"))??;
        loop {
            let attempted = tokio::task::spawn_blocking(move || {
                let acquired =
                    match rustix::fs::flock(&file, FlockOperation::NonBlockingLockExclusive) {
                        Ok(()) => Ok(true),
                        Err(error)
                            if std::io::Error::from(error).kind()
                                == std::io::ErrorKind::WouldBlock =>
                        {
                            Ok(false)
                        }
                        Err(error) => Err(ProxyError(format!("cannot lock proxy: {error}"))),
                    };
                (file, acquired)
            })
            .await
            .map_err(|error| format!("cannot acquire proxy lock: {error}"))?;
            file = attempted.0;
            if attempted.1? {
                return Ok(Self(file));
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}
impl Drop for ProxyLock {
    fn drop(&mut self) {
        // Closing the file also releases the lock after an unlock error.
        rustix::fs::flock(&self.0, FlockOperation::Unlock).ok();
    }
}

/// The saved identity already exists durably before this request is sent.
type RecordWriter = dyn Fn(&Path, &ProxyRecord) -> Result<(), ProxyError> + Sync;

async fn provision(
    client: &reqwest::Client,
    proxy_json: &Path,
    request: ProvisionRequest,
    write: &RecordWriter,
) -> Result<String, ProxyError> {
    // A previous post-rename failure may have left the exact Pending bytes
    // visible. Confirm their durability before crossing HTTP admission.
    std::fs::File::open(proxy_json)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("cannot confirm saved provisioning operation: {error}"))?;
    let parent = proxy_json.parent().ok_or("proxy record needs a parent")?;
    tidepool_atomic_write::DirectoryAnchor::open_existing(parent)
        .and_then(|anchor| anchor.create_dir_all(""))
        .map_err(|error| format!("cannot confirm saved provisioning operation: {error}"))?;
    let response = client
        .post("http://localhost/host/operators")
        .json(&request)
        .send()
        .await
        .map_err(|error| {
            format!("operator provisioning outcome unknown: {error}; retry the saved operation")
        })?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!(
            "cannot provision operator workbench: {status}: {body}; saved operation retained"
        )
        .into());
    }
    let attachment: Attachment = response.json().await.map_err(|error| {
        format!("cannot decode operator provisioning response: {error}; retry the saved operation")
    })?;
    write(
        proxy_json,
        &ProxyRecord::new(ProxySelection::Ready {
            service: request.service,
            operation: Some(request.operation),
            session: attachment.session.clone(),
        }),
    )?;
    Ok(attachment.session)
}

async fn stop(client: &reqwest::Client, session: &str) -> Result<(), ProxyError> {
    let mut url = reqwest::Url::parse("http://localhost/host/operators/")
        .map_err(|e| format!("invalid operator URL: {e}"))?;
    url.path_segments_mut()
        .map_err(|_| "invalid operator URL")?
        .pop_if_empty()
        .push(session)
        .push("stop");
    let response = client
        .post(url)
        .send()
        .await
        .map_err(|e| format!("cannot stop stale operator session {session}: {e}"))?;
    // A session that is already gone is not an error here: our job is only
    // to ensure it is gone before provisioning its replacement.
    if !response.status().is_success() && response.status() != reqwest::StatusCode::NOT_FOUND {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(
            format!("cannot stop stale operator session {session}: {status}: {body}").into(),
        );
    }
    Ok(())
}

async fn session_alive(client: &reqwest::Client, session: &str) -> Result<bool, ProxyError> {
    let response = client
        .get(session_url(session, None)?)
        .send()
        .await
        .map_err(|e| format!("cannot inspect operator session {session}: {e}"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(false);
    }
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("cannot inspect operator session {session}: {status}: {body}").into());
    }
    let observed: SessionInfo = response
        .json()
        .await
        .map_err(|e| format!("cannot decode operator session response: {e}"))?;
    if observed.session != session || observed.protocol_version != super::wire::PROTOCOL_VERSION {
        return Err(
            "operator session observation does not match the requested session/protocol".into(),
        );
    }
    Ok(true)
}

fn session_url(session: &str, endpoint: Option<&str>) -> Result<reqwest::Url, ProxyError> {
    let mut url = reqwest::Url::parse("http://localhost/v1/sessions/")
        .map_err(|error| format!("invalid operator URL: {error}"))?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| "invalid operator URL")?;
        segments.pop_if_empty().push(session);
        if let Some(endpoint) = endpoint {
            segments.push(endpoint);
        }
    }
    Ok(url)
}

/// Ensure a live resident proxy workbench exists for this run and return its
/// operator session id. Provisioning and the `--fresh` stop-then-provision
/// decision happen under `proxy.lock`; the lock is released before the caller
/// submits anything.
async fn resident_operator_session(
    run_root: &Path,
    client: &reqwest::Client,
    fresh: bool,
) -> Result<String, ProxyError> {
    resolve_proxy_selection(run_root, client, fresh, &write_proxy_record).await
}

async fn resolve_proxy_selection(
    run_root: &Path,
    client: &reqwest::Client,
    fresh: bool,
    write: &RecordWriter,
) -> Result<String, ProxyError> {
    let proxy_json = run_root.join("operator").join("proxy.json");
    let lock = ProxyLock::acquire(run_root).await?;
    // Refuse malformed/unreadable state before even observing the network.
    let stored = read_proxy_record(&proxy_json)?;
    let service = super::service_identity(client).await?;
    let session = match stored {
        Some(StoredProxyRecord::Legacy(legacy)) => {
            if !session_alive(client, &legacy.session).await? {
                return Err("legacy proxy selection is not live; list current operators and explicitly reconcile the saved session before provisioning".into());
            }
            // This binds an observed live session to the current owner; it
            // does not claim proof of the legacy record's historical owner.
            write(
                &proxy_json,
                &ProxyRecord::new(ProxySelection::Ready {
                    service,
                    session: legacy.session.clone(),
                    operation: None,
                }),
            )?;
            Some(legacy.session)
        }
        Some(StoredProxyRecord::Current(record)) => match record.selection {
            ProxySelection::Pending { request } => {
                if request.service != service {
                    return Err(service_changed());
                }
                Some(provision(client, &proxy_json, request, write).await?)
            }
            ProxySelection::Ready {
                service: saved,
                session,
                ..
            } => {
                if saved != service {
                    return Err(service_changed());
                }
                Some(session)
            }
        },
        None => None,
    };
    let stored = match session {
        Some(session) if session_alive(client, &session).await? => StoredSession::Alive(session),
        Some(session) => StoredSession::Dead(session),
        None => StoredSession::Absent,
    };
    let result = match decide(stored, fresh) {
        Decision::Reuse(session) => Ok(session),
        Decision::StopThenProvision(session) => {
            stop(client, &session).await?;
            begin_provision(client, &proxy_json, service, write).await
        }
        Decision::Provision => begin_provision(client, &proxy_json, service, write).await,
    };
    drop(lock);
    result
}

fn service_changed() -> ProxyError {
    "operator service incarnation changed; saved selection retained. List operators and explicitly reconcile it before provisioning".into()
}

async fn begin_provision(
    client: &reqwest::Client,
    proxy_json: &Path,
    service: ServiceIdentity,
    write: &RecordWriter,
) -> Result<String, ProxyError> {
    let request = ProvisionRequest::new(service);
    write(
        proxy_json,
        &ProxyRecord::new(ProxySelection::Pending { request }),
    )?;
    provision(client, proxy_json, request, write).await
}

fn read_source(file: &Path) -> Result<String, ProxyError> {
    if file == Path::new("-") {
        let mut source = String::new();
        std::io::stdin()
            .read_to_string(&mut source)
            .map_err(|e| format!("cannot read stdin: {e}"))?;
        return Ok(source);
    }
    std::fs::read_to_string(file).map_err(|e| format!("cannot read {}: {e}", file.display()).into())
}

fn print_blocks(response: &SubmitResponse) {
    for block in &response.blocks {
        match block {
            Block::Output(text) => println!("{text}"),
            Block::Diagnostic(text) => eprintln!("{text}"),
        }
    }
    if let Some(receipt) = &response.receipt {
        println!("{}", receipt.display);
    }
}

pub async fn proxy(options: ProxyOptions) -> Result<(), Box<dyn std::error::Error>> {
    if !options.actors && options.file.is_none() {
        return Err(Box::new(ProxyError(
            "exomonad proxy requires FILE (or `-` for stdin) unless --actors is given".into(),
        )));
    }
    let run_root = match options.runs_dir {
        Some(runs_dir) => run_root_for_session(&runs_dir, &options.session)?,
        None => run_root_for_session_in_roots(&default_runs_dirs()?, &options.session)?,
    };
    let socket = run_root.join("operator/operator.sock");
    let (client, _address) = super::client_for(&socket).map_err(|error| {
        ProxyError(format!(
            "cannot connect to operator socket {}: {error}",
            socket.display()
        ))
    })?;
    let session = resident_operator_session(&run_root, &client, options.fresh).await?;

    if options.actors {
        let response = client
            .get(session_url(&session, Some("actors"))?)
            .send()
            .await
            .map_err(|e| format!("cannot list actors for session {session}: {e}"))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| format!("cannot read actor listing response: {e}"))?;
        if !status.is_success() {
            return Err(Box::new(ProxyError(format!("{status}: {body}"))));
        }
        if options.json {
            println!("{body}");
        } else {
            let value: serde_json::Value = serde_json::from_str(&body)
                .map_err(|e| format!("cannot decode actor listing: {e}"))?;
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        return Ok(());
    }

    // `file` is required (validated above) whenever `--actors` is absent.
    #[allow(
        clippy::expect_used,
        reason = "invariant: line 309 already errored out if !actors && file.is_none(), and the actors branch above returns early"
    )]
    let source = read_source(options.file.as_deref().expect("validated above"))?;
    let sent = client
        .post(session_url(&session, Some("submit"))?)
        .json(&SubmitRequest { source })
        .send()
        .await;
    let response = match sent {
        Ok(response) => response,
        Err(_) => return indeterminate(&options.session),
    };
    let status = response.status();
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(_) => return indeterminate(&options.session),
    };
    if !status.is_success() {
        let body = String::from_utf8_lossy(&bytes);
        return Err(Box::new(ProxyError(format!("{status}: {body}"))));
    }
    let parsed: Result<SubmitResponse, _> = serde_json::from_slice(&bytes);
    let response = match parsed {
        Ok(response) => response,
        Err(_) => return indeterminate(&options.session),
    };

    if options.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        print_blocks(&response);
    }
    match response.outcome {
        super::wire::Outcome::Completed => Ok(()),
        super::wire::Outcome::Rejected => std::process::exit(1),
    }
}

fn indeterminate(session: &str) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!(
        "indeterminate: the cell may have run; inspect with `exomonad proxy {session} --actors` or a display cell; not replaying"
    );
    std::process::exit(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_status(run_root: &Path, session: &str, phase: crate::exomonad::RunPhase) {
        std::fs::create_dir_all(run_root).unwrap();
        let status = serde_json::json!({
            "version": 4,
            "run_id": run_root.file_name().unwrap().to_str().unwrap(),
            "workspace": "/tmp/workspace",
            "session": session,
            "agent": {"model": "test-model", "effort": "low"},
            "phase": match phase {
                crate::exomonad::RunPhase::Starting => serde_json::json!({"state": "starting"}),
                crate::exomonad::RunPhase::Exited => serde_json::json!({"state": "exited"}),
                crate::exomonad::RunPhase::Failed { error } => {
                    serde_json::json!({"state": "failed", "error": error})
                }
                crate::exomonad::RunPhase::AwaitingBinding { .. }
                | crate::exomonad::RunPhase::Ready { .. }
                | crate::exomonad::RunPhase::EmbeddedReady { .. }
                | crate::exomonad::RunPhase::Recovering { .. } => {
                    unreachable!("not exercised by these tests")
                }
            },
        });
        std::fs::write(
            run_root.join("status.json"),
            serde_json::to_vec(&status).unwrap(),
        )
        .unwrap();
        let operator_dir = run_root.join("operator");
        std::fs::create_dir_all(&operator_dir).unwrap();
        std::fs::write(operator_dir.join("operator.sock"), b"").unwrap();
    }

    #[test]
    fn resolves_the_unique_live_run_for_a_session() {
        let runs = tempfile::tempdir().unwrap();
        write_status(
            &runs.path().join("run-a"),
            "session-a",
            crate::exomonad::RunPhase::Starting,
        );
        write_status(
            &runs.path().join("run-b"),
            "session-b",
            crate::exomonad::RunPhase::Starting,
        );
        let resolved = run_root_for_session(runs.path(), "session-b").unwrap();
        assert_eq!(resolved, runs.path().join("run-b"));
    }

    #[test]
    fn no_match_names_the_search_directory() {
        let runs = tempfile::tempdir().unwrap();
        write_status(
            &runs.path().join("run-a"),
            "session-a",
            crate::exomonad::RunPhase::Starting,
        );
        let error = run_root_for_session(runs.path(), "missing").unwrap_err();
        assert!(error.to_string().contains("missing"));
        assert!(
            error
                .to_string()
                .contains(&runs.path().display().to_string())
        );
    }

    #[test]
    fn ambiguous_match_names_every_run_id() {
        let runs = tempfile::tempdir().unwrap();
        write_status(
            &runs.path().join("run-a"),
            "shared",
            crate::exomonad::RunPhase::Starting,
        );
        write_status(
            &runs.path().join("run-b"),
            "shared",
            crate::exomonad::RunPhase::Starting,
        );
        let error = run_root_for_session(runs.path(), "shared").unwrap_err();
        assert!(error.to_string().contains("run-a"));
        assert!(error.to_string().contains("run-b"));
    }

    #[test]
    fn default_discovery_reaches_legacy_and_refuses_cross_root_ambiguity() {
        let state = tempfile::tempdir().unwrap();
        let legacy = tempfile::tempdir().unwrap();
        write_status(
            &legacy.path().join("legacy-run"),
            "shared",
            crate::exomonad::RunPhase::Starting,
        );
        let roots = vec![state.path().into(), legacy.path().into()];
        assert_eq!(
            run_root_for_session_in_roots(&roots, "shared").unwrap(),
            legacy.path().join("legacy-run")
        );
        write_status(
            &state.path().join("state-run"),
            "shared",
            crate::exomonad::RunPhase::Starting,
        );
        let error = run_root_for_session_in_roots(&roots, "shared")
            .unwrap_err()
            .to_string();
        assert!(error.contains("legacy-run"));
        assert!(error.contains("state-run"));
    }

    #[test]
    fn exited_runs_are_not_candidates() {
        let runs = tempfile::tempdir().unwrap();
        write_status(
            &runs.path().join("run-a"),
            "session-a",
            crate::exomonad::RunPhase::Exited,
        );
        let error = run_root_for_session(runs.path(), "session-a").unwrap_err();
        assert!(error.to_string().contains("no live run"));
    }

    #[test]
    fn proxy_json_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proxy.json");
        let record = ProxyRecord::new(ProxySelection::Ready {
            service: ServiceIdentity {
                incarnation: uuid::Uuid::new_v4(),
            },
            session: "operator-1-1".into(),
            operation: None,
        });
        write_proxy_record(&path, &record).unwrap();
        let Some(StoredProxyRecord::Current(read)) = read_proxy_record(&path).unwrap() else {
            panic!("current record");
        };
        assert_eq!(read, record);
    }

    #[test]
    fn decide_table() {
        use StoredSession::{Absent, Alive, Dead};
        assert_eq!(decide(Absent, false), Decision::Provision);
        assert_eq!(decide(Absent, true), Decision::Provision);
        assert_eq!(
            decide(Alive("s".into()), false),
            Decision::Reuse("s".into())
        );
        assert_eq!(decide(Dead("s".into()), false), Decision::Provision);
        assert_eq!(
            decide(Alive("s".into()), true),
            Decision::StopThenProvision("s".into())
        );
        assert_eq!(
            decide(Dead("s".into()), true),
            Decision::StopThenProvision("s".into())
        );
    }
}

#[cfg(test)]
mod recovery_tests;
