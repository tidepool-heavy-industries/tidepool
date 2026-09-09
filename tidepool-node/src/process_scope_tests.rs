use super::*;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

struct Fixture {
    scope: ServiceScope,
    directory: tempfile::TempDir,
}

impl Fixture {
    fn new(script: &str) -> Self {
        Self::with_sync_mutation(script, false)
    }

    fn with_sync_mutation(script: &str, omit_sync_hold: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path();
        let boundary =
            ProcessMountBoundary::new(path, [path.to_owned()], [path.to_owned()]).unwrap();
        let bwrap = std::env::var_os("SERVICE_SCOPE_BWRAP")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(
                    "/nix/store/dqzmpjz70l4lzg7lmc3x8wih74nh5bpc-bubblewrap-0.11.0/bin/bwrap",
                )
            });
        let mut prepared = boundary
            .prepare_service_scope(
                bwrap,
                ProcessInvocation {
                    program: "/bin/sh".into(),
                    args: vec!["-c".into(), script.into()],
                },
            )
            .unwrap();
        prepared.omit_sync_hold = omit_sync_hold;
        let log = File::create(path.join("output")).unwrap();
        let scope = prepared.spawn(ServiceEnvironment::default(), log).unwrap();
        Self { scope, directory }
    }

    fn pin(&mut self) {
        // Observe the kernel blocking read before pinning/releasing. This is
        // an actual readiness barrier, not an assumed scheduling delay.
        if let Ok(info) = self.scope.read_info(deadline()) {
            let limit = deadline();
            loop {
                let state = std::fs::read_to_string(format!("/proc/{}/wchan", info.pid)).unwrap();
                if state.trim() == "pipe_read" {
                    break;
                }
                assert!(
                    Instant::now() < limit,
                    "init gate wait unavailable: {state}"
                );
                std::thread::yield_now();
            }
            self.absent();
        }
        if let Err(error) = self.scope.pin_init(deadline()) {
            panic!(
                "mounted identity unavailable/failure (NOT skipped): {error}; bwrap output: {}",
                std::fs::read_to_string(self.directory.path().join("output")).unwrap()
            );
        }
    }

    fn observe_still_blocked(&self) {
        // No init signal occurs in this observation interval. Polling its exact
        // witness must time out, then the init must still be blocked in read.
        let error = wait_readable(
            &self.scope.init.as_ref().unwrap().pidfd,
            Instant::now() + Duration::from_millis(250),
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        let pid = self.scope.info_record.as_ref().unwrap().pid;
        assert_eq!(
            std::fs::read_to_string(format!("/proc/{pid}/wchan"))
                .unwrap()
                .trim(),
            "pipe_read"
        );
        self.absent();
    }

    fn absent(&self) {
        assert!(!self.directory.path().join("started").exists());
    }
}

// Test cleanup does not depend on the intentionally corrupted info record.
impl Drop for Fixture {
    fn drop(&mut self) {
        if self.scope.init.is_some() {
            self.scope
                .terminate_and_wait(deadline())
                .expect("fixture exact-witness cleanup");
        }
    }
}

#[test]
fn scope_gate_writer_close_does_not_release() {
    let mut fixture = Fixture::new("echo started > started");
    fixture.pin();
    fixture.absent();
    fixture.scope.gate.take();
    assert!(matches!(
        fixture.scope.release_command(),
        Err(ServiceScopeError::ReleaseUnconfirmed(_))
    ));
    assert!(matches!(
        fixture.scope.observation().unwrap(),
        ScopeObservation::ReleaseUnconfirmed
    ));
    assert!(matches!(
        fixture.scope.release_command(),
        Err(ServiceScopeError::WrongPhase)
    ));
    fixture.observe_still_blocked();
    fixture.scope.terminate_and_wait(deadline()).unwrap();
    fixture.absent();
}

