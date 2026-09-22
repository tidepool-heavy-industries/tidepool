//! `exomonad proxy`: submit a Haskell cell into a running Exomonad session through
//! one resident operator workbench per run, so bindings persist between
//! invocations of this CLI.
use std::io::Read;
use std::path::{Path, PathBuf};

use rustix::fs::FlockOperation;
use serde::{Deserialize, Serialize};

use super::wire::{Block, SessionInfo, SubmitRequest, SubmitResponse};
use super::Attachment;

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
        matches.push(
            entry
                .file_name()
                .to_str()
                .map(str::to_owned)
                .unwrap_or_else(|| run_root.display().to_string()),
        );
    }
    match matches.len() {
        0 => Err(format!(
            "no live run for session {session:?} (looked in {})",
            runs_dir.display()
        )
        .into()),
        1 => Ok(runs_dir.join(&matches[0])),
        _ => Err(format!(
            "session {session:?} matches multiple live runs: {}",
            matches.join(", ")
        )
        .into()),
    }
}

fn default_runs_dir() -> PathBuf {
    tidepool_toolchain::paths::cache_dir()
        .join("exomonad")
        .join("runs")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProxyRecord {
    session: String,
}

/// What to do with a stored proxy session, given whether the caller asked for
/// `--fresh` and whether the stored session is still alive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Reuse(String),
    StopThenProvision(String),
    Provision,
}

pub fn decide(stored: Option<String>, fresh: bool, alive: bool) -> Decision {
    match (stored, fresh) {
        (Some(session), true) => Decision::StopThenProvision(session),
        (Some(session), false) if alive => Decision::Reuse(session),
        (Some(_), false) => Decision::Provision,
        (None, _) => Decision::Provision,
    }
}

fn read_proxy_record(path: &Path) -> Option<ProxyRecord> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_proxy_record(path: &Path, record: &ProxyRecord) -> Result<(), ProxyError> {
    let bytes =
        serde_json::to_vec(record).map_err(|e| format!("cannot encode proxy record: {e}"))?;
    tidepool_atomic_write::write_best_effort(path, &bytes)
        .map_err(|e| format!("cannot write {}: {e}", path.display()).into())
}

/// Holds `run_root/operator/proxy.lock` exclusively until dropped. Advisory
/// `flock` acquisition and release are cheap local-filesystem syscalls, so
/// they run directly on the async task rather than via `spawn_blocking`.
struct ProxyLock(std::fs::File);
impl ProxyLock {
    fn acquire(run_root: &Path) -> Result<Self, ProxyError> {
        let operator_dir = run_root.join("operator");
        std::fs::create_dir_all(&operator_dir)
            .map_err(|e| format!("cannot create {}: {e}", operator_dir.display()))?;
        let lock_path = operator_dir.join("proxy.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&lock_path)
            .map_err(|e| format!("cannot open {}: {e}", lock_path.display()))?;
        rustix::fs::flock(&file, FlockOperation::LockExclusive)
            .map_err(|e| format!("cannot lock {}: {e}", lock_path.display()))?;
        Ok(Self(file))
    }
}
impl Drop for ProxyLock {
    fn drop(&mut self) {
        let _ = rustix::fs::flock(&self.0, FlockOperation::Unlock);
    }
}

fn client_for(socket: &Path) -> Result<reqwest::Client, ProxyError> {
    reqwest::Client::builder()
        .unix_socket(socket)
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()
        .map_err(|e| format!("cannot build operator client for {}: {e}", socket.display()).into())
}

/// Provision a fresh operator workbench and record it.
async fn provision(client: &reqwest::Client, proxy_json: &Path) -> Result<String, ProxyError> {
    let response = client
        .post("http://localhost/host/operators")
        .send()
        .await
        .map_err(|e| format!("cannot provision operator workbench: {e}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("cannot provision operator workbench: {status}: {body}").into());
    }
    let attachment: Attachment = response
        .json()
        .await
        .map_err(|e| format!("cannot decode operator provisioning response: {e}"))?;
    write_proxy_record(
        proxy_json,
        &ProxyRecord {
            session: attachment.session.clone(),
        },
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
        .get(format!("http://localhost/v1/sessions/{session}"))
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
    let _: SessionInfo = response
        .json()
        .await
        .map_err(|e| format!("cannot decode operator session response: {e}"))?;
    Ok(true)
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
    let proxy_json = run_root.join("operator").join("proxy.json");
    let lock = ProxyLock::acquire(run_root)?;
    let stored = read_proxy_record(&proxy_json).map(|r| r.session);
    let alive = match &stored {
        Some(session) => session_alive(client, session).await?,
        None => false,
    };
    let result = match decide(stored, fresh, alive) {
        Decision::Reuse(session) => Ok(session),
        Decision::StopThenProvision(session) => {
            stop(client, &session).await?;
            provision(client, &proxy_json).await
        }
        Decision::Provision => provision(client, &proxy_json).await,
    };
    drop(lock);
    result
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
    let runs_dir = options.runs_dir.unwrap_or_else(default_runs_dir);
    let run_root = run_root_for_session(&runs_dir, &options.session)?;
    let socket = run_root.join("operator/operator.sock");
    let client = client_for(&socket)?;
    let session = resident_operator_session(&run_root, &client, options.fresh).await?;

    if options.actors {
        let response = client
            .get(format!("http://localhost/v1/sessions/{session}/actors"))
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
    let source = read_source(options.file.as_deref().expect("validated above"))?;
    let sent = client
        .post(format!("http://localhost/v1/sessions/{session}/submit"))
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
        assert!(error
            .to_string()
            .contains(&runs.path().display().to_string()));
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
        write_proxy_record(
            &path,
            &ProxyRecord {
                session: "operator-1-1".into(),
            },
        )
        .unwrap();
        let read = read_proxy_record(&path).unwrap();
        assert_eq!(read.session, "operator-1-1");
    }

    #[test]
    fn decide_table() {
        assert_eq!(decide(None, false, false), Decision::Provision);
        assert_eq!(decide(None, true, false), Decision::Provision);
        assert_eq!(
            decide(Some("s".into()), false, true),
            Decision::Reuse("s".into())
        );
        assert_eq!(decide(Some("s".into()), false, false), Decision::Provision);
        assert_eq!(
            decide(Some("s".into()), true, true),
            Decision::StopThenProvision("s".into())
        );
        assert_eq!(
            decide(Some("s".into()), true, false),
            Decision::StopThenProvision("s".into())
        );
    }
}
