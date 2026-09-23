//! Shared schema-independent JSONL append/read mechanism.
//!
//! Callers own row schemas, ordering, sequence numbers and synchronization across
//! writers. [`append_new_line`] owns the path and syncs its directory for strict
//! policies; [`write_line`] owns only a borrowed file and cannot establish its path.
//! Directory creation remains explicit: durable store owners use
//! `tidepool_atomic_write::create_dir_all_durable` before first publication.
//!
//! [`TailPolicy::Repair`] truncates an incomplete final row for a single-owner
//! journal and completes the delimiter of a valid unterminated row.
//! [`TailPolicy::Observe`] leaves artifacts untouched for read-only or
//! retain-first consumers. Malformed rows before another row are corruption and
//! fail under either policy. Complete rows rejected by the schema or version also
//! fail without mutation; tail repair never performs a version migration.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

/// How hard an append forces its bytes to storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncPolicy {
    /// No fsync at all — the degenerate/best-effort case
    /// (`JsonlObserver`'s transcript writes).
    None,
    /// Sync file data and metadata required to read it. Path-based append also
    /// syncs its containing directory; borrowed-file writes cannot do that.
    Data,
    /// Sync all file metadata as well as data, and the containing directory for
    /// path-based append. Newly created ancestry must be established separately.
    All,
}

fn sync(file: &File, policy: SyncPolicy) -> std::io::Result<()> {
    match policy {
        SyncPolicy::None => Ok(()),
        SyncPolicy::Data => file.sync_data(),
        SyncPolicy::All => file.sync_all(),
    }
}

/// Append one JSON row (WITHOUT newline), creating the file but not its parent.
/// `Data` and `All` sync the file and its containing directory on every append,
/// including retries after an earlier uncertain first-file publication. `None`
/// performs no sync. The caller owns durable parent creation and serializes writers.
///
/// An error can follow a visible append; it does not prove the row was absent and
/// must not cause an automatic retry. One `write_all` includes row and newline,
/// but may issue multiple OS writes; callers must not assume row atomicity across
/// concurrent writers from `O_APPEND` alone.
pub fn append_new_line(path: &Path, line: &str, sync_policy: SyncPolicy) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let mut row = line.to_string();
    row.push('\n');
    file.write_all(row.as_bytes())?;
    sync(&file, sync_policy)?;
    if sync_policy != SyncPolicy::None {
        tidepool_atomic_write::sync_parent_directory(path)
            .map_err(|error| std::io::Error::new(error.source.kind(), error))?;
    }
    Ok(())
}

/// Write `line` (WITHOUT a trailing newline) to an ALREADY-OPEN file — the
/// shape a writer that holds its own file handle for its whole lifetime wants
/// (`LogWriter`, `JsonlObserver`), as opposed to [`append_new_line`]'s
/// open-write-close-per-call shape. This syncs only the file: the caller must
/// establish new-file directory entries and newly created ancestry separately.
/// Errors can follow a partial or complete visible write, so they do not authorize
/// blind retry.
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
    /// Corruption or an unsupported complete row, never silently absorbed.
    /// `line_no` is one-based, matching text editors and command-line tools.
    MalformedRow {
        line_no: usize,
        detail: String,
    },
}

impl std::fmt::Display for JsonlReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JsonlReadError::Io(e) => write!(f, "{e}"),
            JsonlReadError::MalformedRow { line_no, detail } => {
                write!(f, "malformed row at line {line_no}: {detail}")
            }
        }
    }
}

impl std::error::Error for JsonlReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            JsonlReadError::Io(e) => Some(e),
            JsonlReadError::MalformedRow { .. } => None,
        }
    }
}

/// How [`read_tail`] treats a torn final row on disk — see the module doc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TailPolicy {
    /// Truncate an incomplete row, or complete a valid final row's delimiter.
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