#[test]
fn scope_monitor_death_before_release_does_not_start() {
    let mut fixture = Fixture::new("echo started > started");
    fixture.pin();
    fixture.scope.gate.take();
    fixture.scope.monitor.kill().unwrap();
    fixture.scope.monitor_status = Some(fixture.scope.monitor.wait().unwrap());
    assert!(fixture.scope.cleanup.is_none());
    fixture.observe_still_blocked();
    fixture.scope.terminate_and_wait(deadline()).unwrap();
    fixture.absent();
}

#[test]
fn scope_rejects_stale_and_sibling_info_retains_exact_cleanup() {
    let mut fixture = Fixture::new("echo started > started");
    fixture.pin();
    let witness = fixture.scope.init.as_ref().unwrap().pidfd.as_raw_fd();
    let actual = fixture.scope.info_record.clone().unwrap();
    let mut sibling = Command::new("sleep").arg("30").spawn().unwrap();
    let result = fixture.scope.pin_record(&InitInfo {
        pid: sibling.id(),
        namespace: actual.namespace,
    });
    sibling.kill().unwrap();
    sibling.wait().unwrap();
    assert!(result.is_err());
    assert_eq!(
        fixture.scope.init.as_ref().unwrap().pidfd.as_raw_fd(),
        witness
    );
    let result = fixture.scope.pin_record(&InitInfo {
        pid: std::process::id(),
        namespace: actual.namespace,
    });
    assert!(result.is_err());
    let result = fixture.scope.pin_record(&InitInfo {
        pid: actual.pid,
        namespace: actual.namespace + 1,
    });
    assert!(result.is_err());
    assert_eq!(
        fixture.scope.init.as_ref().unwrap().pidfd.as_raw_fd(),
        witness
    );
    fixture.scope.gate.take();
    fixture.scope.terminate_and_wait(deadline()).unwrap();
    fixture.absent();
}

#[test]
fn scope_timeout_retains_owner_then_confirms_both_facts() {
    let mut fixture = Fixture::new("echo started > started");
    fixture.pin();
    assert!(fixture
        .scope
        .terminate_and_wait(Instant::now() - Duration::from_secs(1))
        .is_err());
    assert!(fixture.scope.init.is_some());
    assert!(fixture.scope.cleanup.is_none());
    assert!(matches!(
        fixture.scope.observation().unwrap(),
        ScopeObservation::Stopping
    ));
    assert!(matches!(
        fixture.scope.release_command(),
        Err(ServiceScopeError::WrongPhase)
    ));
    let cleanup = fixture.scope.terminate_and_wait(deadline()).unwrap();
    assert!(fixture.scope.monitor_status.is_some());
    assert!(matches!(
        fixture.scope.observation().unwrap(),
        ScopeObservation::ProcessStopped(observed)
            if observed.monitor_status() == cleanup.monitor_status()
    ));
    fixture.absent();
}

#[test]
fn scope_released_detached_descendants_and_output_pressure() {
    released_descendants(false);
}

#[test]
fn scope_monitor_death_after_detached_readiness_requires_init_evidence() {
    released_descendants(true);
}

