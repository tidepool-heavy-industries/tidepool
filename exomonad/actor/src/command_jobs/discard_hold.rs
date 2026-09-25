//! The discard hold: a Git command that would take committed work off a ref
//! runs only when the command carries a `DiscardIntent` naming the ref's
//! actual tip.
//!
//! Held forms: `git reset --hard <target>` whose target does not contain the
//! current `HEAD`; `git rebase --onto <newbase> <upstream> [<branch>]` whose
//! skipped commits are not in `<newbase>`; `git rebase --skip`, which drops
//! `REBASE_HEAD`; `git branch -D` (or `-d --force`) of a branch whose commits no
//! other ref reaches; and a forced `git push <remote> <refspec>` whose
//! remote-tracking ref holds commits the pushed source lacks. A form that drops
//! nothing runs unheld. Whether the worktree is clean never enters the
//! decision: the evidence case was a clean worktree over a committed branch.
//!
//! The intent travels in the command's environment:
//! `EXOMONAD_DISCARD_EXPECTED_TIP` (required), `EXOMONAD_DISCARD_TARGET`
//! (compared when the form names a target: the reset target, the rebase
//! newbase, the pushed source), `EXOMONAD_DISCARD_REASON` (echoed in the
//! receipt). Haskell spells it `withDiscardIntent` (`Tidepool.Worktree`). A
//! mismatched or missing tip refuses before Git runs, with
//! `Ref is at <actual>, not expected <expected>; <verb> would drop committed
//! <oids>. Inspect/rebase or confirm the actual tip.`
//!
//! Where it is enforced: every actor command, hosted `bash` tool call and
//! `Cmd.run` alike, crosses `CommandJobs::start`, and nothing else sees both.
//! The typed `Git` effect is read-only, so it needs no hold. The comparison
//! must read the ref where the command will run — a native backend's sandbox
//! or the host — and admission does not know that directory, so the check
//! travels with the command: a Bash script (`bash ... -c <script>`) that
//! mentions `git`, and an argv whose program is `git` with a subcommand the
//! hold inspects, gain a one-line prefix defining a Bash `git` function
//! (`discard_hold.sh`). Bash itself expands each call, so `cd`, variables and
//! compound commands resolve before the check, and the check runs in the
//! directory the call runs in. The prefix is ANSI-C quoted on one physical
//! line, so the script's own line numbers are unchanged.
//!
//! Not held: `command git`, a path to the Git binary, `sh -c`, and nested
//! shells, which bypass the function; `git clean`, checkout over uncommitted
//! edits and `reset --hard` over a dirty tree, which name no commit (the design
//! asks for an exact path acknowledgment there); a force push's live remote
//! ref, which is compared through its local remote-tracking ref.
use tidepool_bridge_effects::CommandSpec;

const HOLD: &str = include_str!("discard_hold.sh");

/// The argv subcommands whose held forms need the hold installed.
const HELD_SUBCOMMANDS: [&str; 4] = ["reset", "rebase", "branch", "push"];

/// Install the hold into a command bound for execution. A command that
/// cannot reach a held form is returned unchanged.
pub(super) fn install(mut spec: CommandSpec) -> CommandSpec {
    if let Some(script) = bash_script(&spec.argv) {
        if spec.argv[script].contains("git") {
            spec.argv[script] = format!("{}{}", prefix(), spec.argv[script]);
        }
    } else if invokes_held_git(&spec.argv) {
        let mut wrapped = vec![
            "bash".to_owned(),
            "--noprofile".to_owned(),
            "--norc".to_owned(),
            "-c".to_owned(),
            format!("{}git \"$@\"", prefix()),
            "exomonad-git".to_owned(),
        ];
        wrapped.extend(spec.argv.drain(1..));
        spec.argv = wrapped;
    }
    spec
}

/// The index of the script in `bash [options] -c <script> ...`.
fn bash_script(argv: &[String]) -> Option<usize> {
    if program_name(argv.first()?) != "bash" {
        return None;
    }
    let flag = argv
        .iter()
        .skip(1)
        .take_while(|arg| arg.starts_with('-'))
        .position(|arg| arg == "-c")?;
    let script = flag + 2;
    (script < argv.len()).then_some(script)
}

fn invokes_held_git(argv: &[String]) -> bool {
    let Some(program) = argv.first() else {
        return false;
    };
    if program_name(program) != "git" {
        return false;
    }
    let mut rest = argv.iter().skip(1);
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" => {
                rest.next();
            }
            option if option.starts_with('-') => {}
            subcommand => return HELD_SUBCOMMANDS.contains(&subcommand),
        }
    }
    false
}

fn program_name(program: &str) -> &str {
    program.rsplit('/').next().unwrap_or(program)
}

