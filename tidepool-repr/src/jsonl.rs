//! A generic durable JSONL append/read primitive — the shared mechanism
//! behind every hand-rolled JSONL journal/log/observer in the workspace
//! (`tidepool-worktree::journal::EventJournal`,
//! `tidepool-handlers::handlers::journal::{JournalHandler, load_journal}`,
//! `tidepool-harness::log::writer::LogWriter`,
//! `tidepool-harness::selfharness::persistence::JsonlObserver`).
//!
//! ## Design note (dedupe-io-substrate consolidation)
//!
//! What lives here is MECHANISM only — how a line is durably appended and how
//! a torn final line is read back — never a row SCHEMA, an envelope (sequence
//! numbers, cursors, segment ordinals), or a durability POLICY (whether/how a
//! given consumer fsyncs). Each consumer keeps its own row type, its own
//! seq/cursor bookkeeping, and its own choice of [`SyncPolicy`]:
//!
//! - `EventJournal` — row `JournalEntry{cursor,event_id,event,recorded_at_ms}`,
//!   cursor derived from the last entry, [`SyncPolicy::All`], no in-process
//!   lock needed (`&mut self` is already exclusive).
//! - `JournalHandler`/`load_journal` — row `JournalEntry{seq,kind,key,payload}`,
//!   seq composed from a segment ordinal + a per-handler local counter,
//!   [`SyncPolicy::Data`], appends serialized under the handler's OWN
//!   `Arc<Mutex<()>>` (this module provides no locking of its own — a single
//!   owning `&mut self` needs none, and a `Clone`-able handler already has
//!   its own lock, so baking a second one in here would just be a second lock
//!   to keep in sync with the first).
//! - `LogWriter` — a header-first envelope, `EventRecord{seq,event}` rows,
//!   [`SyncPolicy::All`], `create_new`-only (never overwrites an existing
//!   run's log), holds its own [`File`] open for the writer's lifetime.
//! - `JsonlObserver` — the degenerate sink: no envelope, no fsync at all
//!   ([`SyncPolicy::None`]) by design — a transcript observer's own writes are
//!   best-effort, never load-bearing for correctness the way a journal's are.
//!
//! ## Tail policy: `Repair` vs `Observe` — a real, per-consumer choice
//!
//! A malformed final line is the torn-write shape (a crash mid-`write`) and
//! is always FORGIVEN — it never fails the read — but what happens to the
//! bytes on disk is a genuine per-consumer policy choice, [`TailPolicy`]:
//!
//! - **`Repair`** — truncate the file to the last good row, so a later append
//!   lands after good data, not after garbage. Right for a single-owner file
//!   nothing else ever reads or writes (`EventJournal`, where `&mut self` is
//!   the only handle that will ever touch this path again).
//! - **`Observe`** — leave the file byte-for-byte untouched; the torn row is
//!   still reported (so the caller can warn) but never truncated away. Right
//!   for `load_journal`, which folds SEGMENT files it does not own — the
//!   segmented journal's "retain first, nothing ever rewrites, truncates, or
//!   deletes a segment" invariant (`tidepool-harness::selfharness::resume`)
//!   extends to a torn tail too: a later boot redoes the lost row into its
//!   OWN fresh segment rather than editing a segment some other process
//!   (possibly still alive, possibly the subject of a later forensic read)
//!   exclusively claimed.
//!
//! This was flagged as an open design point in the duplication survey that
//! motivated this consolidation ("whether the shared reader always repairs
//! the tail or exposes `RepairTail` versus `ObserveOnly`"); testing against
//! `resume.rs`'s own pinned invariant
//! (`a_segment_with_a_torn_tail_never_poisons_a_later_boot`) settled it in
//! favor of keeping BOTH — a single global policy would have either corrupted
//! `EventJournal`'s single-writer-file assumption's safety margin (mutating
//! under `Observe` everywhere) or broken the segmented design (mutating under
//! `Repair` everywhere).
//!
//! A malformed line ANYWHERE ELSE — not the final line — is loud regardless
//! of policy: the append-only invariant means only the very last line can
//! ever be incomplete, so an earlier bad line is real corruption and is never
//! silently absorbed.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