fn released_descendants(kill_monitor: bool) {
    use std::os::unix::net::UnixListener;
    // Python is resolved by the Nix shell. It announces readiness only after
    // setsid grandchild startup and output larger than pipe capacity.
    let python = Command::new("which").arg("python3").output().unwrap();
    assert!(python.status.success());
    let python = String::from_utf8(python.stdout).unwrap();
    let mut fixture = Fixture::new(&format!(
        "exec {} -c '{}'",
        python.trim(),
        include_str!("process_scope_fixture.py")
    ));
    let socket = fixture.directory.path().join("ready");
    let listener = UnixListener::bind(socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    fixture.pin();
    assert!(matches!(
        fixture.scope.observation().unwrap(),
        ScopeObservation::Pinned
    ));
    let _release = fixture.scope.release_command().unwrap();
    assert!(matches!(
        fixture.scope.observation().unwrap(),
        ScopeObservation::Released
    ));
    let limit = deadline();
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < limit,
                    "payload barrier timeout; output: {:?}",
                    std::fs::read_to_string(fixture.directory.path().join("output"))
                );
                std::thread::yield_now();
            }
            Err(error) => panic!("{error}"),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut reader = std::io::BufReader::new(stream);
    let mut pid = String::new();
    std::io::BufRead::read_line(&mut reader, &mut pid).unwrap();
    assert_eq!(pid, "ready\n");
    if kill_monitor {
        fixture.scope.monitor.kill().unwrap();
        fixture.scope.monitor_status = Some(fixture.scope.monitor.wait().unwrap());
        assert!(
            fixture.scope.cleanup.is_none(),
            "monitor wait alone must not issue receipt"
        );
    }
    fixture.scope.terminate_and_wait(deadline()).unwrap();
    let mut rest = String::new();
    reader.read_to_string(&mut rest).unwrap();
    assert!(rest.is_empty());
    assert!(
        fixture
            .directory
            .path()
            .join("output")
            .metadata()
            .unwrap()
            .len()
            >= 1024 * 1024
    );
}

#[test]
fn scope_private_gate_is_blocking_cloexec_and_above_stdio() {
    let (read, write) = private_pipe().unwrap();
    assert!(read.as_raw_fd() > 2 && write.as_raw_fd() > 2);
    assert!(!rustix::fs::fcntl_getfl(&read)
        .unwrap()
        .contains(OFlags::NONBLOCK));
    assert!(rustix::io::fcntl_getfd(&read)
        .unwrap()
        .contains(FdFlags::CLOEXEC));
    assert!(wait_readable(&read, Instant::now()).is_err());
}

#[test]
fn scope_invalid_executable_is_pre_spawn_failure() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path();
    let boundary = ProcessMountBoundary::new(path, [path.to_owned()], [path.to_owned()]).unwrap();
    let invocation = ProcessInvocation {
        program: "/bin/true".into(),
        args: vec![],
    };
    assert!(matches!(
        boundary.prepare_service_scope("bwrap".into(), invocation.clone()),
        Err(ServiceScopeError::ExecutableNotAbsolute)
    ));
    let prepared = boundary
        .prepare_service_scope("/no/such/bwrap".into(), invocation)
        .unwrap();
    assert!(matches!(
        prepared.spawn(Default::default(), tempfile::tempfile().unwrap()),
        Err(ServiceScopeError::NotSpawned(_))
    ));
}

