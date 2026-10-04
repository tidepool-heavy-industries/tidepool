use exomonad_worktree::git::GitCli;
use std::path::PathBuf;
use tidepool_bridge_effects::{GitCommit, GitCommitDeltas, GitFileDelta, GitStatusEntry};

// ============================================================================
// Tag 7: Git (read-only repository queries)
// ============================================================================

// GitReq + DescribeEffect + EffectHandler dispatch are generated from the
// single-source definition; only the handler struct and the per-verb method
// bodies below are hand-written.
tidepool_mcp::git_effect_def!(crate::effect_glue::effect_rust_projection);

#[derive(Clone)]
pub struct GitHandler {
    root: PathBuf,
}

impl GitHandler {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Run read-only Git queries through the shared subprocess owner.
    fn run_git(&self, args: &[&str]) -> Result<Vec<u8>, GitError> {
        GitCli::new()
            .stdout_bytes(&self.root, args)
            .map_err(|error| match error {
                exomonad_worktree::error::WorktreeError::GitFailure(receipt) => {
                    GitError::GitFailed(
                        receipt.exit_code.unwrap_or(-1) as i64,
                        receipt.stderr.trim().to_string(),
                    )
                }
                other => GitError::GitFailed(-1, other.to_string()),
            })
    }

    /// Reject option-shaped input before passing a revspec to Git.
    fn validate_revspec(rev: &str) -> Result<(), GitError> {
        if rev.starts_with('-') {
            return Err(GitError::GitBadRevspec(format!(
                "revspec must not start with '-' (looks like a git option): {:?}",
                rev
            )));
        }
        Ok(())
    }

    fn malformed(detail: impl Into<String>) -> GitError {
        GitError::GitMalformedOutput(detail.into())
    }