/// How hard an append forces its bytes to storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncPolicy {
    /// No fsync at all — the degenerate/best-effort case
    /// (`JsonlObserver`'s transcript writes).
    None,
    /// `File::sync_data` — durable, skipping the metadata sync that doesn't
    /// affect readability.
    Data,
    /// `File::sync_all` — durable, including metadata.
    All,
}

fn sync(file: &File, policy: SyncPolicy) -> std::io::Result<()> {
    match policy {
        SyncPolicy::None => Ok(()),
        SyncPolicy::Data => file.sync_data(),
        SyncPolicy::All => file.sync_all(),
    }
}

/// Append `line` (one JSON row, WITHOUT a trailing newline) to `path`,
/// creating the file if needed (but NOT its parent directory — a missing
/// parent is a real failure to report, not to silently paper over by
/// recreating it; a caller that wants mkdir-p-on-append, as
/// `JournalHandler` does, calls `std::fs::create_dir_all` itself first),
/// opened in append mode. ONE `write_all` for the whole row plus its
/// newline, not two syscalls — so a burst of appends can never land
/// interleaved under `O_APPEND` (see `SyncPolicy`'s callers for why this
/// matters more or less depending on their own locking).
///
/// Serialization across CONCURRENT writers (multiple threads, multiple
/// clones of one handler) is the CALLER's job — wrap this in the caller's own
/// lock, exactly as `JournalHandler` already does. A single owning `&mut
/// self` (as `EventJournal` has) is exclusive by construction and needs none.
pub fn append_new_line(path: &Path, line: &str, sync_policy: SyncPolicy) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let mut row = line.to_string();
    row.push('\n');
    file.write_all(row.as_bytes())?;
    sync(&file, sync_policy)
}

/// Write `line` (WITHOUT a trailing newline) to an ALREADY-OPEN file — the
/// shape a writer that holds its own file handle for its whole lifetime wants
/// (`LogWriter`, `JsonlObserver`), as opposed to [`append_new_line`]'s
/// open-write-close-per-call shape.
pub fn write_line(file: &mut File, line: &str, sync_policy: SyncPolicy) -> std::io::Result<()> {
    let mut row = line.to_string();
    row.push('\n');
    file.write_all(row.as_bytes())?;
    sync(file, sync_policy)
}

/// Why [`read_tail`] refused to return entries.
#[derive(Debug)]
pub enum JsonlReadError {
    Io(std::io::Error),
    /// A malformed row before the last line — never the torn-write shape, so
    /// never silently absorbed. `line_no` is ONE-based (an operator's "line
    /// 1", matching what a text editor or `sed -n '<n>p'` shows).
    TornMidFile {
        line_no: usize,
        detail: String,
    },
}

impl std::fmt::Display for JsonlReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JsonlReadError::Io(e) => write!(f, "{e}"),
            JsonlReadError::TornMidFile { line_no, detail } => write!(
                f,
                "malformed row at line {line_no} is followed by more data — not the final line: \
                 {detail}"
            ),
        }
    }
}

impl std::error::Error for JsonlReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            JsonlReadError::Io(e) => Some(e),
            JsonlReadError::TornMidFile { .. } => None,
        }
    }
}

/// How [`read_tail`] treats a torn final row on disk — see the module doc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TailPolicy {
    /// Truncate the file to the last good row.
    Repair,
    /// Leave the file byte-for-byte untouched.
    Observe,
}

/// A torn final row was found — see [`TailPolicy`] for whether it was
/// truncated away or left in place.
#[derive(Debug, Clone)]
pub struct TornTail {
    /// ONE-based line number of the torn row.
    pub line_no: usize,
    /// Why that row failed to parse — from `parse`'s own `Err`.
    pub reason: String,
}

