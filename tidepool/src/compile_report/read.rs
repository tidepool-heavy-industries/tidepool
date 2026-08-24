//! Reads the durable compile-failure evidence a compile-failure report folds
//! over — through the ONE durable-JSONL read primitive
//! ([`tidepool_repr::jsonl::read_tail`]), never a hand-rolled `BufReader`.
//!
//! Two row shapes are recognized, auto-detected per line (so one reader
//! handles both a harness `transcript.jsonl` and this crate's own
//! `eval-failures.jsonl` — see [`crate::compile_report`]'s module doc):
//!
//! - **`answerer_round`** — `tidepool_harness::selfharness::observer::Event`'s
//!   `AnswererRound{node,site,round,error}` variant, as written to
//!   `transcript.jsonl`. That type derives `Serialize` only (not
//!   `Deserialize`) and this crate's spec forbids touching
//!   `tidepool-harness/src`, so the three fields this report needs are read
//!   by hand off `serde_json::Value` — the same shape
//!   `tidepool-handlers::handlers::journal::JournalEntry::from_json` already
//!   uses to fold a foreign JSONL schema it does not own the Rust type for.
//! - **eval-failure rows** — this crate's own `{ts_ms, op, class, phase,
//!   detail}` shape, written by `tidepool-mcp`'s eval-surface logging (see
//!   `tidepool_runtime::paths::eval_failure_log_path`).
//!
//! Every other row shape (`log-*.jsonl`'s per-node `Event`, an
//! `outer_compile` transcript line, …) is silently skipped — this reader
//! only extracts what a compile-failure report classifies.

use std::path::{Path, PathBuf};

use serde_json::Value;
use tidepool_repr::jsonl::{self, TailPolicy};

/// One `answerer_round` transcript line's three report-relevant fields.
/// `error` is `None` on a round that compiled; the fold needs every round
/// (not just failures) to compute a first-try-compile rate per hole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswererRoundRow {
    pub site: u32,
    pub round: u32,
    pub error: Option<String>,
}

/// One eval-surface compile/run failure row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalFailureRow {
    pub op: String,
    pub class: String,
    pub phase: String,
    pub detail: String,
}

/// Every recognized row from one evidence file, labeled with the file it
/// came from — the report's "per-run" trend unit (see `report.rs`).
#[derive(Debug, Clone, Default)]
pub struct RunEvidence {
    pub run_label: String,
    pub answerer_rounds: Vec<AnswererRoundRow>,
    pub eval_failures: Vec<EvalFailureRow>,
}

/// Why a file failed to read as evidence — the same two failure shapes
/// [`jsonl::read_tail`] itself can report, since this reader adds no I/O of
/// its own beyond that one call.
#[derive(Debug)]
pub enum EvidenceReadError {
    Io(std::io::Error),
    TornMidFile {
        path: PathBuf,
        line_no: usize,
        detail: String,
    },
}

impl std::fmt::Display for EvidenceReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EvidenceReadError::Io(e) => write!(f, "{e}"),
            EvidenceReadError::TornMidFile {
                path,
                line_no,
                detail,
            } => write!(
                f,
                "{path:?} corrupted at line {line_no} (not the final line): {detail}"
            ),
        }
    }
}

impl std::error::Error for EvidenceReadError {}

fn parse_answerer_round(v: &Value) -> Option<AnswererRoundRow> {
    let site = v.get("site")?.as_u64()? as u32;
    let round = v.get("round")?.as_u64()? as u32;
    let error = match v.get("error") {
        Some(Value::Null) | None => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => None,
    };
    Some(AnswererRoundRow { site, round, error })
}

fn parse_eval_failure(v: &Value) -> Option<EvalFailureRow> {
    let op = v.get("op")?.as_str()?.to_string();
    let class = v.get("class")?.as_str()?.to_string();
    let phase = v.get("phase")?.as_str()?.to_string();
    let detail = v.get("detail")?.as_str()?.to_string();
    Some(EvalFailureRow {
        op,
        class,
        phase,
        detail,
    })
}

/// Read one evidence file (a `transcript.jsonl` or an eval-failures log) into
/// its recognized rows. A missing file is empty evidence, not an error — the
/// same "no file yet" tolerance [`jsonl::read_tail`] itself gives.
///
/// `TailPolicy::Observe`: this reader never owns the file (it reads a
/// harness's or a running server's live log), so a torn final line is
/// reported-but-left-alone, exactly as `load_journal` already treats a
/// segment file it doesn't own.
pub fn read_evidence_file(path: &Path) -> Result<RunEvidence, EvidenceReadError> {
    let (rows, torn) = jsonl::read_tail(
        path,
        |l| serde_json::from_str::<Value>(l).map_err(|e| e.to_string()),
        TailPolicy::Observe,
    )
    .map_err(|e| match e {
        jsonl::JsonlReadError::Io(io) => EvidenceReadError::Io(io),
        jsonl::JsonlReadError::TornMidFile { line_no, detail } => EvidenceReadError::TornMidFile {
            path: path.to_path_buf(),
            line_no,
            detail,
        },
    })?;
    if let Some(torn) = torn {
        eprintln!(
            "warning: {path:?}: torn final line skipped (crash mid-append?): {}",
            torn.reason
        );
    }

    let mut evidence = RunEvidence {
        run_label: run_label_for(path),
        ..Default::default()
    };
    for v in &rows {
        match v.get("ev").and_then(Value::as_str) {
            Some("answerer_round") => {
                if let Some(row) = parse_answerer_round(v) {
                    evidence.answerer_rounds.push(row);
                }
            }
            _ => {
                if let Some(row) = parse_eval_failure(v) {
                    evidence.eval_failures.push(row);
                }
            }
        }
    }
    Ok(evidence)
}

/// The trend label for one evidence file: its file stem (`transcript` from
/// `.../round-3/transcript.jsonl`) prefixed with its parent directory name
/// when present (`round-3/transcript`), so a glob over several dogfood round
/// directories produces distinct, orderable labels instead of the same
/// `"transcript"` repeated.
fn run_label_for(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    match path.parent().and_then(|p| p.file_name()) {
        Some(parent) => format!("{}/{}", parent.to_string_lossy(), stem),
        None => stem,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "tidepool_compile_report_read_{label}_{}_{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    #[test]
    fn missing_file_is_empty_evidence() {
        let path = tmp_path("missing");
        let _ = std::fs::remove_file(&path);
        let ev = read_evidence_file(&path).unwrap();
        assert!(ev.answerer_rounds.is_empty());
        assert!(ev.eval_failures.is_empty());
    }

    #[test]
    fn reads_answerer_round_and_eval_failure_rows_skipping_others() {
        let path = tmp_path("mixed");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"ev":"loop_boundary"}}"#).unwrap();
        writeln!(
            f,
            r#"{{"ev":"answerer_round","node":0,"site":1,"round":1,"error":null}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"ev":"answerer_round","node":0,"site":1,"round":2,"error":"boom"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"ts_ms":1,"op":"eval","class":"user-haskell","phase":"compile","detail":"nope"}}"#
        )
        .unwrap();
        drop(f);

        let ev = read_evidence_file(&path).unwrap();
        assert_eq!(ev.answerer_rounds.len(), 2);
        assert_eq!(ev.answerer_rounds[0].error, None);
        assert_eq!(ev.answerer_rounds[1].error.as_deref(), Some("boom"));
        assert_eq!(ev.eval_failures.len(), 1);
        assert_eq!(ev.eval_failures[0].detail, "nope");

        let _ = std::fs::remove_file(&path);
    }
}
