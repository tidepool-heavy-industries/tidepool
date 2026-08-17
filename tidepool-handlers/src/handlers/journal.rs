//! Journal effect handler: a durable append-only run journal (PRD 20, S1-L5).
//!
//! One JSON line per `record` call — `{seq, kind, key, payload}` — appended
//! and flushed immediately. No rewrite or compaction code path exists. The
//! fold API below (`load_journal`/`last_by_key`) is for the swarm driver's
//! boot-time resume; nothing here wires it in.

use std::collections::HashMap;
use std::fmt;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tidepool_effect::dispatch::EffectContext;
use tidepool_effect::error::EffectError;
use tidepool_mcp::CapturedOutput;

// JournalReq + DescribeEffect + EffectHandler dispatch are generated from the
// single-source definition; only the handler struct and the per-verb method
// body below are hand-written.
tidepool_mcp::journal_effect_def!(crate::effect_glue::effect_rust_projection);

// ============================================================================
// Entries + the fold API (for the swarm driver's boot-time resume — not
// wired to anything here)
// ============================================================================

/// One durable journal entry, as it round-trips to/from a JSON line.
#[derive(Debug, Clone, PartialEq)]
pub struct JournalEntry {
    pub seq: u64,
    pub kind: String,
    pub key: String,
    pub payload: serde_json::Value,
}

impl JournalEntry {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "seq": self.seq,
            "kind": self.kind,
            "key": self.key,
            "payload": self.payload,
        })
    }

    fn from_json(v: &serde_json::Value) -> Result<Self, String> {
        let seq = v
            .get("seq")
            .and_then(serde_json::Value::as_u64)
            .ok_or("missing or non-integer \"seq\"")?;
        let kind = v
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .ok_or("missing or non-string \"kind\"")?
            .to_string();
        let key = v
            .get("key")
            .and_then(serde_json::Value::as_str)
            .ok_or("missing or non-string \"key\"")?
            .to_string();
        let payload = v.get("payload").cloned().ok_or("missing \"payload\"")?;
        Ok(JournalEntry {
            seq,
            kind,
            key,
            payload,
        })
    }
}

/// Why [`load_journal`] refused to load a journal file. A torn FINAL line
/// (the crash-mid-append case) is not one of these — it is skipped with a
/// `tracing::warn!` and left out of the returned entries, never an error.
#[derive(Debug)]
pub enum JournalLoadError {
    Io(std::io::Error),
    /// A line before the last one failed to parse. The journal is
    /// append-only and every write but the last is complete by
    /// construction, so this means real corruption — never silently
    /// absorbed the way a torn final line is.
    TornMidFile {
        path: PathBuf,
        line_no: usize,
        detail: String,
    },
}

impl fmt::Display for JournalLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JournalLoadError::Io(e) => write!(f, "journal I/O error: {e}"),
            JournalLoadError::TornMidFile {
                path,
                line_no,
                detail,
            } => write!(
                f,
                "journal {path:?} corrupted at line {line_no} (not the final line): {detail}"
            ),
        }
    }
}

impl std::error::Error for JournalLoadError {}

/// Load a journal file into its entries, in append order. A MISSING file is
/// an empty journal (`Ok(vec![])`), not an error — a run that has not
/// recorded anything yet has no file on disk. A torn FINAL line (a crash
/// mid-append) is skipped with a `tracing::warn!`; a torn line anywhere else
/// is loud (`Err(JournalLoadError::TornMidFile)`) — the journal is
/// append-only, so only the very last write can ever be incomplete.
pub fn load_journal(path: &Path) -> Result<Vec<JournalEntry>, JournalLoadError> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(JournalLoadError::Io(e)),
    };
    let lines: Vec<String> = BufReader::new(file)
        .lines()
        .collect::<std::io::Result<_>>()
        .map_err(JournalLoadError::Io)?;
    let last_idx = lines.len().saturating_sub(1);
    let mut entries = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let parsed = serde_json::from_str::<serde_json::Value>(line)
            .map_err(|e| e.to_string())
            .and_then(|v| JournalEntry::from_json(&v));
        match parsed {
            Ok(entry) => entries.push(entry),
            Err(detail) if i == last_idx => {
                tracing::warn!(
                    "journal {:?}: torn final line skipped (crash mid-append?): {}",
                    path,
                    detail
                );
            }
            Err(detail) => {
                return Err(JournalLoadError::TornMidFile {
                    path: path.to_path_buf(),
                    line_no: i,
                    detail,
                });
            }
        }
    }
    Ok(entries)
}