#[test]
fn scope_stale_proc_directory_cannot_validate_replacement() {
    let proc = checked_proc().unwrap();
    let mut child = Command::new("sleep").arg("30").spawn().unwrap();
    let directory = rustix::fs::openat(
        &proc,
        child.id().to_string(),
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    // Opening any replacement pidfd does not revive this retained proc inode.
    let _replacement = rustix::process::pidfd_open(
        rustix::process::Pid::from_raw(std::process::id() as i32).unwrap(),
        rustix::process::PidfdFlags::empty(),
    )
    .unwrap();
    assert!(read_status(&directory).is_err());
}

#[test]
fn scope_non_proc_view_is_explicitly_unsupported() {
    let directory = tempfile::tempdir().unwrap();
    assert!(matches!(
        checked_proc_at(directory.path().to_str().unwrap()),
        Err(ServiceScopeError::Unsupported(_))
    ));
}

#[test]
fn scope_malformed_and_missing_info_never_release() {
    let mut fixture = Fixture::new("echo started > started");
    fixture.pin();
    // Keep the independently acquired witness available for fixture cleanup.
    let (read, write) = private_pipe().unwrap();
    rustix::fs::fcntl_setfl(&read, OFlags::NONBLOCK).unwrap();
    fixture.scope.info = read;
    fixture.scope.info_record = None;
    fixture.scope.info_bytes.clear();
    fixture.scope.phase = Phase::Blocked;
    assert!(fixture.scope.pin_init(Instant::now()).is_err());
    assert!(fixture.scope.release_command().is_err());
    rustix::io::write(&write, b"{invalid").unwrap();
    drop(write);
    assert!(fixture.scope.pin_init(deadline()).is_err());
    assert!(fixture.scope.release_command().is_err());
    fixture.scope.terminate_and_wait(deadline()).unwrap();
    fixture.absent();
}

#[test]
fn scope_omitted_sync_hold_mutation_releases_payload_on_writer_close() {
    let mut fixture = Fixture::with_sync_mutation("echo started > started; exec sleep 30", true);
    fixture.pin();
    fixture.scope.gate.take();
    // Positive detection is the mutation oracle: removing only the self-hold
    // lets EOF launch a command without release_command ever being called.
    let limit = deadline();
    while !fixture.directory.path().join("started").exists() {
        assert!(
            Instant::now() < limit,
            "mutation did not expose EOF release"
        );
        std::thread::yield_now();
    }
    fixture.scope.terminate_and_wait(deadline()).unwrap();
}

#[test]
fn scope_inherited_terminal_job_control_survives_interrupt_and_restores_terminal() {
    const CHILD: &str = "TIDEPOOL_SCOPE_PTY_CHILD";
    if std::env::var_os(CHILD).is_some() {
        inherited_terminal_child();
        return;
    }

    let executable = std::env::current_exe().unwrap();
    let test = "process_boundary::service_scope::tests::scope_inherited_terminal_job_control_survives_interrupt_and_restores_terminal";
    let command = format!(
        "{}=1 '{}' --exact '{}' --nocapture",
        CHILD,
        executable.display().to_string().replace('\'', "'\\''"),
        test
    );
    let status = Command::new("script")
        .args(["-qefc", &command, "/dev/null"])
        .status()
        .expect("util-linux script supplies an actual PTY");
    assert!(status.success(), "PTY child failed with {status}");
}

fn inherited_terminal_child() {
    let stdin = unsafe { BorrowedFd::borrow_raw(0) };
    let owner_group = rustix::termios::tcgetpgrp(stdin).expect("PTY foreground owner");
    let directory = tempfile::tempdir().unwrap();
    let boundary = ProcessMountBoundary::new(
        directory.path(),
        [directory.path().to_owned()],
        [directory.path().to_owned()],
    )
    .unwrap();
    let bwrap = std::env::var_os("SERVICE_SCOPE_BWRAP")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from("/nix/store/dqzmpjz70l4lzg7lmc3x8wih74nh5bpc-bubblewrap-0.11.0/bin/bwrap")
        });
    let mut scope = boundary
        .reserve_service_scope(
            bwrap,
            ProcessInvocation {
                program: "/bin/sh".into(),
                args: vec![
                    "-c".into(),
                    "trap 'exit 0' INT; echo ready > started; while :; do sleep 1; done".into(),
                ],
            },
        )
        .unwrap()
        .spawn_with_stdio(
            ServiceEnvironment::default(),
            ServiceStdio::InheritedTerminal,
        )
        .unwrap();
    scope.pin_init(deadline()).unwrap();
    let child_group = rustix::process::Pid::from_raw(scope.monitor.id() as i32).unwrap();
    assert_ne!(child_group, owner_group);
    assert_eq!(rustix::termios::tcgetpgrp(stdin).unwrap(), child_group);
    scope.release_command().unwrap();
    let limit = deadline();
    while !directory.path().join("started").exists() {
        assert!(Instant::now() < limit, "PTY payload did not start");
        std::thread::yield_now();
    }

    rustix::process::kill_process_group(child_group, rustix::process::Signal::INT).unwrap();
    // Reaching exact cleanup proves the terminal interrupt did not target the
    // helper/test process that owns the ScopeCapability.
    scope.terminate_and_wait(deadline()).unwrap();
    assert_eq!(rustix::termios::tcgetpgrp(stdin).unwrap(), owner_group);
}