/// `eval $'<hold>'; ` — the hold as one physical line.
fn prefix() -> String {
    let mut quoted = String::with_capacity(HOLD.len() + 16);
    quoted.push_str("eval $'");
    for character in HOLD.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '\'' => quoted.push_str("\\'"),
            '\n' => quoted.push_str("\\n"),
            other => quoted.push(other),
        }
    }
    quoted.push_str("'; ");
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use tidepool_bridge_effects::CommandInput;

    const REFUSAL_TAIL: &str = "Inspect/rebase or confirm the actual tip.";

    struct Repository {
        path: PathBuf,
        base: String,
        work: String,
    }

    impl Drop for Repository {
        fn drop(&mut self) {
            // best-effort: a leaked scratch repository is harmless.
            drop(std::fs::remove_dir_all(&self.path));
        }
    }

    #[allow(clippy::disallowed_methods, reason = "test fixture repository")]
    fn git(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .env("GIT_AUTHOR_NAME", "hold")
            .env("GIT_AUTHOR_EMAIL", "hold@example.invalid")
            .env("GIT_COMMITTER_NAME", "hold")
            .env("GIT_COMMITTER_EMAIL", "hold@example.invalid")
            .output()
            .expect("git runs");
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    /// `master` at a base commit; `work`, checked out and clean, one commit
    /// ahead — the shape in which `reset --hard master` dropped `6667ddc`.
    fn repository() -> Repository {
        let path = std::env::temp_dir().join(format!("discard-hold-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        git(&path, &["init", "-q", "-b", "master", "."]);
        std::fs::write(path.join("notes.md"), "base\n").unwrap();
        git(&path, &["add", "notes.md"]);
        git(&path, &["commit", "-q", "-m", "base"]);
        let base = git(&path, &["rev-parse", "HEAD"]);
        git(&path, &["checkout", "-q", "-b", "work"]);
        std::fs::write(path.join("notes.md"), "base\ninterview\n").unwrap();
        git(&path, &["commit", "-q", "-am", "interview"]);
        let work = git(&path, &["rev-parse", "HEAD"]);
        assert_eq!(git(&path, &["status", "--porcelain"]), "");
        Repository { path, base, work }
    }

    fn spec(argv: &[&str], environment: &[(&str, &str)]) -> CommandSpec {
        CommandSpec {
            argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
            directory: None,
            environment: environment
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
            memory: 1,
            input: CommandInput::ClosedInput,
        }
    }

    /// Run an admitted spec the way a backend does: argv, directory, environment.
    #[allow(
        clippy::disallowed_methods,
        reason = "test stand-in for a command backend"
    )]
    fn execute(repository: &Repository, spec: CommandSpec) -> (bool, String) {
        let admitted = install(spec);
        let output = Command::new(&admitted.argv[0])
            .args(&admitted.argv[1..])
            .current_dir(&repository.path)
            .envs(admitted.environment)
            .output()
            .expect("command runs");
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    fn bash(script: &str, environment: &[(&str, &str)]) -> CommandSpec {
        spec(
            &[
                "bash",
                "--noprofile",
                "--norc",
                "-c",
                script,
                "exomonad-bash",
            ],
            environment,
        )
    }

    fn head(repository: &Repository) -> String {
        git(&repository.path, &["rev-parse", "HEAD"])
    }

    #[test]
    fn stale_expected_tip_refuses_reset_and_drops_nothing() {
        let repository = repository();
        let (ran, stderr) = execute(
            &repository,
            bash(
                "set -e\ngit reset --hard master\necho reset-ran",
                &[("EXOMONAD_DISCARD_EXPECTED_TIP", &repository.base)],
            ),
        );
        assert!(!ran, "{stderr}");
        let expected = format!(
            "Ref is at {}, not expected {}; reset would drop committed {}. {REFUSAL_TAIL}",
            &repository.work[..7],
            &repository.base[..7],
            &repository.work[..7],
        );
        assert!(stderr.starts_with(&expected), "{stderr}");
        assert_eq!(head(&repository), repository.work);
    }

    #[test]
    fn clean_worktree_without_intent_does_not_waive_the_hold() {
        let repository = repository();
        let (ran, stderr) = execute(&repository, bash("git reset --hard master", &[]));
        assert!(!ran, "{stderr}");
        assert!(stderr.contains("would drop committed"), "{stderr}");
        assert_eq!(head(&repository), repository.work);
    }

    #[test]
    fn actual_tip_confirms_reset() {
        let repository = repository();
        let (ran, stderr) = execute(
            &repository,
            bash(
                "cd . && git reset --hard master",
                &[
                    ("EXOMONAD_DISCARD_EXPECTED_TIP", &repository.work),
                    ("EXOMONAD_DISCARD_TARGET", &repository.base),
                    ("EXOMONAD_DISCARD_REASON", "roll back"),
                ],
            ),
        );
        assert!(ran, "{stderr}");
        assert!(stderr.contains("Discard confirmed"), "{stderr}");
        assert_eq!(head(&repository), repository.base);
    }

    #[test]
    fn argv_git_is_held_and_unheld_forms_run_unchanged() {
        let repository = repository();
        let (ran, stderr) = execute(
            &repository,
            spec(&["git", "-C", ".", "reset", "--hard", "master"], &[]),
        );
        assert!(!ran, "{stderr}");
        assert_eq!(head(&repository), repository.work);
        // Nothing dropped: forward and in-place resets are not held.
        let (ran, stderr) = execute(&repository, spec(&["git", "reset", "--hard"], &[]));
        assert!(ran, "{stderr}");
        let status = spec(&["git", "status", "--short"], &[]);
        assert_eq!(install(status.clone()), status);
    }
}
