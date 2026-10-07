//! A command job's completion as a settlement.
//!
//! `CommandJobs` owns the job; `RequestRegistry` owns settlement. Joining them
//! here lets a finished job wake its owner through the same settlement notice,
//! watch and route paths as a child's reply. Every start also records
//! the source the command started at: a short `git` probe runs through the
//! same job owner, in the same directory, and the command itself is released
//! to its backend only after the probe has answered or its bound expired.
//!
//! The checkout is not guarded while a job runs: the recorded commit is where
//! the command started, not proof that the tree stayed there. Settlement is
//! process-local, like a child's reply: a job running when the host restarts
//! is not recovered and sends no notice.

use super::*;
use crate::command_jobs::{CommandBackendRequest, CommandJobs};
use crate::request_effect::WatchSubject;
use crate::RequestId;
use tidepool_bridge_effects::{
    CommandCleanup, CommandError, CommandInput, CommandOutcome, CommandPage, CommandPosition,
    CommandReport, CommandResult, CommandSource, CommandSourceCapture, CommandSpec, CommandStream,
};

/// How long a running source probe may take before the command is released
/// without a recorded source. Admission time does not count: a probe queued
/// behind other jobs' memory holds the command it precedes.
const SOURCE_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Pages read while confirming that a stream's retained output is contiguous.
/// A stream whose tail cannot be reached within this many pages is reported
/// as unverified rather than complete.
const CONTIGUITY_PAGE_BUDGET: usize = 256;
const TAIL_LINES: usize = 12;
const TAIL_BYTES: usize = 1200;
const COMMAND_DISPLAY_BYTES: usize = 400;

/// How long a start waits, once the probe's backend exists, for
/// its resource admission (memory admission can queue it behind other jobs)
/// before releasing the command without a recorded source.
const SOURCE_PROBE_ADMISSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Prints the working directory, then `HEAD`, then whether the checkout
/// differs from it, untracked files included. Optional locks stay off so the
/// probe never contends for the index with the command it precedes or with
/// another checkout user.
const SOURCE_PROBE: &str = "pwd || exit 1\ncommit=$(git rev-parse --verify -q HEAD 2>/dev/null) || exit 0\nstatus=$(git status --porcelain --untracked-files=normal 2>/dev/null)\nstatus_code=$?\n[ \"$status_code\" -eq 0 ] || exit \"$status_code\"\nif [ -z \"$status\" ]; then state=clean; else state=dirty; fi\nprintf '%s\\n%s\\n' \"$commit\" \"$state\"\n";

#[derive(Clone)]
pub(super) struct CommandSettlements {
    jobs: CommandJobs,
    requests: Arc<RequestRegistry>,
    deployments: mpsc::Sender<LocalResidentDeployment>,
}

/// The two jobs of one start: the source probe (absent when it
/// could not be started) and the backend request the command waits behind.
pub(super) struct CommandStart {
    probe: Option<String>,
    command: Arc<CommandBackendRequest>,
}

/// Ask the deployment owner for this job's backend. Without an owner the job
/// settles as failed, exactly as a foreground start does.
pub(super) fn dispatch_backend(
    deployments: &mpsc::Sender<LocalResidentDeployment>,
    request: Arc<CommandBackendRequest>,
) {
    if !request.pending() {
        return;
    }
    if deployments
        .try_send(LocalResidentDeployment::CommandBackend(request.clone()))
        .is_err()
    {
        request.supply(Err(CommandError::CommandUnavailable(
            "native host unavailable".into(),
        )));
    }
}

fn probe_spec(spec: &CommandSpec) -> CommandSpec {
    let mut environment = spec
        .environment
        .iter()
        .filter(|(key, _)| key != "GIT_OPTIONAL_LOCKS")
        .cloned()
        .collect::<Vec<_>>();
    environment.push(("GIT_OPTIONAL_LOCKS".into(), "0".into()));
    CommandSpec {
        argv: vec![
            "bash".into(),
            "--noprofile".into(),
            "--norc".into(),
            "-c".into(),
            SOURCE_PROBE.into(),
            "exomonad-source".into(),
        ],
        directory: spec.directory.clone(),
        environment,
        memory: 256 * 1024 * 1024,
        input: CommandInput::ClosedInput,
        source_capture: CommandSourceCapture::NoCapture,
    }
}

