use std::path::PathBuf;
use tidepool_bridge_effects::{GitCommit, GitCommitDeltas, GitFileDelta, GitStatusEntry};
use tidepool_worktree::git::GitCli;

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

    /// Run a git command in the sandbox root, through `GitCli` — the
    /// workspace's one place that shells out to git (env scrubbing, prompt
    /// disabling, locale; see `tidepool-worktree/CLAUDE.md`'s "one git call
    /// site" rule). Failure is TYPED (#335): an unknown/ambiguous revspec is
    /// `GitBadRevspec`; any other nonzero exit (or a git binary that can't
    /// even be spawned, exit code -1) is `GitFailed code detail`.
    fn run_git(&self, args: &[&str]) -> Result<String, GitError> {
        GitCli::new()
            .run(&self.root, args)
            .map(|out| out.stdout)
            .map_err(|receipt: tidepool_worktree::error::GitFailureReceipt| {
                let stderr = receipt.stderr.trim().to_string();
                let code = receipt.exit_code.unwrap_or(-1) as i64;
                if stderr.contains("bad revision")
                    || stderr.contains("unknown revision")
                    || stderr.contains("ambiguous argument")
                {
                    GitError::GitBadRevspec(stderr)
                } else {
                    GitError::GitFailed(code, stderr)
                }
            })
    }

    /// Reject a revspec that could be mistaken for an option by git's own
    /// argv parser (e.g. `--output=/tmp/x` writing OUTSIDE the Fs sandbox via
    /// `git diff`'s `--output`). No legitimate ref/revspec starts with `-`
    /// (git itself refuses to create such refs), so this is a pure guard —
    /// callers pass the revspec positionally, and `run_git` never invokes a
    /// shell, so this is the one place flag-shaped input can still reach git.
    fn validate_revspec(rev: &str) -> Result<(), GitError> {
        if rev.starts_with('-') {
            return Err(GitError::GitBadRevspec(format!(
                "revspec must not start with '-' (looks like a git option): {:?}",
                rev
            )));
        }
        Ok(())
    }

    /// Parse `git log --format="%H%x00%s%x00%an%x00%cI" --name-only` output.
    ///
    /// Each commit block is separated by a blank line (`\n\n`).  The first
    /// line of each block is NUL-delimited metadata; subsequent non-empty
    /// lines are changed files.
    fn parse_log_output(output: &str) -> Vec<GitCommit> {
        // git log --format=... --name-only produces:
        //   sha\x00subject\x00author\x00date\n
        //   \n                           <- blank separator between header and files
        //   file1\n
        //   file2\n
        //   sha2\x00...                 <- next header follows files directly (no blank)
        //
        // Scan line by line: lines with 4+ NUL-separated fields are headers; blank
        // lines are separators (skip); all other non-empty lines are file paths.
        let mut commits = Vec::new();
        let mut current: Option<GitCommit> = None;

        for line in output.lines() {
            if line.is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.splitn(4, '\x00').collect();
            if parts.len() >= 4 {
                // Header line: sha\x00subject\x00author\x00date
                if let Some(commit) = current.take() {
                    commits.push(commit);
                }
                current = Some(GitCommit {
                    sha: parts[0].to_string(),
                    subject: parts[1].to_string(),
                    author: parts[2].to_string(),
                    date: parts[3].to_string(),
                    files: Vec::new(),
                });
            } else if let Some(ref mut commit) = current {
                commit.files.push(line.to_string());
            }
        }
        if let Some(commit) = current {
            commits.push(commit);
        }
        commits
    }

    /// Parse one commit from log output; error if missing.
    fn parse_single_commit(output: &str, revspec: &str) -> Result<GitCommit, GitError> {
        let commits = Self::parse_log_output(output);
        commits.into_iter().next().ok_or_else(|| {
            GitError::GitBadRevspec(format!("gitShow: no commit found for '{}'", revspec))
        })
    }

    /// Parse `git status --porcelain=v1` output.
    fn parse_status_output(output: &str) -> Vec<GitStatusEntry> {
        output
            .lines()
            .filter(|l| l.len() >= 3)
            .map(|line| {
                let state = line[..2].to_string();
                let rest = line[3..].trim();
                // For renames "XY old -> new", take the destination path.
                let path = if let Some(idx) = rest.find(" -> ") {
                    rest[idx + 4..].to_string()
                } else {
                    rest.to_string()
                };
                GitStatusEntry { path, state }
            })
            .collect()
    }

    /// Parse `git diff --numstat <rev>` output.
    fn parse_numstat_output(output: &str) -> Vec<GitFileDelta> {
        output
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|line| {
                let parts: Vec<&str> = line.splitn(3, '\t').collect();
                if parts.len() < 3 {
                    return None;
                }
                let (adds_s, dels_s, path) = (parts[0], parts[1], parts[2].trim());
                // Binary files show "-" instead of counts.
                let binary = adds_s == "-" || dels_s == "-";
                let adds = if binary {
                    0
                } else {
                    adds_s.parse::<i64>().unwrap_or(0)
                };
                let dels = if binary {
                    0
                } else {
                    dels_s.parse::<i64>().unwrap_or(0)
                };
                Some(GitFileDelta {
                    path: path.to_string(),
                    adds,
                    dels,
                    binary,
                })
            })
            .collect()
    }

    /// A numstat path that names a rename: either the plain `old => new` form
    /// (no common prefix/suffix) or the `{old => new}` infix form git emits
    /// when the rename shares a directory prefix and/or filename suffix
    /// (e.g. `src/{old.rs => new.rs}`, `{old => new}/file.txt`). Returns the
    /// NEW path in both cases; a non-rename path is returned unchanged.
    fn resolve_renamed_path(raw: &str) -> String {
        if let (Some(start), Some(end)) = (raw.find('{'), raw.find('}')) {
            if end > start {
                if let Some(arrow) = raw[start..end].find(" => ") {
                    let prefix = &raw[..start];
                    let new_part = &raw[start + arrow + 4..end];
                    let suffix = &raw[end + 1..];
                    return format!("{prefix}{new_part}{suffix}");
                }
            }
        }
        match raw.find(" => ") {
            Some(arrow) => raw[arrow + 4..].trim().to_string(),
            None => raw.to_string(),
        }
    }

    /// Parse `git log --format="%H%x00%s%x00%an%x00%cI" --numstat -M` output:
    /// one NUL-delimited commit header (same shape `parse_log_output` reads)
    /// followed by that commit's own numstat lines, in ONE subprocess for
    /// every commit — the single parse site for `gitLogNumstat`. A merge or
    /// otherwise-empty commit's header is followed by zero numstat lines (git
    /// shows no diff for a merge without `-m`), so `deltas` comes back `[]`
    /// for it, not an error. `Commit.files` is populated from the same
    /// deltas, matching `gitLog`'s (`--name-only`) shape.
    fn parse_log_numstat_output(output: &str) -> Vec<GitCommitDeltas> {
        let mut result = Vec::new();
        let mut current: Option<(GitCommit, Vec<GitFileDelta>)> = None;

        for line in output.lines() {
            if line.is_empty() {
                continue;
            }
            let header_parts: Vec<&str> = line.splitn(4, '\x00').collect();
            if header_parts.len() >= 4 {
                if let Some((commit, deltas)) = current.take() {
                    result.push(GitCommitDeltas { commit, deltas });
                }
                current = Some((
                    GitCommit {
                        sha: header_parts[0].to_string(),
                        subject: header_parts[1].to_string(),
                        author: header_parts[2].to_string(),
                        date: header_parts[3].to_string(),
                        files: Vec::new(),
                    },
                    Vec::new(),
                ));
                continue;
            }
            let Some((commit, deltas)) = current.as_mut() else {
                continue;
            };
            let parts: Vec<&str> = line.splitn(3, '\t').collect();
            if parts.len() < 3 {
                continue;
            }
            let (adds_s, dels_s, raw_path) = (parts[0], parts[1], parts[2].trim());
            let binary = adds_s == "-" || dels_s == "-";
            let adds = if binary {
                0
            } else {
                adds_s.parse::<i64>().unwrap_or(0)
            };
            let dels = if binary {
                0
            } else {
                dels_s.parse::<i64>().unwrap_or(0)
            };
            let path = Self::resolve_renamed_path(raw_path);
            commit.files.push(path.clone());
            deltas.push(GitFileDelta {
                path,
                adds,
                dels,
                binary,
            });
        }
        if let Some((commit, deltas)) = current {
            result.push(GitCommitDeltas { commit, deltas });
        }
        result
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
            "--format=%H%x00%s%x00%an%x00%cI",
            "--name-only",
        ])?;
        Ok(Self::parse_log_output(&output))
    }

    fn git_status(&mut self) -> Result<Vec<GitStatusEntry>, GitError> {
        let output = self.run_git(&["status", "--porcelain=v1"])?;
        Ok(Self::parse_status_output(&output))
    }

    fn git_diff_stat(&mut self, rev: String) -> Result<Vec<GitFileDelta>, GitError> {
        Self::validate_revspec(&rev)?;
        // Trailing `--` closes the pathspec boundary so `rev` can never be
        // reinterpreted as (or followed by) an option, even defensively.
        let output = self.run_git(&["diff", "--numstat", &rev, "--"])?;
        Ok(Self::parse_numstat_output(&output))
    }

    fn git_show(&mut self, rev: String) -> Result<GitCommit, GitError> {
        Self::validate_revspec(&rev)?;
        let output = self.run_git(&[
            "log",
            "-n",
            "1",
            &rev,
            "--format=%H%x00%s%x00%an%x00%cI",
            "--name-only",
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
            "--format=%H%x00%s%x00%an%x00%cI",
            "--numstat",
            "-M",
        ])?;
        Ok(Self::parse_log_numstat_output(&output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use tidepool_bridge::{FromCore, ToCore};
    use tidepool_effect::dispatch::{EffectContext, EffectHandler};
    use tidepool_eval::value::Value;
    use tidepool_mcp::CapturedOutput;

    /// Peel one `Right`/`Left` Con layer off a #335 errors-tagged response,
    /// panicking with the decoded `GitError` on `Left`. Matches by reference
    /// (`Value` has a manual `Drop` impl, so it can't be partially moved out
    /// of) and clones just the field it needs.
    fn unwrap_right(val: Value, table: &tidepool_repr::DataConTable) -> Value {
        match &val {
            Value::Con(id, fields) if table.name_of(*id).unwrap() == "Right" => fields[0].clone(),
            Value::Con(id, fields) if table.name_of(*id).unwrap() == "Left" => {
                let err: GitError = FromCore::from_value(&fields[0], table).unwrap();
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
        // Simulate `git log -n 2 --format="%H%x00%s%x00%an%x00%cI" --name-only`
        let output = "\
abc123\x00First commit\x00Alice\x002024-01-01T00:00:00+00:00\n\
\n\
file_a.txt\n\
file_b.rs\n\
\n\
def456\x00Second commit\x00Bob\x002024-01-02T00:00:00+00:00\n\
\n\
file_c.txt\n\
\n";
        let commits = GitHandler::parse_log_output(output);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].sha, "abc123");
        assert_eq!(commits[0].subject, "First commit");
        assert_eq!(commits[0].author, "Alice");
        assert_eq!(commits[0].date, "2024-01-01T00:00:00+00:00");
        assert_eq!(commits[0].files, vec!["file_a.txt", "file_b.rs"]);
        assert_eq!(commits[1].sha, "def456");
        assert_eq!(commits[1].subject, "Second commit");
        assert_eq!(commits[1].files, vec!["file_c.txt"]);
    }

    #[test]
    fn test_git_parse_status_renames() {
        let output = "M  src/lib.rs\n?? untracked.txt\nR  old.rs -> new.rs\n";
        let entries = GitHandler::parse_status_output(output);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].state, "M ");
        assert_eq!(entries[0].path, "src/lib.rs");
        assert_eq!(entries[1].state, "??");
        assert_eq!(entries[1].path, "untracked.txt");
        // Rename: destination path only
        assert_eq!(entries[2].path, "new.rs");
    }

    #[test]
    fn test_git_parse_numstat_with_binary() {
        let output = "10\t5\tsrc/lib.rs\n-\t-\timage.png\n3\t0\tdocs/README.md\n";
        let deltas = GitHandler::parse_numstat_output(output);
        assert_eq!(deltas.len(), 3);
        assert_eq!(deltas[0].path, "src/lib.rs");
        assert_eq!(deltas[0].adds, 10);
        assert_eq!(deltas[0].dels, 5);
        assert!(!deltas[0].binary);
        assert_eq!(deltas[1].path, "image.png");
        assert!(deltas[1].binary);
        assert_eq!(deltas[1].adds, 0);
        assert_eq!(deltas[2].path, "docs/README.md");
        assert_eq!(deltas[2].adds, 3);
        assert_eq!(deltas[2].dels, 0);
    }

    /// `gitLogNumstat`'s single parse site (`parse_log_numstat_output`),
    /// every hazard as one table: a plain file, a plain `old => new` rename,
    /// a `{old => new}` infix rename with a common PREFIX, one with a common
    /// SUFFIX, a binary file (`-\t-\tpath`), and a merge/empty commit with
    /// zero numstat lines (deltas == [], not an error).
    #[test]
    fn test_git_parse_log_numstat_output_hazards() {
        let output = "\
c1\x00Normal commit\x00Alice\x002024-01-01T00:00:00+00:00\n\
\n\
10\t5\tsrc/lib.rs\n\
\n\
c2\x00Plain rename\x00Alice\x002024-01-02T00:00:00+00:00\n\
\n\
0\t0\told_name.rs => new_name.rs\n\
\n\
c3\x00Infix rename, common prefix\x00Alice\x002024-01-03T00:00:00+00:00\n\
\n\
2\t1\tsrc/{old.rs => new.rs}\n\
\n\
c4\x00Infix rename, common suffix\x00Alice\x002024-01-04T00:00:00+00:00\n\
\n\
1\t1\t{old => new}/shared.rs\n\
\n\
c5\x00Binary file\x00Alice\x002024-01-05T00:00:00+00:00\n\
\n\
-\t-\timage.png\n\
\n\
c6\x00Merge commit (no diff)\x00Alice\x002024-01-06T00:00:00+00:00\n\
\n\
c7\x00Trailing commit\x00Alice\x002024-01-07T00:00:00+00:00\n\
\n\
3\t3\tdocs/README.md\n";
        let rows = GitHandler::parse_log_numstat_output(output);
        assert_eq!(rows.len(), 7, "expected 7 commits, got {}", rows.len());

        // c1: plain file, unaffected by rename resolution.
        assert_eq!(rows[0].commit.sha, "c1");
        assert_eq!(rows[0].deltas.len(), 1);
        assert_eq!(rows[0].deltas[0].path, "src/lib.rs");
        assert_eq!(rows[0].deltas[0].adds, 10);
        assert_eq!(rows[0].deltas[0].dels, 5);
        assert!(!rows[0].deltas[0].binary);
        assert_eq!(rows[0].commit.files, vec!["src/lib.rs"]);

        // c2: plain `old => new` rename records only the NEW path.
        assert_eq!(rows[1].deltas[0].path, "new_name.rs");

        // c3: `{old => new}` infix with a common directory PREFIX.
        assert_eq!(rows[2].deltas[0].path, "src/new.rs");

        // c4: `{old => new}` infix with a common filename SUFFIX.
        assert_eq!(rows[3].deltas[0].path, "new/shared.rs");

        // c5: binary file — adds/dels are 0, not parsed from "-".
        assert_eq!(rows[4].deltas[0].path, "image.png");
        assert!(rows[4].deltas[0].binary);
        assert_eq!(rows[4].deltas[0].adds, 0);
        assert_eq!(rows[4].deltas[0].dels, 0);

        // c6: merge/empty commit — zero numstat lines, not an error.
        assert_eq!(rows[5].commit.sha, "c6");
        assert!(rows[5].deltas.is_empty());
        assert!(rows[5].commit.files.is_empty());

        // c7: parsing resumes correctly after an empty-deltas commit.
        assert_eq!(rows[6].commit.sha, "c7");
        assert_eq!(rows[6].deltas[0].path, "docs/README.md");
    }

    // Build a scratch git repo with 2 commits, a staged file, and an untracked file.
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
    fn test_git_handler_log_two_commits_newest_first() {
        let dir = make_scratch_repo();
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handler = GitHandler::new(dir.path().to_path_buf());

        let n = (2i64).to_value(&table).unwrap();
        let con_id = table.get_by_name("GitLog").unwrap();
        let request = Value::Con(con_id, vec![n]);
        let result = unwrap_right(
            response_value(handler.handle(GitReq::GitLog(2), &cx).unwrap(), &table),
            &table,
        );

        // Should be a cons list with 2 Commit cells
        let mut node = &result;
        let mut count = 0;
        loop {
            match node {
                Value::Con(id, fields) => {
                    let name = table.name_of(*id).unwrap();
                    match name {
                        "[]" => break,
                        ":" => {
                            assert_eq!(fields.len(), 2);
                            // head is a Commit (5 fields)
                            match &fields[0] {
                                Value::Con(cid, cfields) => {
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
        // Suppress unused warning from the FromCore round-trip test above
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
                Value::Con(id, fields) if table.name_of(*id).unwrap() == ":" => {
                    if let Value::Con(eid, efields) = &fields[0] {
                        assert_eq!(table.name_of(*eid).unwrap(), "StatusEntry");
                        assert_eq!(efields.len(), 2);
                        // path is efields[0], state is efields[1]
                        // Extract Text from Con("Text", [ByteArray, off, len])
                        paths_and_states.push(("?".into(), "?".into()));
                    }
                    node = &fields[1];
                }
                Value::Con(id, _) if table.name_of(*id).unwrap() == "[]" => break,
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
                Value::Con(id, fields) if table.name_of(*id).unwrap() == ":" => {
                    match &fields[0] {
                        Value::Con(did, dfields) => {
                            assert_eq!(table.name_of(*did).unwrap(), "FileDelta");
                            assert_eq!(dfields.len(), 4, "FileDelta must have 4 fields");
                        }
                        other => panic!("expected FileDelta Con, got {:?}", other),
                    }
                    count += 1;
                    node = &fields[1];
                }
                Value::Con(id, _) if table.name_of(*id).unwrap() == "[]" => break,
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
                Value::Con(id, fields) if table.name_of(*id).unwrap() == ":" => {
                    match &fields[0] {
                        Value::Con(cdid, cdfields) => {
                            assert_eq!(table.name_of(*cdid).unwrap(), "CommitDeltas");
                            assert_eq!(cdfields.len(), 2, "CommitDeltas must have 2 fields");
                            match &cdfields[0] {
                                Value::Con(cid, cfields) => {
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
                Value::Con(id, _) if table.name_of(*id).unwrap() == "[]" => break,
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
            Value::Con(id, fields) => {
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
    fn test_git_handler_show_bad_revspec_errors() {
        let dir = make_scratch_repo();
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handler = GitHandler::new(dir.path().to_path_buf());

        // GitShow is errors-tagged (#335): a bad revspec is a typed
        // `Left (GitBadRevspec _)` DATA, not an abort.
        let res = response_value(
            handler
                .handle(GitReq::GitShow("notaref_zzzzzz".to_string()), &cx)
                .unwrap(),
            &table,
        );
        match &res {
            Value::Con(id, fields) if table.name_of(*id).unwrap() == "Left" => {
                let err: GitError = FromCore::from_value(&fields[0], &table).unwrap();
                assert!(
                    matches!(err, GitError::GitBadRevspec(_)),
                    "expected GitBadRevspec, got {:?}",
                    err
                );
            }
            other => panic!("expected Left (GitBadRevspec _), got {:?}", other),
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

    /// Bundles `test_jit_git_log_returns_commit` (full JIT end-to-end:
    /// `gitLog 1` on the real repo returns a Commit record with a
    /// 40-character sha field, exercising the generated Tidepool.Effects
    /// wiring + Records visibility + con-name/arity agreement through the
    /// JIT) + `test_jit_git_show_bad_revspec_is_typed_left` (#335 acceptance:
    /// `gitShow` with a bad revspec is a typed `Left (GitBadRevspec _)` the
    /// eval pattern-matches, never an abort) + a `gitLogNumstat 1` exercise
    /// (the new bulk verb: `CommitDeltas{commit,deltas}` resolves through the
    /// real `Tidepool.Records.Bridged`/`.Stable` wiring, nested record-dot
    /// access included) into one tidepool-extract compile. Skips cleanly
    /// when TIDEPOOL_EXTRACT is unavailable.
    #[tokio::test]
    async fn test_jit_git_family() {
        if !tidepool_testing::eval_harness::extract_available() {
            eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
            return;
        }
        let decls = tidepool_mcp::standard_decls();
        // Return the observed values (not collapsed Bools) so a failure names
        // which invariant broke and what we actually saw.
        let source = jit_test_source(&[
            "commits <- gitLog 1 >>= liftEither",
            "let n = length commits",
            "let shaLen = case commits of { (c:_) -> T.length c.sha; _ -> 0 }",
            "badRevspec <- gitShow \"notaref_zzzzzz\"",
            "let badRevspecOk = case badRevspec of { Left (GitBadRevspec _) -> True; _ -> False }",
            "deltaRows <- gitLogNumstat 1 >>= liftEither",
            "let numstatN = length deltaRows",
            "let numstatShaLen = case deltaRows of { (cd:_) -> T.length cd.commit.sha; _ -> 0 }",
            "pure (object [\"logCount\" .= n, \"shaLen\" .= shaLen, \"badRevspecOk\" .= badRevspecOk, \"numstatN\" .= numstatN, \"numstatShaLen\" .= numstatShaLen])",
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
            crate::FsHandler::new(cwd.clone()),
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
                    json["badRevspecOk"],
                    serde_json::json!(true),
                    "gitShow with a bad revspec should be a typed Left (GitBadRevspec _)"
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