    fn path_text(bytes: &[u8]) -> Result<String, GitError> {
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| GitError::GitNonUtf8Path(format!("path bytes: {bytes:02x?}")))
    }

    fn nul_fields(bytes: &[u8]) -> Result<Vec<&[u8]>, GitError> {
        if bytes.is_empty() {
            return Ok(Vec::new());
        }
        if bytes.last() != Some(&0) {
            return Err(Self::malformed("NUL-delimited output is truncated"));
        }
        Ok(bytes[..bytes.len() - 1].split(|byte| *byte == 0).collect())
    }

    fn parse_status_output(bytes: &[u8]) -> Result<Vec<GitStatusEntry>, GitError> {
        let fields = Self::nul_fields(bytes)?;
        let mut entries = Vec::new();
        let mut index = 0;
        while index < fields.len() {
            let record = fields[index];
            if record.len() < 4 || record[2] != b' ' {
                return Err(Self::malformed("invalid porcelain v1 status record"));
            }
            let valid = |byte| b" MADRCU?!T".contains(&byte);
            if !valid(record[0]) || !valid(record[1]) || (record[0] == b' ' && record[1] == b' ') {
                return Err(Self::malformed("invalid porcelain v1 status code"));
            }
            let path = Self::path_text(&record[3..])?;
            if path.is_empty() {
                return Err(Self::malformed(
                    "porcelain v1 status record has an empty path",
                ));
            }
            entries.push(GitStatusEntry {
                path,
                state: String::from_utf8(vec![record[0], record[1]]).unwrap(),
            });
            index += 1;
            // With -z, a rename/copy record is followed by the source path;
            // the path embedded in the status record is the destination.
            if record[0] == b'R' || record[1] == b'R' || record[0] == b'C' || record[1] == b'C' {
                if index >= fields.len() || fields[index].is_empty() {
                    return Err(Self::malformed(
                        "porcelain v1 rename is missing its source path",
                    ));
                }
                let _source = Self::path_text(fields[index])?;
                index += 1;
            }
        }
        Ok(entries)
    }

    fn parse_count(raw: &[u8]) -> Result<(i64, bool), GitError> {
        if raw == b"-" {
            return Ok((0, true));
        }
        if raw.is_empty() || !raw.iter().all(u8::is_ascii_digit) {
            return Err(Self::malformed("numstat contains an invalid line count"));
        }
        let text = std::str::from_utf8(raw).expect("ASCII digits are UTF-8");
        let value = text
            .parse::<i64>()
            .map_err(|_| Self::malformed("numstat line count is out of range"))?;
        Ok((value, false))
    }

    fn parse_numstat_fields(
        fields: &[&[u8]],
        index: &mut usize,
        transport_newline: bool,
    ) -> Result<GitFileDelta, GitError> {
        let field = fields[*index];
        let row = if transport_newline {
            Self::first_path_field(field)?
        } else {
            field
        };
        let first = row
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| Self::malformed("numstat row is missing its first tab"))?;
        let rest = &row[first + 1..];
        let second = rest
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| Self::malformed("numstat row is missing its second tab"))?;
        let (adds, adds_binary) = Self::parse_count(&row[..first])?;
        let (dels, dels_binary) = Self::parse_count(&rest[..second])?;
        if adds_binary != dels_binary {
            return Err(Self::malformed("numstat binary counts must both be '-'"));
        }
        let raw_path = &rest[second + 1..];
        *index += 1;
        let path = if raw_path.is_empty() {
            // NUL numstat represents a rename as an empty path field followed
            // by the old and new path fields.
            if *index + 1 >= fields.len()
                || fields[*index].is_empty()
                || fields[*index + 1].is_empty()
            {
                return Err(Self::malformed("numstat rename is missing a path"));
            }
            let _old = Self::path_text(fields[*index])?;
            let new = Self::path_text(fields[*index + 1])?;
            *index += 2;
            new
        } else {
            Self::path_text(raw_path)?
        };
        if path.is_empty() {
            return Err(Self::malformed("numstat row has an empty path"));
        }
        Ok(GitFileDelta {
            path,
            adds,
            dels,
            binary: adds_binary,
        })
    }

    fn parse_numstat_output(bytes: &[u8]) -> Result<Vec<GitFileDelta>, GitError> {
        let fields = Self::nul_fields(bytes)?;
        let mut deltas = Vec::new();
        let mut index = 0;
        while index < fields.len() {
            if fields[index].is_empty() {
                return Err(Self::malformed("numstat output contains an empty row"));
            }
            deltas.push(Self::parse_numstat_fields(&fields, &mut index, false)?);
        }
        Ok(deltas)
    }

    fn parse_log_header(fields: &[&[u8]], index: usize) -> Result<(GitCommit, usize), GitError> {
        if index + 3 >= fields.len() {
            return Err(Self::malformed(
                "git log output has a truncated commit header",
            ));
        }
        let sha = fields[index];
        let date = fields[index + 3];
        if !matches!(sha.len(), 40 | 64)
            || !sha.iter().all(u8::is_ascii_hexdigit)
            || date.len() < 20
            || date.get(4) != Some(&b'-')
            || date.get(7) != Some(&b'-')
        {
            return Err(Self::malformed(
                "git log output has an invalid commit header",
            ));
        }
        let decode = |bytes: &[u8]| {
            std::str::from_utf8(bytes)
                .map(str::to_owned)
                .map_err(|_| Self::malformed("git log metadata is not UTF-8"))
        };
        Ok((
            GitCommit {
                sha: decode(sha)?,
                subject: decode(fields[index + 1])?,
                author: decode(fields[index + 2])?,
                date: decode(date)?,
                files: Vec::new(),
            },
            index + 4,
        ))
    }

    fn next_commit_header(
        fields: &[&[u8]],
        index: &mut usize,
    ) -> Result<Option<(GitCommit, usize)>, GitError> {
        let boundary = *index;
        while *index < fields.len() && fields[*index].is_empty() {
            *index += 1;
        }
        if *index == fields.len() {
            return if boundary == 0 {
                Err(Self::malformed(
                    "git log output has a truncated first commit header",
                ))
            } else {
                Ok(None)
            };
        }
        if boundary == 0 && (*index != 1 || !fields[0].is_empty()) {
            return Err(Self::malformed(
                "git log output is missing its format boundary",
            ));
        }
        Self::parse_log_header(fields, *index).map(Some)
    }

    fn first_path_field<'a>(field: &'a [u8]) -> Result<&'a [u8], GitError> {
        field
            .strip_prefix(b"\n")
            .ok_or_else(|| Self::malformed("git log output is missing its transport newline"))
    }

    fn parse_log_output(bytes: &[u8]) -> Result<Vec<GitCommit>, GitError> {
        let fields = Self::nul_fields(bytes)?;
        if fields.is_empty() {
            return Ok(Vec::new());
        }
        if !fields[0].is_empty() {
            return Err(Self::malformed(
                "git log output is missing its format boundary",
            ));
        }

        let mut commits = Vec::new();
        let mut index = 0;
        loop {
            let Some((mut commit, next)) = Self::next_commit_header(&fields, &mut index)? else {
                break;
            };
            index = next;
            let mut first_path = true;
            while index < fields.len() && !fields[index].is_empty() {
                let field = if first_path {
                    first_path = false;
                    Self::first_path_field(fields[index])?
                } else {
                    fields[index]
                };
                commit.files.push(Self::path_text(field)?);
                index += 1;
            }
            commits.push(commit);
        }
        Ok(commits)
    }

    fn parse_single_commit(bytes: &[u8], revspec: &str) -> Result<GitCommit, GitError> {
        Self::parse_log_output(bytes)?
            .into_iter()
            .next()
            .ok_or_else(|| {
                GitError::GitMalformedOutput(format!(
                    "gitShow returned no commit for revspec '{revspec}'"
                ))
            })
    }

    fn parse_log_numstat_output(bytes: &[u8]) -> Result<Vec<GitCommitDeltas>, GitError> {
        let fields = Self::nul_fields(bytes)?;
        if fields.is_empty() {
            return Ok(Vec::new());
        }
        if !fields[0].is_empty() {
            return Err(Self::malformed(
                "git log output is missing its format boundary",
            ));
        }

        let mut rows = Vec::new();
        let mut index = 0;
        loop {
            let Some((mut commit, next)) = Self::next_commit_header(&fields, &mut index)? else {
                break;
            };
            index = next;
            let mut deltas = Vec::new();
            let mut first_stat = true;
            while index < fields.len() && !fields[index].is_empty() {
                let delta = Self::parse_numstat_fields(&fields, &mut index, first_stat)?;
                first_stat = false;
                commit.files.push(delta.path.clone());
                deltas.push(delta);
            }
            rows.push(GitCommitDeltas { commit, deltas });
        }
        Ok(rows)
    }
}