/// Read rows in file order with caller-owned schema validation. A missing file
/// reads as an empty journal. Only a malformed unterminated final row can be
/// repaired or skipped; a newline-terminated row or complete
/// JSON value rejected by the caller is corruption, even at EOF.
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
    let not_final = |bad: (usize, String, u64), next: usize| JsonlReadError::MalformedRow {
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
    let mut unterminated = false;
    loop {
        buf.clear();
        let n = reader
            .read_until(b'\n', &mut buf)
            .map_err(JsonlReadError::Io)?;
        if n == 0 {
            break;
        }
        lineno += 1;
        unterminated = !buf.ends_with(b"\n");
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
                        if buf.ends_with(b"\n")
                            || serde_json::from_str::<serde_json::Value>(l).is_ok()
                        {
                            return Err(JsonlReadError::MalformedRow {
                                line_no: lineno,
                                detail: reason,
                            });
                        }
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
                if buf.ends_with(b"\n") {
                    return Err(JsonlReadError::MalformedRow {
                        line_no: lineno,
                        detail: format!("invalid utf-8: {err}"),
                    });
                }
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
        if unterminated && policy == TailPolicy::Repair {
            // A crash can leave the complete JSON payload without its newline.
            // Preserve the accepted row and separate it from the next append.
            let mut repair_file = OpenOptions::new()
                .append(true)
                .open(path)
                .map_err(JsonlReadError::Io)?;
            repair_file.write_all(b"\n").map_err(JsonlReadError::Io)?;
            repair_file.sync_all().map_err(JsonlReadError::Io)?;
        }
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
    fn valid_unterminated_row_is_separated_before_append() {
        let path = tmp_file("missing_delimiter");
        std::fs::write(&path, b"1\n2").unwrap();
        let (entries, torn) = read_tail(&path, parse_u64, TailPolicy::Observe).unwrap();
        assert_eq!(entries, vec![1, 2]);
        assert!(torn.is_none());
        assert_eq!(std::fs::read(&path).unwrap(), b"1\n2");

        let (entries, torn) = read_tail(&path, parse_u64, TailPolicy::Repair).unwrap();
        assert_eq!(entries, vec![1, 2]);
        assert!(torn.is_none());
        assert_eq!(std::fs::read(&path).unwrap(), b"1\n2\n");
        append_new_line(&path, "3", SyncPolicy::All).unwrap();
        let (entries, torn) = read_tail(&path, parse_u64, TailPolicy::Repair).unwrap();
        assert_eq!(entries, vec![1, 2, 3]);
        assert!(torn.is_none());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn append_then_read_roundtrips() {
        let path = tmp_file("roundtrip");
        // best-effort: test cleanup of a temp path.
        std::fs::remove_file(&path).ok();
        append_new_line(&path, "1", SyncPolicy::All).unwrap();
        append_new_line(&path, "2", SyncPolicy::Data).unwrap();
        append_new_line(&path, "3", SyncPolicy::None).unwrap();

        let (entries, torn) = read_tail(&path, parse_u64, TailPolicy::Repair).unwrap();
        assert_eq!(entries, vec![1, 2, 3]);
        assert!(torn.is_none());
        // best-effort: test cleanup of a temp path.
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn missing_file_reads_as_empty() {
        let path = tmp_file("missing");
        // best-effort: test cleanup of a temp path.
        std::fs::remove_file(&path).ok();
        let (entries, torn) = read_tail::<u64>(&path, parse_u64, TailPolicy::Repair).unwrap();
        assert_eq!(entries, Vec::<u64>::new());
        assert!(torn.is_none());
    }

    #[test]
    fn torn_final_line_under_repair_is_truncated_on_disk() {
        let path = tmp_file("torn_final_repair");
        // best-effort: test cleanup of a temp path.
        std::fs::remove_file(&path).ok();
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

        // best-effort: test cleanup of a temp path.
        std::fs::remove_file(&path).ok();
    }

    /// `TailPolicy::Observe` reports the same torn row but never mutates the
    /// file — the segmented-journal invariant `read_tail`'s module doc
    /// describes (a foreign segment is never rewritten, truncated, or
    /// deleted).
    #[test]
    fn torn_final_line_under_observe_is_reported_but_left_in_place() {
        let path = tmp_file("torn_final_observe");
        // best-effort: test cleanup of a temp path.
        std::fs::remove_file(&path).ok();
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

        // best-effort: test cleanup of a temp path.
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn torn_mid_file_line_is_loud_not_absorbed() {
        let path = tmp_file("torn_mid");
        // best-effort: test cleanup of a temp path.
        std::fs::remove_file(&path).ok();
        std::fs::write(&path, b"not-a-number\n2\n").unwrap();

        let err = read_tail(&path, parse_u64, TailPolicy::Repair).unwrap_err();
        assert!(
            matches!(err, JsonlReadError::MalformedRow { line_no: 1, .. }),
            "expected MalformedRow at line 1, got {err:?}"
        );
        // best-effort: test cleanup of a temp path.
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn complete_invalid_final_rows_are_never_repaired() {
        let path = tmp_file("complete_invalid_final");
        for bytes in [b"1\nwrong\n".as_slice(), b"1\n{}\n", b"1\n{}", b"1\n\xff\n"] {
            for policy in [TailPolicy::Repair, TailPolicy::Observe] {
                std::fs::write(&path, bytes).unwrap();
                assert!(matches!(
                    read_tail(&path, parse_u64, policy),
                    Err(JsonlReadError::MalformedRow { line_no: 2, .. })
                ));
                assert_eq!(std::fs::read(&path).unwrap(), bytes);
            }
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn write_line_to_open_file_appends() {
        let path = tmp_file("write_line");
        // best-effort: test cleanup of a temp path.
        std::fs::remove_file(&path).ok();
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
        // best-effort: test cleanup of a temp path.
        std::fs::remove_file(&path).ok();
    }
}