/// Read every row in `path`, in file order, parsing each with the caller's
/// own `parse` (so this stays schema-agnostic: a caller like `load_journal`
/// applies its own field-level validation on top of the raw JSON, exactly as
/// it did before this consolidation). A torn FINAL row is always forgiven —
/// see the module doc for what `policy` does to the bytes on disk. A missing
/// file reads as `Ok((vec![], None))` — an empty journal, not an error.
///
/// "Malformed" is judged by `parse`'s own verdict — a syntactically valid
/// JSON line that doesn't match the caller's expected shape is exactly as
/// malformed as invalid JSON, matching what every consolidated consumer
/// already did (both `EventJournal` and `load_journal` treated a
/// wrong-shaped row as bad, not just a syntactically invalid one).
pub fn read_tail<T>(
    path: &Path,
    parse: impl Fn(&str) -> Result<T, String>,
    policy: TailPolicy,
) -> Result<(Vec<T>, Option<TornTail>), JsonlReadError> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), None)),
        Err(e) => return Err(JsonlReadError::Io(e)),
    };
    let mut reader = BufReader::new(file);

    let mut entries = Vec::new();
    // A malformed row is recoverable ONLY as the final row — see the module
    // doc. Bytes are counted (hence `read_until`, not `lines()`) so the
    // eventual truncation lands exactly at the start of the bad row.
    let mut pending_bad: Option<(usize, String, u64)> = None;
    let not_final = |bad: (usize, String, u64), next: usize| JsonlReadError::TornMidFile {
        line_no: bad.0,
        detail: format!(
            "a corrupted row in the middle of the file is not a torn write and must not be \
             silently skipped (followed by line {next}): {}",
            bad.1
        ),
    };

    let mut buf: Vec<u8> = Vec::new();
    let mut offset: u64 = 0;
    let mut lineno: usize = 0;
    loop {
        buf.clear();
        let n = reader
            .read_until(b'\n', &mut buf)
            .map_err(JsonlReadError::Io)?;
        if n == 0 {
            break;
        }
        lineno += 1;
        let line_start = offset;
        offset += n as u64;
        match std::str::from_utf8(&buf) {
            Ok(l) => {
                if l.trim().is_empty() {
                    if let Some(bad) = pending_bad.take() {
                        return Err(not_final(bad, lineno));
                    }
                    continue;
                }
                match parse(l) {
                    Ok(entry) => {
                        if let Some(bad) = pending_bad.take() {
                            return Err(not_final(bad, lineno));
                        }
                        entries.push(entry);
                    }
                    Err(reason) => {
                        if let Some(bad) = pending_bad.take() {
                            return Err(not_final(bad, lineno));
                        }
                        pending_bad = Some((lineno, reason, line_start));
                    }
                }
            }
            // Non-UTF-8 bytes are the same torn-write shape as a truncated
            // JSON row, so they get the same final-row-only treatment.
            Err(err) => {
                if let Some(bad) = pending_bad.take() {
                    return Err(not_final(bad, lineno));
                }
                pending_bad = Some((lineno, format!("invalid utf-8: {err}"), line_start));
            }
        }
    }

    // Reached EOF with a bad row outstanding: it WAS the final row, so this
    // is the recoverable torn write. Under `Repair`, the file is TRUNCATED to
    // the last good row so the next append lands after good data, not after
    // garbage; under `Observe`, the bytes are left exactly as found.
    let torn = if let Some((lineno, reason, bad_start)) = pending_bad {
        if policy == TailPolicy::Repair {
            let repair_file = OpenOptions::new()
                .write(true)
                .open(path)
                .map_err(JsonlReadError::Io)?;
            repair_file.set_len(bad_start).map_err(JsonlReadError::Io)?;
            repair_file.sync_all().map_err(JsonlReadError::Io)?;
        }
        Some(TornTail {
            line_no: lineno,
            reason,
        })
    } else {
        None
    };

    Ok((entries, torn))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_file(label: &str) -> std::path::PathBuf {
        let pid = std::process::id();
        std::env::temp_dir().join(format!(
            "tidepool_repr_jsonl_{label}_{pid}_{}.jsonl",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    fn parse_u64(l: &str) -> Result<u64, String> {
        // Mirrors real callers (`serde_json::from_str`), which tolerate
        // trailing whitespace after the value — `l` still carries the
        // trailing newline `read_until` includes.
        l.trim().parse::<u64>().map_err(|e| e.to_string())
    }

    #[test]
    fn append_then_read_roundtrips() {
        let path = tmp_file("roundtrip");
        let _ = std::fs::remove_file(&path);
        append_new_line(&path, "1", SyncPolicy::All).unwrap();
        append_new_line(&path, "2", SyncPolicy::Data).unwrap();
        append_new_line(&path, "3", SyncPolicy::None).unwrap();

        let (entries, torn) = read_tail(&path, parse_u64, TailPolicy::Repair).unwrap();
        assert_eq!(entries, vec![1, 2, 3]);
        assert!(torn.is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_reads_as_empty() {
        let path = tmp_file("missing");
        let _ = std::fs::remove_file(&path);
        let (entries, torn) = read_tail::<u64>(&path, parse_u64, TailPolicy::Repair).unwrap();
        assert_eq!(entries, Vec::<u64>::new());
        assert!(torn.is_none());
    }

    #[test]
    fn torn_final_line_under_repair_is_truncated_on_disk() {
        let path = tmp_file("torn_final_repair");
        let _ = std::fs::remove_file(&path);
        append_new_line(&path, "1", SyncPolicy::All).unwrap();
        append_new_line(&path, "2", SyncPolicy::All).unwrap();
        // Simulate a crash mid-write: append a truncated (non-numeric,
        // non-parseable) partial final row with no trailing newline.
        {
            use std::io::Write as _;
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(b"3x").unwrap();
        }

        let (entries, torn) = read_tail(&path, parse_u64, TailPolicy::Repair).unwrap();
        assert_eq!(entries, vec![1, 2]);
        let torn = torn.expect("a torn final row must be reported");
        assert_eq!(torn.line_no, 3);

        // The file itself must now be truncated to the last good row: a
        // fresh read (and a fresh append) sees no trace of the torn row.
        let (entries_after, torn_after) = read_tail(&path, parse_u64, TailPolicy::Repair).unwrap();
        assert_eq!(entries_after, vec![1, 2]);
        assert!(torn_after.is_none());

        append_new_line(&path, "4", SyncPolicy::All).unwrap();
        let (entries_final, _) = read_tail(&path, parse_u64, TailPolicy::Repair).unwrap();
        assert_eq!(entries_final, vec![1, 2, 4]);

        let _ = std::fs::remove_file(&path);
    }

    /// `TailPolicy::Observe` reports the same torn row but never mutates the
    /// file — the segmented-journal invariant `read_tail`'s module doc
    /// describes (a foreign segment is never rewritten, truncated, or
    /// deleted).
    #[test]
    fn torn_final_line_under_observe_is_reported_but_left_in_place() {
        let path = tmp_file("torn_final_observe");
        let _ = std::fs::remove_file(&path);
        append_new_line(&path, "1", SyncPolicy::All).unwrap();
        {
            use std::io::Write as _;
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(b"2x").unwrap();
        }
        let before = std::fs::read(&path).unwrap();

        let (entries, torn) = read_tail(&path, parse_u64, TailPolicy::Observe).unwrap();
        assert_eq!(entries, vec![1]);
        let torn = torn.expect("a torn final row must still be reported under Observe");
        assert_eq!(torn.line_no, 2);

        let after = std::fs::read(&path).unwrap();
        assert_eq!(before, after, "Observe must never mutate the file");

        // Reading again yields the identical result — Observe is idempotent,
        // not a one-shot repair.
        let (entries2, torn2) = read_tail(&path, parse_u64, TailPolicy::Observe).unwrap();
        assert_eq!(entries2, vec![1]);
        assert!(torn2.is_some());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn torn_mid_file_line_is_loud_not_absorbed() {
        let path = tmp_file("torn_mid");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, b"not-a-number\n2\n").unwrap();

        let err = read_tail(&path, parse_u64, TailPolicy::Repair).unwrap_err();
        assert!(
            matches!(err, JsonlReadError::TornMidFile { line_no: 1, .. }),
            "expected TornMidFile at line 1, got {err:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_line_to_open_file_appends() {
        let path = tmp_file("write_line");
        let _ = std::fs::remove_file(&path);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        write_line(&mut file, "1", SyncPolicy::All).unwrap();
        write_line(&mut file, "2", SyncPolicy::None).unwrap();
        drop(file);

        let (entries, _) = read_tail(&path, parse_u64, TailPolicy::Repair).unwrap();
        assert_eq!(entries, vec![1, 2]);
        let _ = std::fs::remove_file(&path);
    }
}