fn parse_probe(stdout: &str) -> Option<CommandSource> {
    let mut lines = stdout.lines();
    let directory = lines.next().filter(|line| !line.is_empty())?.to_owned();
    let Some(commit) = lines.next() else {
        return Some(CommandSource {
            directory,
            commit: None,
            dirty: false,
        });
    };
    let valid_oid = matches!(commit.len(), 40 | 64)
        && commit
            .chars()
            .all(|character| character.is_ascii_hexdigit());
    let state = lines.next()?;
    if !valid_oid || lines.next().is_some() {
        return None;
    }
    let dirty = match state {
        "clean" => false,
        "dirty" => true,
        _ => return None,
    };
    Some(CommandSource {
        directory,
        commit: Some(commit.to_owned()),
        dirty,
    })
}

impl CommandSettlements {
    pub(super) fn new<H, O>(environment: &ResidentEnvironment<H, O>) -> Self {
        Self {
            jobs: environment.commands.clone(),
            requests: Arc::clone(&environment.requests),
            deployments: environment.deployments.clone(),
        }
    }

    /// Capture source only when the authored command requests it. The source
    /// probe is an admitted native command, so ordinary runs avoid that work.
    pub(super) async fn start(
        &self,
        kernel: &KernelContext,
        spec: CommandSpec,
        notify_owner: bool,
        invocation: Option<&super::invocation_work::InvocationWork>,
    ) -> Result<String, CommandError> {
        let capture_source = spec.source_capture == CommandSourceCapture::CaptureBeforeStart;
        let source_probe_spec = capture_source.then(|| probe_spec(&spec));
        let (job, command) = self.jobs.start(kernel, spec, invocation).await?;
        let probe = if let Some(probe_spec) = source_probe_spec {
            match self
                .jobs
                .start_source_probe(kernel, probe_spec, invocation)
                .await
            {
                Ok((probe, request)) => {
                    self.jobs.set_source_probe(&job, probe.clone())?;
                    dispatch_backend(&self.deployments, request);
                    Some(probe)
                }
                Err(error) => {
                    tracing::warn!(?error, %job, "command source probe not started");
                    None
                }
            }
        } else {
            None
        };
        let start = CommandStart { probe, command };
        match self.arm(&job, notify_owner, Some(start)) {
            Ok(request) if !notify_owner => {
                // Ordinary starts retain their report at the job owner, not an
                // unused readiness hold. A later watch acquires its own hold.
                self.requests.release_command_holds(&[request]);
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(?error, %job, "command settlement not armed"),
        }
        Ok(job)
    }

    /// The caller has claimed this observation before the notice mutation.
    /// A still-live job keeps its existing settlement as the single owner
    /// notice; a racing finish is delivered through that same record.
    pub(super) async fn notify_after_observation(
        &self,
        owner: crate::ActorRef,
        job: &str,
        observed: tidepool_bridge_effects::CommandStatus,
    ) -> Result<tidepool_bridge_effects::CommandStatus, CommandError> {
        if self.jobs.owner(job)? != owner {
            return Err(CommandError::CommandUnauthorized);
        }
        if matches!(
            observed,
            tidepool_bridge_effects::CommandStatus::CommandFinished(_)
        ) {
            return self.jobs.status(owner, job).await;
        }
        self.arm(job, true, None)?;
        Ok(observed)
    }

    /// The request that settles when `job` finishes, arming it on first use.
    /// Only the call that arms it releases a start's command.
    fn arm(
        &self,
        job: &str,
        notify_owner: bool,
        start: Option<CommandStart>,
    ) -> Result<RequestId, CommandError> {
        let requests = Arc::clone(&self.requests);
        let settled = self.jobs.settlement(job, |owner| {
            requests.reserve_command_settlement(owner, job.to_owned(), notify_owner)
        });
        let (request, armed) = match settled {
            Ok(settled) => settled,
            Err(error) => {
                if let Some(start) = start {
                    dispatch_backend(&self.deployments, start.command);
                }
                return Err(error);
            }
        };
        if notify_owner && !self.jobs.claim_owner_notice(job)? {
            if let Some(start) = start {
                dispatch_backend(&self.deployments, start.command);
            }
            return Ok(request);
        }
        if !armed {
            if notify_owner {
                match self.requests.notify_command_owner(request) {
                    Some(notifications) => {
                        let settlements = self.clone();
                        tokio::spawn(async move {
                            publish_request_notifications(
                                &settlements.requests,
                                &settlements.deployments,
                                notifications,
                            )
                            .await;
                        });
                    }
                    None => return self.rearm(job, start, true),
                }
            } else if !self.requests.hold_command(request) {
                // A released record is rearmed from the retained report for
                // a later watch on that same completed job.
                return self.rearm(job, start, false);
            }
        }
        if armed {
            let settlements = self.clone();
            let job = job.to_owned();
            tokio::spawn(async move { settlements.settle(job, request, start).await });
        } else if let Some(start) = start {
            dispatch_backend(&self.deployments, start.command);
        }
        Ok(request)
    }

    fn rearm(
        &self,
        job: &str,
        start: Option<CommandStart>,
        notify_owner: bool,
    ) -> Result<RequestId, CommandError> {
        if let Some(start) = start {
            dispatch_backend(&self.deployments, start.command);
        }
        let report = self.jobs.report(job)?.ok_or_else(|| {
            CommandError::CommandUnavailable("job settled without a retained report".into())
        })?;
        let revision = report
            .source
            .as_ref()
            .and_then(|source| source.commit.clone());
        let requests = Arc::clone(&self.requests);
        let request = self.jobs.replace_settlement(job, |owner| {
            requests.reserve_command_settlement(owner, job.to_owned(), notify_owner)
        })?;
        let notifications = self.requests.settle_command(
            request,
            render_report(job, &report),
            revision,
            Some(report),
        );
        if notify_owner {
            let settlements = self.clone();
            tokio::spawn(async move {
                publish_request_notifications(
                    &settlements.requests,
                    &settlements.deployments,
                    notifications,
                )
                .await;
            });
        }
        Ok(request)
    }

    /// Resolve command-job watch dependencies to their retained settlement,
    /// rearming a released report without an owner notice of its own. The records
    /// armed here are held for the registration; a refused registration hands
    /// them to [`Self::release`].
    pub(super) fn resolve(
        &self,
        plan: crate::request::readiness::Plan<(WatchSubject, crate::request::WatchRequirement)>,
    ) -> Result<
        crate::request::readiness::Plan<(RequestId, crate::request::WatchRequirement)>,
        crate::request::ReplyError,
    > {
        let mut held = Vec::new();
        let resolved = plan.try_map(|(subject, requirement)| {
            let request = match subject {
                WatchSubject::Request(request) => request,
                WatchSubject::Command(job) => {
                    self.arm(&job, false, None).map_err(|error| match error {
                        CommandError::CommandUnauthorized => {
                            crate::request::ReplyError::Unauthorized
                        }
                        _ => crate::request::ReplyError::Stale,
                    })?
                }
            };
            held.push(request);
            Ok((request, requirement))
        });
        if resolved.is_err() {
            self.requests.release_command_holds(&held);
        }
        resolved
    }

    pub(super) fn release(
        &self,
        plan: &crate::request::readiness::Plan<(RequestId, crate::request::WatchRequirement)>,
    ) {
        self.requests.release_command_holds(
            &plan
                .leaves()
                .map(|(_, (request, _))| *request)
                .collect::<Vec<_>>(),
        );
    }

    async fn settle(self, job: String, request: RequestId, start: Option<CommandStart>) {
        let source = match start {
            Some(CommandStart { probe, command }) => {
                let source = match probe {
                    Some(probe) => self.probe(&probe).await,
                    None => None,
                };
                dispatch_backend(&self.deployments, command);
                source
            }
            None => None,
        };
        let result = match self.jobs.owner(&job) {
            Ok(owner) => match self.jobs.finished(owner, &job).await {
                Ok(result) => result,
                Err(error) => CommandResult {
                    outcome: CommandOutcome::CommandUnconfirmed(format!("{error:?}")),
                    cleanup: CommandCleanup::CommandCleanupUnknown(format!("{error:?}")),
                },
            },
            Err(error) => CommandResult {
                outcome: CommandOutcome::CommandUnconfirmed(format!("{error:?}")),
                cleanup: CommandCleanup::CommandCleanupUnknown(format!("{error:?}")),
            },
        };
        let report = self.collect(&job, source, result).await;
        let text = render_report(&job, &report);
        let revision = report
            .source
            .as_ref()
            .and_then(|source| source.commit.clone());
        if let Err(error) = self.jobs.record_report(&job, report.clone()) {
            tracing::warn!(?error, %job, "command report not retained");
        }
        let notifications = self
            .requests
            .settle_command(request, text, revision, Some(report));
        publish_request_notifications(&self.requests, &self.deployments, notifications).await;
    }

    async fn probe(&self, probe: &str) -> Option<CommandSource> {
        let owner = self.jobs.owner(probe).ok()?;
        let answered = async {
            // The host supplies a backend promptly or fails the job; only the
            // resource admission after that is bounded.
            self.jobs.supplied(probe).await.ok()?;
            tokio::time::timeout(SOURCE_PROBE_ADMISSION_TIMEOUT, self.jobs.admitted(probe))
                .await
                .ok()?
                .ok()?;
            tokio::time::timeout(SOURCE_PROBE_TIMEOUT, self.jobs.finished(owner, probe))
                .await
                .ok()?
                .ok()
        };
        match answered.await {
            Some(CommandResult {
                outcome: CommandOutcome::CommandExited(0),
                ..
            }) => {}
            Some(_) => return None,
            None => {
                tracing::warn!(%probe, "command source probe did not answer in time");
                // best-effort: the probe's outcome no longer matters.
                drop(
                    self.jobs
                        .control(owner, probe, crate::command_jobs::CommandControl::Cancel)
                        .await,
                );
                return None;
            }
        }
        let page = self
            .jobs
            .read(
                owner,
                probe,
                CommandStream::Stdout,
                CommandPosition::OutputBeginning,
            )
            .await
            .ok()?;
        parse_probe(&page.text)
    }

    async fn collect(
        &self,
        job: &str,
        source: Option<CommandSource>,
        result: CommandResult,
    ) -> CommandReport {
        let stdout = self.stream(job, CommandStream::Stdout).await;
        let stderr = self.stream(job, CommandStream::Stderr).await;
        let mut tail = String::new();
        for (name, evidence) in [("stdout", &stdout), ("stderr", &stderr)] {
            if !evidence.tail.trim().is_empty() {
                tail.push_str(&format!("{name} tail:\n{}\n", evidence.tail.trim_end()));
            }
        }
        for (name, evidence) in [("stdout", &stdout), ("stderr", &stderr)] {
            if let Err(gap) = &evidence.complete {
                tail.push_str(&format!("{name} incomplete: {gap}\n"));
            }
        }
        CommandReport {
            command: self.jobs.command(job).unwrap_or_default(),
            source,
            result,
            output_complete: stdout.complete.is_ok() && stderr.complete.is_ok(),
            tail,
        }
    }

    async fn stream(&self, job: &str, stream: CommandStream) -> StreamEvidence {
        let reader = match self.jobs.owner(job) {
            Ok(owner) => owner,
            Err(error) => {
                return StreamEvidence {
                    complete: Err(format!("output unavailable: {error:?}")),
                    tail: String::new(),
                };
            }
        };
        let tail = match self
            .jobs
            .read(reader, job, stream.clone(), CommandPosition::OutputTail)
            .await
        {
            Ok(page) => page,
            Err(error) => {
                return StreamEvidence {
                    complete: Err(format!("output unavailable: {error:?}")),
                    tail: String::new(),
                };
            }
        };
        let complete = if !tail.finished {
            Err("the stream did not reach end of file".to_owned())
        } else if tail.retained_start > 0 || tail.lost_bytes > 0 {
            Err(format!(
                "{} bytes were not retained",
                tail.retained_start.max(tail.lost_bytes)
            ))
        } else if tail.start > 0 {
            self.contiguous(reader, job, stream, tail.start).await
        } else {
            Ok(())
        };
        StreamEvidence {
            complete,
            tail: tail_text(&tail),
        }
    }

    /// Whether bytes `0..end` are retained without a gap, read forward page
    /// by page. A gap is reported by the page that skips it.
    async fn contiguous(
        &self,
        reader: ActorRef,
        job: &str,
        stream: CommandStream,
        end: i64,
    ) -> Result<(), String> {
        let mut offset = 0;
        for _ in 0..CONTIGUITY_PAGE_BUDGET {
            let page = self
                .jobs
                .read(
                    reader,
                    job,
                    stream.clone(),
                    CommandPosition::OutputOffset(offset),
                )
                .await
                .map_err(|error| format!("output unavailable: {error:?}"))?;
            if page.lost_bytes > 0 || page.start > offset {
                return Err(format!(
                    "{} bytes were not retained after byte {offset}",
                    page.lost_bytes.max(page.start - offset)
                ));
            }
            if page.end >= end {
                return Ok(());
            }
            if page.end <= offset {
                return Err(format!("retained output stops at byte {offset}"));
            }
            offset = page.end;
        }
        Err(format!("contiguity not verified beyond byte {offset}"))
    }
}

struct StreamEvidence {
    complete: Result<(), String>,
    tail: String,
}

fn tail_text(page: &CommandPage) -> String {
    let text = page.text.as_str();
    let mut start = text.len().saturating_sub(TAIL_BYTES);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let text = &text[start..];
    let lines = text.lines().collect::<Vec<_>>();
    lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n")
}

/// The argv as the model wrote it: a `bash` tool script is shown as its
/// script, anything else as its words.
fn command_text(argv: &[String]) -> String {
    let text = match argv {
        [bash, _, _, flag, script, ..] if bash == "bash" && flag == "-c" => {
            script.trim().to_owned()
        }
        words => words
            .iter()
            .map(|word| {
                if word.is_empty() || word.chars().any(char::is_whitespace) {
                    format!("{word:?}")
                } else {
                    word.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    };
    crate::workbench_display::bounded_output(&text, COMMAND_DISPLAY_BYTES)
}

fn outcome_text(outcome: &CommandOutcome) -> String {
    match outcome {
        CommandOutcome::CommandExited(code) => format!("exit {code}"),
        CommandOutcome::CommandSignalled(signal) => format!("killed by signal {signal}"),
        CommandOutcome::CommandOutOfMemory(mib) => format!("out of memory at {mib} MiB"),
        CommandOutcome::CommandCancelled => "cancelled".into(),
        CommandOutcome::CommandFailed(detail) => format!("did not run: {detail}"),
        CommandOutcome::CommandUnconfirmed(detail) => format!("outcome unconfirmed: {detail}"),
    }
}

fn cleanup_text(cleanup: &CommandCleanup) -> String {
    match cleanup {
        CommandCleanup::CommandClean => "process and cleanup terminal".into(),
        CommandCleanup::CommandRetained => "cleanup not terminal: descendants retained".into(),
        CommandCleanup::CommandCleanupUnknown(detail) => format!("cleanup unconfirmed: {detail}"),
    }
}

/// The settlement notice body for one finished job. Read by a model on the
/// turn it wakes: every line is a fact it may act on.
pub(super) fn render_report(job: &str, report: &CommandReport) -> String {
    let (output, recovery) = if report.output_complete {
        (
            "output complete",
            format!("Full output: read_output session_id={job}; nothing reruns."),
        )
    } else {
        (
            "output incomplete",
            format!(
                "Output incomplete; read_output session_id={job} has what was retained. Nothing reruns."
            ),
        )
    };
    let source = match &report.source {
        Some(CommandSource {
            directory,
            commit: Some(commit),
            dirty,
        }) => format!(
            "started in {directory} at {commit}{}",
            if *dirty {
                " with uncommitted changes"
            } else {
                ""
            }
        ),
        Some(CommandSource {
            directory,
            commit: None,
            ..
        }) => format!("started in {directory}, not a Git checkout"),
        None => "source revision not recorded".into(),
    };
    format!(
        "command: {}\n{} · {} · {output}\n{source}\n{}{recovery} The checkout was not guarded while it ran; a pass covers this source only, not a later revision.",
        command_text(&report.command),
        outcome_text(&report.result.outcome),
        cleanup_text(&report.result.cleanup),
        report.tail,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_probe_output_parses_directory_commit_and_dirty_state() {
        let commit = "a".repeat(40);
        assert_eq!(
            parse_probe(&format!("/work/tree\n{commit}\ndirty\n")),
            Some(CommandSource {
                directory: "/work/tree".into(),
                commit: Some(commit.clone()),
                dirty: true,
            })
        );
        assert_eq!(
            parse_probe(&format!("/work/tree\n{commit}\nclean\n")).map(|source| source.dirty),
            Some(false)
        );
        assert_eq!(
            parse_probe("/outside\n"),
            Some(CommandSource {
                directory: "/outside".into(),
                commit: None,
                dirty: false,
            })
        );
        assert_eq!(parse_probe(""), None);
    }

    #[test]
    fn source_probe_rejects_incomplete_or_invalid_git_evidence() {
        let sha256 = "b".repeat(64);
        assert_eq!(
            parse_probe(&format!("/work/tree\n{sha256}\nclean\n")).map(|source| source.dirty),
            Some(false)
        );
        assert_eq!(
            parse_probe(&format!("/work/tree\n{}\nclean\n", "a".repeat(39))),
            None
        );
        assert_eq!(
            parse_probe(&format!("/work/tree\n{}\nunknown\n", "a".repeat(40))),
            None
        );
        assert_eq!(
            parse_probe(&format!("/work/tree\n{}\nclean\nextra\n", "a".repeat(40))),
            None
        );
        assert_eq!(
            parse_probe(&format!("/work/tree\n{}\n", "a".repeat(40))),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_git_status_does_not_emit_clean_source_evidence() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let git = directory.path().join("git");
        std::fs::write(
            &git,
            "#!/bin/sh\ncase \"$1\" in\n  rev-parse) printf '%040d\\n' 0 ;;\n  status) exit 7 ;;\n  *) exit 2 ;;\nesac\n",
        )
        .unwrap();
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();

        let output = std::process::Command::new("bash")
            .args(["--noprofile", "--norc", "-c", SOURCE_PROBE])
            .current_dir(directory.path())
            .env("PATH", {
                let mut paths = vec![directory.path().to_path_buf()];
                paths.extend(std::env::split_paths(
                    &std::env::var_os("PATH").unwrap_or_default(),
                ));
                std::env::join_paths(paths).unwrap()
            })
            .output()
            .unwrap();

        assert!(!output.status.success());
        assert_eq!(
            parse_probe(std::str::from_utf8(&output.stdout).unwrap())
                .map(|source| (source.commit, source.dirty)),
            Some((None, false))
        );
    }

    #[test]
    fn report_names_command_outcome_cleanup_output_source_and_recovery() {
        let report = CommandReport {
            command: vec![
                "bash".into(),
                "--noprofile".into(),
                "--norc".into(),
                "-c".into(),
                "cargo test -p crate --lib".into(),
                "exomonad-bash".into(),
            ],
            source: Some(CommandSource {
                directory: "/work/tree".into(),
                commit: Some("0123abcd".into()),
                dirty: true,
            }),
            result: CommandResult {
                outcome: CommandOutcome::CommandExited(101),
                cleanup: CommandCleanup::CommandClean,
            },
            output_complete: true,
            tail: "stdout tail:\ntest result: FAILED\n".into(),
        };
        assert_eq!(
            render_report("job-1", &report),
            "command: cargo test -p crate --lib\nexit 101 · process and cleanup terminal · output complete\nstarted in /work/tree at 0123abcd with uncommitted changes\nstdout tail:\ntest result: FAILED\nFull output: read_output session_id=job-1; nothing reruns. The checkout was not guarded while it ran; a pass covers this source only, not a later revision."
        );
        let incomplete = CommandReport {
            output_complete: false,
            source: None,
            ..report
        };
        let text = render_report("job-1", &incomplete);
        assert!(
            text.contains("output incomplete\nsource revision not recorded\n"),
            "{text}"
        );
        assert!(
            text.contains("Output incomplete; read_output session_id=job-1 has what was retained."),
            "{text}"
        );
        assert!(!text.contains("Full output"), "{text}");
    }

    #[test]
    fn probe_keeps_the_command_location_and_environment_without_optional_locks() {
        let spec = CommandSpec {
            argv: vec!["cargo".into(), "test".into()],
            directory: Some("/work/tree".into()),
            environment: vec![
                ("GIT_OPTIONAL_LOCKS".into(), "1".into()),
                ("KEEP".into(), "yes".into()),
            ],
            memory: 8 * 1024 * 1024 * 1024,
            input: CommandInput::PipeInput,
            source_capture: CommandSourceCapture::CaptureBeforeStart,
        };
        let probe = probe_spec(&spec);
        assert_eq!(probe.directory, spec.directory);
        assert_eq!(
            probe.environment,
            vec![
                ("KEEP".to_owned(), "yes".to_owned()),
                ("GIT_OPTIONAL_LOCKS".to_owned(), "0".to_owned()),
            ]
        );
        assert_eq!(probe.input, CommandInput::ClosedInput);
        assert_eq!(probe.source_capture, CommandSourceCapture::NoCapture);
    }
}