/// Fold entries down to the LAST record per `key` (later `seq` wins) — the
/// shape a boot-time resume wants: "what do I already know about this
/// branch/task". Not wired into anything here; the swarm driver injects this
/// at boot per PRD 20's "Persistence and resume" lean.
pub fn last_by_key(entries: &[JournalEntry]) -> HashMap<String, JournalEntry> {
    let mut out = HashMap::new();
    for entry in entries {
        out.insert(entry.key.clone(), entry.clone());
    }
    out
}

// ============================================================================
// The handler
// ============================================================================

#[derive(Clone)]
pub struct JournalHandler {
    path: PathBuf,
    // Monotonic for the lifetime of THIS handler instance, starting at 0 —
    // not derived from what is already on disk. A resumed run's continuity
    // across handler instances is a fold-API/driver concern, not a promise
    // this counter makes.
    seq: Arc<AtomicU64>,
}

impl JournalHandler {
    /// One journal file per run — the caller picks the path.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            seq: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Append one entry, flushed before returning. Parent directories are
    /// created as needed. No rewrite, truncate, or compaction path exists —
    /// this always opens in append mode.
    fn append(
        &self,
        kind: String,
        key: String,
        payload: serde_json::Value,
    ) -> Result<(), EffectError> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    EffectError::Handler(format!(
                        "journal: failed to create dir {:?}: {}",
                        parent, e
                    ))
                })?;
            }
        }
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let entry = JournalEntry {
            seq,
            kind,
            key,
            payload,
        };
        let mut line = serde_json::to_string(&entry.to_json())
            .map_err(|e| EffectError::Handler(format!("journal: serialize failed: {}", e)))?;
        line.push('\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| {
                EffectError::Handler(format!("journal: failed to open {:?}: {}", self.path, e))
            })?;
        // ONE `write_all` for the whole line (content + newline), not
        // `writeln!`/`write_fmt` — those can split into several `write_all`
        // calls on the underlying fd, and each individual `write()` syscall is
        // the unit POSIX guarantees is atomic against a concurrent O_APPEND
        // writer. Two calls sharing one line is exactly how a concurrent
        // recorder tears it.
        file.write_all(line.as_bytes())
            .map_err(|e| EffectError::Handler(format!("journal: write failed: {}", e)))?;
        file.flush()
            .map_err(|e| EffectError::Handler(format!("journal: flush failed: {}", e)))?;
        Ok(())
    }

    fn record_step(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        kind: String,
        key: String,
        payload: crate::effect_glue::JsonArg,
    ) -> Result<tidepool_effect::Response, EffectError> {
        self.append(kind, key, payload.0)?;
        cx.respond(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_file(label: &str) -> PathBuf {
        let pid = std::process::id();
        std::env::temp_dir().join(format!("tidepool_journal_{label}_{pid}.jsonl"))
    }

    fn tmp_dir(label: &str) -> PathBuf {
        let pid = std::process::id();
        std::env::temp_dir().join(format!("tidepool_journal_dir_{label}_{pid}"))
    }

    #[test]
    fn append_then_fold_roundtrips() {
        let path = tmp_file("roundtrip");
        let _ = std::fs::remove_file(&path);
        let h = JournalHandler::new(path.clone());

        h.append(
            "split".into(),
            "branch/a".into(),
            serde_json::json!({"n": 1}),
        )
        .unwrap();
        h.append(
            "outcome".into(),
            "branch/b".into(),
            serde_json::json!({"ok": true}),
        )
        .unwrap();

        let entries = load_journal(&path).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].kind, "split");
        assert_eq!(entries[0].key, "branch/a");
        assert_eq!(entries[0].payload, serde_json::json!({"n": 1}));
        assert_eq!(entries[1].kind, "outcome");
        assert_eq!(entries[1].key, "branch/b");
        assert_eq!(entries[1].payload, serde_json::json!({"ok": true}));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn seq_is_monotonic() {
        let path = tmp_file("seq");
        let _ = std::fs::remove_file(&path);
        let h = JournalHandler::new(path.clone());

        for i in 0..5 {
            h.append("k".into(), format!("key{i}"), serde_json::json!(i))
                .unwrap();
        }

        let entries = load_journal(&path).unwrap();
        let seqs: Vec<u64> = entries.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![0, 1, 2, 3, 4]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn torn_last_line_skipped_with_warning() {
        let path = tmp_file("torn");
        let _ = std::fs::remove_file(&path);
        let h = JournalHandler::new(path.clone());
        h.append("split".into(), "a".into(), serde_json::json!(1))
            .unwrap();
        h.append("split".into(), "b".into(), serde_json::json!(2))
            .unwrap();

        // Simulate a crash mid-append: keep the well-formed first line but
        // truncate the second partway through, as a torn write would leave it.
        let contents = std::fs::read_to_string(&path).unwrap();
        let first_newline = contents.find('\n').unwrap();
        let torn = format!(
            "{}\n{}",
            &contents[..first_newline],
            &contents[first_newline + 1..first_newline + 5]
        );
        std::fs::write(&path, torn).unwrap();

        let entries = load_journal(&path).unwrap();
        assert_eq!(
            entries.len(),
            1,
            "the torn final line must be skipped, not fatal"
        );
        assert_eq!(entries[0].key, "a");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn torn_mid_file_line_is_loud_not_absorbed() {
        let path = tmp_file("torn_mid");
        let _ = std::fs::remove_file(&path);
        // A well-formed final line preceded by a corrupted first line can
        // never happen from a real append-only crash, so it must be
        // reported, not silently dropped the way a torn final line is.
        std::fs::write(
            &path,
            "{not json\n{\"seq\":0,\"kind\":\"k\",\"key\":\"a\",\"payload\":1}\n",
        )
        .unwrap();

        let result = load_journal(&path);
        assert!(
            matches!(
                result,
                Err(JournalLoadError::TornMidFile { line_no: 0, .. })
            ),
            "expected TornMidFile at line 0, got {:?}",
            result
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn by_key_helper_returns_last_record_per_key() {
        let path = tmp_file("bykey");
        let _ = std::fs::remove_file(&path);
        let h = JournalHandler::new(path.clone());
        h.append("split".into(), "branch/a".into(), serde_json::json!(1))
            .unwrap();
        h.append("outcome".into(), "branch/a".into(), serde_json::json!(2))
            .unwrap();
        h.append("split".into(), "branch/b".into(), serde_json::json!(3))
            .unwrap();

        let entries = load_journal(&path).unwrap();
        let by_key = last_by_key(&entries);

        assert_eq!(by_key.len(), 2);
        assert_eq!(by_key["branch/a"].kind, "outcome");
        assert_eq!(by_key["branch/a"].payload, serde_json::json!(2));
        assert_eq!(by_key["branch/b"].kind, "split");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn parent_dir_creation_works() {
        let base = tmp_dir("parentdir");
        let _ = std::fs::remove_dir_all(&base);
        let path = base.join("nested").join("run.jsonl");
        let h = JournalHandler::new(path.clone());

        h.append("split".into(), "a".into(), serde_json::json!(1))
            .unwrap();

        assert!(path.exists());
        let entries = load_journal(&path).unwrap();
        assert_eq!(entries.len(), 1);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn missing_file_loads_as_empty() {
        let path = tmp_file("missing");
        let _ = std::fs::remove_file(&path);
        assert_eq!(load_journal(&path).unwrap(), vec![]);
    }

    /// A multi-threaded burst of records through CLONED handlers (sharing the
    /// same seq counter and the same path) must never tear a line: every
    /// append is one `write_all`, and POSIX guarantees one `write()` against
    /// an O_APPEND fd is atomic regardless of how many writers share it.
    #[test]
    fn concurrent_burst_through_cloned_handlers_yields_no_torn_lines() {
        let path = tmp_file("concurrent_burst");
        let _ = std::fs::remove_file(&path);
        let handler = JournalHandler::new(path.clone());

        const THREADS: usize = 8;
        const PER_THREAD: usize = 50;
        std::thread::scope(|scope| {
            for t in 0..THREADS {
                let h = handler.clone();
                scope.spawn(move || {
                    for i in 0..PER_THREAD {
                        h.append(
                            "burst".into(),
                            format!("t{t}-{i}"),
                            serde_json::json!({"t": t, "i": i}),
                        )
                        .unwrap();
                    }
                });
            }
        });

        let entries =
            load_journal(&path).expect("a torn line must never happen, so this must never error");
        assert_eq!(
            entries.len(),
            THREADS * PER_THREAD,
            "every append from every thread must survive as a complete, parseable line"
        );

        let mut seqs: Vec<u64> = entries.iter().map(|e| e.seq).collect();
        seqs.sort_unstable();
        seqs.dedup();
        assert_eq!(
            seqs.len(),
            THREADS * PER_THREAD,
            "the shared seq counter must not be raced past — no seq reused across threads"
        );

        let _ = std::fs::remove_file(&path);
    }
}