impl GitHandler {
    // Errors-tagged verbs: total in `GitError`, no `cx` — the dispatch arm
    // wraps the `Result` via `cx.respond` (Ok→Right, Err→Left). See #335.
    fn git_log(&mut self, n: i64) -> Result<Vec<GitCommit>, GitError> {
        let n_str = n.to_string();
        let output = self.run_git(&[
            "log",
            "-n",
            &n_str,
            "--format=%x00%H%x00%s%x00%an%x00%cI",
            "--name-only",
            "-z",
        ])?;
        Self::parse_log_output(&output)
    }

    fn git_status(&mut self) -> Result<Vec<GitStatusEntry>, GitError> {
        let output = self.run_git(&["status", "--porcelain=v1", "-z"])?;
        Self::parse_status_output(&output)
    }

    fn git_diff_stat(&mut self, rev: String) -> Result<Vec<GitFileDelta>, GitError> {
        Self::validate_revspec(&rev)?;
        // Trailing `--` closes the pathspec boundary so `rev` can never be
        // reinterpreted as (or followed by) an option, even defensively.
        let output = self.run_git(&["diff", "--numstat", "-M", "-z", &rev, "--"])?;
        Self::parse_numstat_output(&output)
    }

    fn git_show(&mut self, rev: String) -> Result<GitCommit, GitError> {
        Self::validate_revspec(&rev)?;
        let output = self.run_git(&[
            "log",
            "-n",
            "1",
            &rev,
            "--format=%x00%H%x00%s%x00%an%x00%cI",
            "--name-only",
            "-z",
            "--",
        ])?;
        Self::parse_single_commit(&output, &rev)
    }

    /// The bulk git-history substrate: last N commits, each paired with its
    /// own numstat deltas, in ONE subprocess (`-M` enables rename detection
    /// so the parser's `{old => new}`/`old => new` handling is exercised).
    fn git_log_numstat(&mut self, n: i64) -> Result<Vec<GitCommitDeltas>, GitError> {
        let n_str = n.to_string();
        let output = self.run_git(&[
            "log",
            "-n",
            &n_str,
            "--format=%x00%H%x00%s%x00%an%x00%cI",
            "--numstat",
            "-M",
            "-z",
        ])?;
        Self::parse_log_numstat_output(&output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use tidepool_bridge::HaskellValue;
    use tidepool_bridge::{FromHaskell, ToHaskell};
    use tidepool_effect::dispatch::{EffectContext, EffectHandler};
    use tidepool_mcp::CapturedOutput;

    /// Peel one `Right`/`Left` Con layer off a #335 errors-tagged response,
    /// panicking with the decoded `GitError` on `Left`. Matches by reference
    /// (`HaskellValue` has a manual `Drop` impl, so it can't be partially moved out
    /// of) and clones just the field it needs.
    fn unwrap_right(val: HaskellValue, table: &tidepool_repr::DataConTable) -> HaskellValue {
        match &val {
            HaskellValue::Con(id, fields) if table.name_of(*id).unwrap() == "Right" => {
                fields[0].clone()
            }
            HaskellValue::Con(id, fields) if table.name_of(*id).unwrap() == "Left" => {
                let err: GitError = FromHaskell::from_value(&fields[0], table).unwrap();
                panic!("expected Right, got Left({:?})", err);
            }
            other => panic!("expected Right/Left, got {:?}", other),
        }
    }

    // =========================================================================
    // Git handler tests — unit (parse functions) + integration (scratch repo)
    // =========================================================================

    #[test]
    fn test_git_parse_log_output_two_commits() {
        let output = b"\x000123456789012345678901234567890123456789\x00First commit\x00Alice\x002024-01-01T00:00:00+00:00\x00\nfile_a.txt\x00file_b.rs\x00\x00aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\x00Second commit\x00Bob\x002024-01-02T00:00:00+00:00\x00\nfile_c.txt\x00";
        let commits = GitHandler::parse_log_output(output).unwrap();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].subject, "First commit");
        assert_eq!(commits[0].author, "Alice");
        assert_eq!(commits[0].date, "2024-01-01T00:00:00+00:00");
        assert_eq!(commits[0].files, vec!["file_a.txt", "file_b.rs"]);
        assert_eq!(commits[1].subject, "Second commit");
        assert_eq!(commits[1].files, vec!["file_c.txt"]);
    }

    #[test]
    fn test_git_parse_status_renames() {
        let output = b"M  src/lib.rs\x00?? untracked.txt\x00R  new.rs\x00old.rs\x00";
        let entries = GitHandler::parse_status_output(output).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].state, "M ");
        assert_eq!(entries[0].path, "src/lib.rs");
        assert_eq!(entries[1].state, "??");
        assert_eq!(entries[1].path, "untracked.txt");
        assert_eq!(entries[2].path, "new.rs");
    }

    #[test]
    fn test_git_parse_numstat_with_binary_and_rename() {
        let output =
            b"10\t5\tsrc/lib.rs\x00-\t-\timage.png\x000\t0\t\x00old_name.rs\x00new_name.rs\x00";
        let deltas = GitHandler::parse_numstat_output(output).unwrap();
        assert_eq!(deltas.len(), 3);
        assert_eq!(deltas[0].path, "src/lib.rs");
        assert_eq!(deltas[0].adds, 10);
        assert_eq!(deltas[0].dels, 5);
        assert!(!deltas[0].binary);
        assert_eq!(deltas[1].path, "image.png");
        assert!(deltas[1].binary);
        assert_eq!(deltas[1].adds, 0);
        assert_eq!(deltas[2].path, "new_name.rs");
    }

    #[test]
    fn test_git_parse_log_numstat_output_hazards() {
        let output = b"\x001111111111111111111111111111111111111111\x00Normal commit\x00Alice\x002024-01-01T00:00:00+00:00\x00\n10\t5\tsrc/lib.rs\x00\x002222222222222222222222222222222222222222\x00Plain rename\x00Alice\x002024-01-02T00:00:00+00:00\x00\n0\t0\t\x00old_name.rs\x00new_name.rs\x00\x003333333333333333333333333333333333333333\x00Binary file\x00Alice\x002024-01-03T00:00:00+00:00\x00\n-\t-\timage.png\x00\x004444444444444444444444444444444444444444\x00Empty commit\x00Alice\x002024-01-04T00:00:00+00:00\x00\x00";
        let rows = GitHandler::parse_log_numstat_output(output).unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].deltas[0].path, "src/lib.rs");
        assert_eq!(rows[0].deltas[0].adds, 10);
        assert_eq!(rows[0].deltas[0].dels, 5);
        assert_eq!(rows[1].deltas[0].path, "new_name.rs");
        assert_eq!(rows[2].deltas[0].path, "image.png");
        assert!(rows[2].deltas[0].binary);
        assert!(rows[3].deltas.is_empty());
    }

    #[test]
    fn test_git_log_numstat_frames_consecutive_empty_commits() {
        let output = b"\x001111111111111111111111111111111111111111\x00Empty one\x00Author\x002024-01-01T00:00:00+00:00\x00\x00\x00\x002222222222222222222222222222222222222222\x00Empty two\x00Author\x002024-01-02T00:00:00+00:00\x00\x00\x00\x003333333333333333333333333333333333333333\x00Has a file\x00Author\x002024-01-03T00:00:00+00:00\x00\n1\t0\tfile.txt\x00";
        let rows = GitHandler::parse_log_numstat_output(output).unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows[0].deltas.is_empty());
        assert!(rows[1].deltas.is_empty());
        assert_eq!(rows[2].deltas[0].path, "file.txt");
    }

    #[test]
    fn test_git_log_parser_keeps_header_shaped_paths_in_the_commit() {
        let output = b"\x001111111111111111111111111111111111111111\x00Subject\x00Author\x002024-01-01T00:00:00+00:00\x00\none\x00aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\x00looks like subject\x00looks like author\x002024-01-02T00:00:00+00:00\x00";
        let commits = GitHandler::parse_log_output(output).unwrap();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].files.len(), 5);
        assert_eq!(
            commits[0].files[1],
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
    }

    #[test]
    fn test_git_parsers_reject_malformed_and_truncated_output() {
        assert!(matches!(
            GitHandler::parse_status_output(b"M  path"),
            Err(GitError::GitMalformedOutput(_))
        ));
        assert!(matches!(
            GitHandler::parse_status_output(b"M  path\x00R  new\x00"),
            Err(GitError::GitMalformedOutput(_))
        ));
        assert!(matches!(
            GitHandler::parse_numstat_output(b"x\t1\tfile\x00"),
            Err(GitError::GitMalformedOutput(_))
        ));
        assert!(matches!(
            GitHandler::parse_numstat_output(b"1\t1\tfile"),
            Err(GitError::GitMalformedOutput(_))
        ));
        assert!(matches!(
            GitHandler::parse_log_output(
                b"\x00abc\x00subject\x00author\x002024-01-01T00:00:00+00:00\x00"
            ),
            Err(GitError::GitMalformedOutput(_))
        ));
        assert!(matches!(
            GitHandler::parse_log_output(b"\x00"),
            Err(GitError::GitMalformedOutput(_))
        ));
        assert!(matches!(
            GitHandler::parse_log_output(b"\x001111111111111111111111111111111111111111\x00ok\x00author\x002024-01-01T00:00:00+00:00\x00\nfile\x00\x00broken\x00"),
            Err(GitError::GitMalformedOutput(_))
        ));
    }

    #[test]
    #[cfg(unix)]
    fn test_git_parsers_reject_non_utf8_paths() {
        assert!(matches!(
            GitHandler::parse_status_output(b"?? bad-\xff\x00"),
            Err(GitError::GitNonUtf8Path(_))
        ));
        assert!(matches!(
            GitHandler::parse_numstat_output(b"1\t0\tbad-\xff\x00"),
            Err(GitError::GitNonUtf8Path(_))
        ));
    }

    // Build a scratch git repo with 2 commits, a staged file, and an untracked file.
    #[allow(
        clippy::disallowed_methods,
        reason = "short synchronous test-fixture probes (git init/add/commit) that exit \
                  immediately, not a long-lived child needing the launcher"
    )]
    fn make_scratch_repo() -> tempfile::TempDir {
        use std::fs;
        use std::process::Command;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();

        let run = |args: &[&str]| {
            Command::new(args[0])
                .args(&args[1..])
                .current_dir(p)
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@test.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@test.com")
                .env("GIT_AUTHOR_DATE", "2024-01-01T00:00:00+00:00")
                .env("GIT_COMMITTER_DATE", "2024-01-01T00:00:00+00:00")
                .output()
                .expect("git command failed")
        };

        run(&["git", "init", "--initial-branch=main"]);
        run(&["git", "config", "user.email", "test@test.com"]);
        run(&["git", "config", "user.name", "Test"]);

        // First commit
        fs::write(p.join("alpha.txt"), "alpha content").unwrap();
        run(&["git", "add", "alpha.txt"]);
        run(&["git", "commit", "-m", "first: add alpha"]);

        // Second commit
        fs::write(p.join("beta.txt"), "beta content").unwrap();
        run(&["git", "add", "beta.txt"]);
        run(&["git", "commit", "-m", "second: add beta"]);

        // Staged change
        fs::write(p.join("alpha.txt"), "alpha modified").unwrap();
        run(&["git", "add", "alpha.txt"]);

        // Untracked file
        fs::write(p.join("untracked.txt"), "untracked").unwrap();

        dir
    }

    #[test]
    #[allow(
        clippy::disallowed_methods,
        reason = "short synchronous Git fixture commands in a temporary repository"
    )]
    fn test_git_handler_preserves_odd_renamed_and_binary_paths() {
        use std::fs;
        use std::process::Command;

        let dir = make_scratch_repo();
        let root = dir.path();
        let run = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(root)
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@test.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@test.com")
                .output()
                .expect("git fixture command should start");
            assert!(
                output.status.success(),
                "git {:?}: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };
        let old_path = "line\n\"quoted\".txt";
        let new_path = "renamed\n\"quoted\".txt";
        fs::write(root.join(old_path), "odd path contents\n").unwrap();
        run(&["add", "--", old_path]);
        run(&["commit", "-m", "add odd path"]);
        fs::rename(root.join(old_path), root.join(new_path)).unwrap();
        fs::write(root.join("binary.dat"), [0, 1, 2, 255]).unwrap();
        run(&["add", "-A"]);

        let mut handler = GitHandler::new(root.to_path_buf());
        let status = handler.git_status().unwrap();
        assert!(status.iter().any(|entry| entry.path == new_path));
        let deltas = handler.git_diff_stat("HEAD".into()).unwrap();
        assert!(deltas.iter().any(|delta| delta.path == new_path));
        assert!(deltas
            .iter()
            .any(|delta| delta.path == "binary.dat" && delta.binary));
        let history = handler.git_log_numstat(3).unwrap();
        assert!(history
            .iter()
            .flat_map(|row| &row.deltas)
            .any(|delta| delta.path == old_path));
    }

    #[test]
    fn test_git_handler_log_two_commits_newest_first() {
        let dir = make_scratch_repo();
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handler = GitHandler::new(dir.path().to_path_buf());

        let n = (2i64).to_value(&table).unwrap();
        let con_id = table.get_by_name("GitLog").unwrap();
        let request = HaskellValue::Con(con_id, vec![n]);
        let result = unwrap_right(
            response_value(handler.handle(GitReq::GitLog(2), &cx).unwrap(), &table),
            &table,
        );

        // Should be a cons list with 2 Commit cells
        let mut node = &result;
        let mut count = 0;
        loop {
            match node {
                HaskellValue::Con(id, fields) => {
                    let name = table.name_of(*id).unwrap();
                    match name {
                        "[]" => break,
                        ":" => {
                            assert_eq!(fields.len(), 2);
                            // head is a Commit (5 fields)
                            match &fields[0] {
                                HaskellValue::Con(cid, cfields) => {
                                    assert_eq!(table.name_of(*cid).unwrap(), "Commit");
                                    assert_eq!(cfields.len(), 5, "Commit must have 5 fields");
                                }
                                other => panic!("expected Commit Con, got {:?}", other),
                            }
                            count += 1;
                            node = &fields[1];
                        }
                        other => panic!("unexpected list constructor: {}", other),
                    }
                }
                other => panic!("expected Con, got {:?}", other),
            }
        }
        assert_eq!(count, 2, "gitLog 2 should return exactly 2 commits");
        // Suppress unused warning from the FromHaskell round-trip test above
        let _ = request;
    }

    #[test]
    fn test_git_handler_status_sees_staged_and_untracked() {
        let dir = make_scratch_repo();
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handler = GitHandler::new(dir.path().to_path_buf());

        let result = unwrap_right(
            response_value(handler.handle(GitReq::GitStatus(), &cx).unwrap(), &table),
            &table,
        );

        // Collect all StatusEntry names from the cons list
        let mut paths_and_states: Vec<(String, String)> = Vec::new();
        let mut node = &result;
        loop {
            match node {
                HaskellValue::Con(id, fields) if table.name_of(*id).unwrap() == ":" => {
                    if let HaskellValue::Con(eid, efields) = &fields[0] {
                        assert_eq!(table.name_of(*eid).unwrap(), "StatusEntry");
                        assert_eq!(efields.len(), 2);
                        // path is efields[0], state is efields[1]
                        // Extract Text from Con("Text", [ByteArray, off, len])
                        paths_and_states.push(("?".into(), "?".into()));
                    }
                    node = &fields[1];
                }
                HaskellValue::Con(id, _) if table.name_of(*id).unwrap() == "[]" => break,
                other => panic!("unexpected: {:?}", other),
            }
        }
        // At minimum we should see 2 entries (staged alpha.txt + untracked.txt)
        assert!(
            paths_and_states.len() >= 2,
            "gitStatus should return at least 2 entries, got {}",
            paths_and_states.len()
        );
    }

    #[test]
    fn test_git_handler_diffstat_head_tilde1() {
        let dir = make_scratch_repo();
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handler = GitHandler::new(dir.path().to_path_buf());

        let result = unwrap_right(
            response_value(
                handler
                    .handle(GitReq::GitDiffStat("HEAD~1".to_string()), &cx)
                    .unwrap(),
                &table,
            ),
            &table,
        );
        // HEAD~1 introduces beta.txt; should return at least 1 FileDelta
        let mut count = 0;
        let mut node = &result;
        loop {
            match node {
                HaskellValue::Con(id, fields) if table.name_of(*id).unwrap() == ":" => {
                    match &fields[0] {
                        HaskellValue::Con(did, dfields) => {
                            assert_eq!(table.name_of(*did).unwrap(), "FileDelta");
                            assert_eq!(dfields.len(), 4, "FileDelta must have 4 fields");
                        }
                        other => panic!("expected FileDelta Con, got {:?}", other),
                    }
                    count += 1;
                    node = &fields[1];
                }
                HaskellValue::Con(id, _) if table.name_of(*id).unwrap() == "[]" => break,
                other => panic!("unexpected: {:?}", other),
            }
        }
        assert!(
            count >= 1,
            "gitDiffStat HEAD~1 should return at least 1 delta"
        );
    }

    #[test]
    fn test_git_handler_log_numstat_two_commits() {
        let dir = make_scratch_repo();
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handler = GitHandler::new(dir.path().to_path_buf());

        let result = unwrap_right(
            response_value(
                handler.handle(GitReq::GitLogNumstat(2), &cx).unwrap(),
                &table,
            ),
            &table,
        );
        // Should be a cons list with 2 CommitDeltas cells, each carrying a
        // Commit (5 fields) and a [FileDelta] with at least one entry.
        let mut count = 0;
        let mut node = &result;
        loop {
            match node {
                HaskellValue::Con(id, fields) if table.name_of(*id).unwrap() == ":" => {
                    match &fields[0] {
                        HaskellValue::Con(cdid, cdfields) => {
                            assert_eq!(table.name_of(*cdid).unwrap(), "CommitDeltas");
                            assert_eq!(cdfields.len(), 2, "CommitDeltas must have 2 fields");
                            match &cdfields[0] {
                                HaskellValue::Con(cid, cfields) => {
                                    assert_eq!(table.name_of(*cid).unwrap(), "Commit");
                                    assert_eq!(cfields.len(), 5, "Commit must have 5 fields");
                                }
                                other => panic!("expected Commit Con, got {:?}", other),
                            }
                        }
                        other => panic!("expected CommitDeltas Con, got {:?}", other),
                    }
                    count += 1;
                    node = &fields[1];
                }
                HaskellValue::Con(id, _) if table.name_of(*id).unwrap() == "[]" => break,
                other => panic!("unexpected: {:?}", other),
            }
        }
        assert_eq!(count, 2, "gitLogNumstat 2 should return exactly 2 commits");

        // Cross-check against the handler method directly: the newest commit
        // (beta.txt added) carries exactly one non-binary delta for it.
        let rows = handler.git_log_numstat(2).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].deltas.len(), 1);
        assert_eq!(rows[0].deltas[0].path, "beta.txt");
        assert!(!rows[0].deltas[0].binary);
        assert_eq!(rows[0].commit.files, vec!["beta.txt"]);
    }

    #[test]
    fn test_git_handler_show_head() {
        let dir = make_scratch_repo();
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handler = GitHandler::new(dir.path().to_path_buf());

        let result = unwrap_right(
            response_value(
                handler
                    .handle(GitReq::GitShow("HEAD".to_string()), &cx)
                    .unwrap(),
                &table,
            ),
            &table,
        );
        match &result {
            HaskellValue::Con(id, fields) => {
                assert_eq!(table.name_of(*id).unwrap(), "Commit");
                assert_eq!(
                    fields.len(),
                    5,
                    "Commit must have 5 fields (sha/subject/author/date/files)"
                );
            }
            other => panic!("gitShow HEAD should return a Commit, got {:?}", other),
        }
    }

    #[test]
    fn test_git_handler_show_unknown_revision_is_git_failed() {
        let dir = make_scratch_repo();
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handler = GitHandler::new(dir.path().to_path_buf());

        // GitShow is errors-tagged: a Git subprocess failure is typed data.
        let res = response_value(
            handler
                .handle(GitReq::GitShow("notaref_zzzzzz".to_string()), &cx)
                .unwrap(),
            &table,
        );
        match &res {
            HaskellValue::Con(id, fields) if table.name_of(*id).unwrap() == "Left" => {
                let err: GitError = FromHaskell::from_value(&fields[0], &table).unwrap();
                assert!(
                    matches!(err, GitError::GitFailed(_, _)),
                    "expected GitFailed, got {:?}",
                    err
                );
            }
            other => panic!("expected Left (GitFailed _ _), got {:?}", other),
        }
    }

    #[test]
    fn test_git_diff_stat_rejects_flag_shaped_revspec() {
        let dir = make_scratch_repo();
        let mut handler = GitHandler::new(dir.path().to_path_buf());
        match handler.git_diff_stat("--output=/tmp/tidepool-git-f6-poc".to_string()) {
            Err(GitError::GitBadRevspec(_)) => {}
            Err(other) => panic!("expected GitBadRevspec, got a different GitError: {other:?}"),
            Ok(_) => panic!("expected Err(GitBadRevspec(_)), got Ok"),
        }
        assert!(
            !std::path::Path::new("/tmp/tidepool-git-f6-poc").exists(),
            "flag-shaped revspec must never reach git's argv"
        );
    }

    #[test]
    fn test_git_show_rejects_flag_shaped_revspec() {
        let dir = make_scratch_repo();
        let mut handler = GitHandler::new(dir.path().to_path_buf());
        match handler.git_show("-n1".to_string()) {
            Err(GitError::GitBadRevspec(_)) => {}
            Err(other) => panic!("expected GitBadRevspec, got a different GitError: {other:?}"),
            Ok(_) => panic!("expected Err(GitBadRevspec(_)), got Ok"),
        }
    }

    /// Compile Git log, typed failure and numstat operations together. Check
    /// nominal records, nested fields and constructor arity through generated
    /// effects and actual JIT execution.
    #[tokio::test]
    async fn test_jit_git_family() {
        tidepool_testing::eval_harness::require_extract();
        let decls = tidepool_mcp::standard_decls();
        // Return the observed values (not collapsed Bools) so a failure names
        // which invariant broke and what we actually saw.
        let source = jit_test_source(&[
            "commits <- gitLog 1 >>= liftEither",
            "let n = length commits",
            "let shaLen = case commits of { (c:_) -> T.length c.sha; _ -> 0 }",
            "unknownRevision <- gitShow \"notaref_zzzzzz\"",
            "let gitFailedOk = case unknownRevision of { Left (GitFailed _ _) -> True; _ -> False }",
            "deltaRows <- gitLogNumstat 1 >>= liftEither",
            "let numstatN = length deltaRows",
            "let numstatShaLen = case deltaRows of { (cd:_) -> T.length cd.commit.sha; _ -> 0 }",
            "pure (object [\"logCount\" .= n, \"shaLen\" .= shaLen, \"gitFailedOk\" .= gitFailedOk, \"numstatN\" .= numstatN, \"numstatShaLen\" .= numstatShaLen])",
        ]);
        let include = prelude_include();
        let effects_dir = tidepool_mcp::ensure_effects_module(&decls).unwrap();
        let include_paths: Vec<&std::path::Path> = vec![
            include.as_path(),
            effects_dir.core.as_path(),
            effects_dir.shim.as_path(),
        ];
        let kv_path = std::env::temp_dir().join("tidepool_git_jit_family_kv.json");
        let cwd = repo_root();
        let captured = CapturedOutput::new();
        let mut handlers = frunk::hlist![
            crate::ConsoleHandler,
            crate::KvHandler::new(kv_path),
            crate::FsReadHandler::new(cwd.clone()),
            crate::FsWriteHandler::new(cwd.clone()),
            crate::HttpHandler,
            crate::ExecHandler::new(cwd.clone()),
            crate::LlmHandler::new("ollama:llama3.2".to_string()),
            GitHandler::new(cwd.clone()),
            crate::TimeHandler,
        ];
        let result = tidepool_runtime::compile_and_run(
            &source,
            "result",
            &include_paths,
            &mut handlers,
            &captured,
        );
        match result {
            Ok(v) => {
                let json = v.to_json();
                assert_eq!(
                    json["logCount"],
                    serde_json::json!(1),
                    "gitLog 1 should return exactly 1 Commit"
                );
                assert_eq!(
                    json["shaLen"],
                    serde_json::json!(40),
                    "gitLog 1 commit sha should be 40 chars"
                );
                assert_eq!(
                    json["gitFailedOk"],
                    serde_json::json!(true),
                    "gitShow with an unknown revision should be a typed Left (GitFailed _ _)"
                );
                assert_eq!(
                    json["numstatN"],
                    serde_json::json!(1),
                    "gitLogNumstat 1 should return exactly 1 CommitDeltas"
                );
                assert_eq!(
                    json["numstatShaLen"],
                    serde_json::json!(40),
                    "gitLogNumstat 1's CommitDeltas.commit.sha should be 40 chars"
                );
            }
            Err(e) => panic!("JIT git family eval failed: {:?}", e),
        }
    }
}
