//! Transport controls use the real frontend with an immutable substitute worker.
//! This worker issues no compiler products or semantic authority.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods,
    reason = "owned transport controls"
)]

use std::fs;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tidepool_extract_cmd::{
    compiler_host_checkpoint, preflight_compiler_daemon, with_compiler_transaction,
    with_compiler_transaction_cancellable, CompilerTransactionCancellation, ExtractCmd,
    NativeDemand, NativeDemandControl, NativeDemandOwner, NativeDemandResolver, NativeReply,
    ResolvedExtractBin,
};

const FRONTEND: &str = env!("CARGO_BIN_EXE_tidepool-extract");

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

struct Environment(Vec<(&'static str, Option<std::ffi::OsString>)>);
impl Environment {
    fn set() -> Self {
        let keys = [
            "TIDEPOOL_EXTRACT_DAEMON_SOCKET",
            "TIDEPOOL_EXTRACT_REQUIRED_DAEMON_ENDPOINT",
            "TIDEPOOL_EXTRACT_NO_DAEMON",
            "TIDEPOOL_GHC_LIBDIR",
        ];
        let previous = keys
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect();
        for key in keys {
            std::env::remove_var(key);
        }
        std::env::set_var("TIDEPOOL_GHC_LIBDIR", "unused-by-transport-fixture");
        Self(previous)
    }
}
impl Drop for Environment {
    fn drop(&mut self) {
        for (key, value) in self.0.drain(..) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn fixture(root: &Path) -> PathBuf {
    let source = root.join("worker.rs");
    fs::write(
        &source,
        include_str!("../src/test_fixtures/control_ack_worker.rs"),
    )
    .unwrap();
    let output = root.join("fixture");
    assert!(Command::new("rustc")
        .arg(source)
        .arg("-o")
        .arg(&output)
        .status()
        .unwrap()
        .success());
    output
}

fn setup(root: &Path, fixture: &Path, phase: u8) -> ExtractCmd {
    fs::create_dir_all(root).unwrap();
    fs::copy(fixture, root.join("worker")).unwrap();
    fs::copy(fixture, root.join("launcher")).unwrap();
    fs::write(root.join("phase"), [phase]).unwrap();
    fs::write(root.join("frontend-path"), FRONTEND.as_bytes()).unwrap();
    let mut cmd = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved(root.join("launcher")));
    cmd.input(root.join("TransportOnly.hs"));
    fs::write(
        root.join("request-flag"),
        cmd.worker_argv()[0].as_os_str().as_bytes(),
    )
    .unwrap();
    cmd
}

fn daemon(root: &Path, deadline: u64) -> OwnedChild {
    let socket = root.join("compiler.sock");
    let log = fs::File::create(root.join("daemon.log")).unwrap();
    let child = Command::new(FRONTEND)
        .env("TIDEPOOL_EXTRACT_WORKER", root.join("worker"))
        .args([
            "--daemon",
            "--persistent",
            "--workers",
            "1",
            "--request-deadline-secs",
        ])
        .arg(deadline.to_string())
        .arg("--socket")
        .arg(&socket)
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .unwrap();
    let mut child = OwnedChild(child);
    let deadline = Instant::now() + Duration::from_secs(10);
    while preflight_compiler_daemon(&socket).is_err() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "daemon failed: {}",
            fs::read_to_string(root.join("daemon.log")).unwrap()
        );
        assert!(Instant::now() < deadline, "daemon readiness");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::env::set_var("TIDEPOOL_EXTRACT_DAEMON_SOCKET", socket);
    child
}

fn stop(root: &Path, daemon: &mut OwnedChild) {
    assert!(Command::new(FRONTEND)
        .args(["--stop-daemon", "--socket"])
        .arg(root.join("compiler.sock"))
        .status()
        .unwrap()
        .success());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = daemon.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "daemon stop");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::env::remove_var("TIDEPOOL_EXTRACT_DAEMON_SOCKET");
}

fn owner() -> NativeDemandOwner {
    NativeDemandOwner {
        admission: [11; 32],
        world: [12; 32],
        inputs: [13; 32],
    }
}

struct Resolver {
    seen: Vec<(u64, u64)>,
    refuse: bool,
    cancel: Option<CompilerTransactionCancellation>,
}
impl NativeDemandResolver for Resolver {
    fn resolve_native_demand(
        &mut self,
        demand: &NativeDemand,
        control: &NativeDemandControl,
    ) -> std::io::Result<NativeReply> {
        // Existing semantic helpers use this checkpoint while the actual scoped
        // transaction is active. It must not borrow that transaction again.
        compiler_host_checkpoint()?;
        control.checkpoint()?;
        let nested = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved(
            "native-reentry-must-never-spawn",
        ));
        for refused in [nested.bind(), nested.bind_direct()] {
            let error = refused.unwrap_err();
            assert!(error.definitely_unsubmitted());
            assert_eq!(error.source.kind(), std::io::ErrorKind::InvalidInput);
        }
        assert_eq!(demand.owner, owner());
        assert_eq!(
            demand.payload,
            format!("selected-stage-{}", demand.stage).as_bytes()
        );
        self.seen.push((demand.stage, demand.sequence));
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
            compiler_host_checkpoint()?;
        }
        if self.refuse {
            Ok(NativeReply::Refused(b"semantic-refusal".to_vec()))
        } else {
            Ok(NativeReply::Resolved(
                format!("resolved-{}", demand.stage).into_bytes(),
            ))
        }
    }
}

fn assert_reaped(root: &Path) {
    let pids = fs::read_to_string(root.join("worker-pids")).unwrap();
    assert!(!pids.is_empty());
    for pid in pids.lines() {
        assert!(
            !Path::new("/proc").join(pid).exists(),
            "unreaped owned worker {pid}"
        );
    }
}

#[test]
fn native_demand_actual_direct_and_daemon_preserve_terminal_counts_and_refusal() {
    let _environment = Environment::set();
    let scratch = tempfile::tempdir().unwrap();
    let fixture = fixture(scratch.path());
    for resident in [false, true] {
        for refuse in [false, true] {
            let root = scratch
                .path()
                .join(format!("resident-{resident}-refuse-{refuse}"));
            let command = setup(&root, &fixture, 20);
            let mut daemon = resident.then(|| daemon(&root, 5));
            let mut resolver = Resolver {
                seen: Vec::new(),
                refuse,
                cancel: None,
            };
            let outcome = with_compiler_transaction(
                |_| {},
                || {
                    command.bind().unwrap().execute_with_native_demand(
                        &command,
                        owner(),
                        &mut resolver,
                    )
                },
            );
            let run = outcome.action.unwrap();
            assert_eq!(run.success(), !refuse);
            assert!(outcome.close.is_clean(), "{:?}", outcome.close);
            assert_eq!(
                resolver.seen,
                if refuse {
                    vec![(0, 1)]
                } else {
                    vec![(0, 1), (7, 2)]
                }
            );
            if !refuse {
                assert_eq!(run.output.stdout, b"native-complete");
            }
            assert_eq!(
                fs::read(root.join("reply-1")).unwrap(),
                if refuse {
                    b"semantic-refusal".as_slice()
                } else {
                    b"resolved-0".as_slice()
                }
            );
            if let Some(daemon) = &mut daemon {
                stop(&root, daemon);
            }
            assert_reaped(&root);
        }
    }
}

#[test]
fn native_demand_actual_transports_poison_malformed_frames_and_reap_workers() {
    let _environment = Environment::set();
    let scratch = tempfile::tempdir().unwrap();
    let fixture = fixture(scratch.path());
    for resident in [false, true] {
        for phase in [21, 22, 23, 24, 25] {
            let root = scratch
                .path()
                .join(format!("resident-{resident}-phase-{phase}"));
            let command = setup(&root, &fixture, phase);
            let mut daemon = resident.then(|| daemon(&root, 2));
            let mut resolver = Resolver {
                seen: Vec::new(),
                refuse: false,
                cancel: None,
            };
            let outcome = with_compiler_transaction(
                |_| {},
                || {
                    command.bind().unwrap().execute_with_native_demand(
                        &command,
                        owner(),
                        &mut resolver,
                    )
                },
            );
            let error = outcome.action.unwrap_err();
            assert!(!error.definitely_unsubmitted(), "phase {phase}: {error}");
            assert!(!error.permits_rebind());
            assert!(!outcome.close.is_clean());
            if let Some(daemon) = &mut daemon {
                stop(&root, daemon);
            }
            assert_reaped(&root);
        }
    }
}

#[test]
fn native_demand_resolver_cancellation_observes_real_host_checkpoint_and_retirement() {
    let _environment = Environment::set();
    let scratch = tempfile::tempdir().unwrap();
    let fixture = fixture(scratch.path());
    for resident in [false, true] {
        let root = scratch.path().join(format!("resident-{resident}"));
        let command = setup(&root, &fixture, 20);
        let mut daemon = resident.then(|| daemon(&root, 20));
        let cancellation = CompilerTransactionCancellation::new();
        let mut resolver = Resolver {
            seen: Vec::new(),
            refuse: false,
            cancel: Some(cancellation.clone()),
        };
        let input = std::sync::Arc::new(tempfile::tempdir().unwrap());
        let input_path = input.path().join("original-input");
        fs::write(&input_path, b"original input owner").unwrap();
        let outcome = with_compiler_transaction_cancellable(
            cancellation,
            |_| {},
            || {
                assert!(tidepool_extract_cmd::retain_compiler_input_directory(input));
                command
                    .bind()
                    .unwrap()
                    .execute_with_native_demand(&command, owner(), &mut resolver)
            },
        );
        assert!(outcome.action.is_err());
        assert!(!outcome.close.is_clean());
        assert_eq!(
            outcome.close.resources_retired(),
            resident,
            "{:?}",
            outcome.close
        );
        if resident {
            assert!(!input_path.exists());
        } else {
            assert_eq!(fs::read(&input_path).unwrap(), b"original input owner");
        }
        drop(outcome.close);
        assert!(
            !input_path.exists(),
            "original input custody was not released with its close evidence"
        );
        assert_eq!(resolver.seen, [(0, 1)]);
        if let Some(daemon) = &mut daemon {
            stop(&root, daemon);
        }
        assert_reaped(&root);
    }
}

fn ticks() -> u64 {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: an initialized timespec is exclusively owned by this call.
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut value) },
        0
    );
    value.tv_sec as u64 * 1_000_000_000 + value.tv_nsec as u64
}

/// Independent raw client: holds the reply writer open after demand delivery.
/// Worker death cannot unblock this pipe, so only the relay's original bound
/// can settle it. Both actual frontend modes must reap their worker.
#[test]
fn native_demand_actual_direct_and_daemon_stalled_reply_keep_original_deadline() {
    use std::os::unix::net::UnixStream;
    let _environment = Environment::set();
    let scratch = tempfile::tempdir().unwrap();
    let fixture = fixture(scratch.path());
    for resident in [false, true] {
        for disconnected in [false, true] {
            let root = scratch
                .path()
                .join(format!("resident-{resident}-disconnect-{disconnected}"));
            let command = setup(&root, &fixture, 20);
            let mut daemon = resident.then(|| daemon(&root, if disconnected { 10 } else { 1 }));
            let mut direct = None;
            let mut socket_write_half = None;
            let (mut input, mut output): (Box<dyn Write>, Box<dyn Read>) = if resident {
                let epoch = preflight_epoch(&root);
                let mut socket = UnixStream::connect(root.join("compiler.sock")).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(4)))
                    .unwrap();
                socket.write_all(b"TPDTR006").unwrap();
                socket.write_all(&epoch).unwrap();
                socket.write_all(&[0, 0]).unwrap();
                let mut accepted = [0; 9];
                socket.read_exact(&mut accepted).unwrap();
                assert_eq!(accepted[0], 1);
                let mut started = [0; 69];
                socket.read_exact(&mut started).unwrap();
                assert_eq!(started[0], 3);
                socket_write_half = Some(socket.try_clone().unwrap());
                (Box::new(socket.try_clone().unwrap()), Box::new(socket))
            } else {
                let mut child = OwnedChild(
                    Command::new(FRONTEND)
                        .env("TIDEPOOL_EXTRACT_WORKER", root.join("worker"))
                        .arg("--compiler-endpoint-v1")
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::null())
                        .spawn()
                        .unwrap(),
                );
                let mut writer = child.0.stdin.take().unwrap();
                let mut reader = child.0.stdout.take().unwrap();
                let mut identity = [0; 72];
                reader.read_exact(&mut identity).unwrap();
                assert_eq!(&identity[..8], b"TPCID002");
                writer.write_all(b"TPDTR003").unwrap();
                writer.flush().unwrap();
                let mut begin = [0];
                reader.read_exact(&mut begin).unwrap();
                assert_eq!(begin, [1]);
                direct = Some(child);
                (Box::new(writer), Box::new(reader))
            };
            let start = Instant::now();
            input.write_all(&[6]).unwrap();
            input.write_all(b"TPNDX001").unwrap();
            input.write_all(&[7; 32]).unwrap();
            let owner = owner();
            input.write_all(&owner.admission).unwrap();
            input.write_all(&owner.world).unwrap();
            input.write_all(&owner.inputs).unwrap();
            // The daemon's one-second bound must tighten this original five-second
            // client allowance once, without waiting the full client allowance.
            let deadline = ticks()
                + if disconnected {
                    20_000_000_000
                } else if resident {
                    5_000_000_000
                } else {
                    200_000_000
                };
            input.write_all(&deadline.to_le_bytes()).unwrap();
            frame(&mut input, root.as_os_str().as_bytes());
            let argv = command.worker_argv();
            input.write_all(&(argv.len() as u32).to_le_bytes()).unwrap();
            for arg in argv {
                frame(&mut input, arg.as_os_str().as_bytes());
            }
            input.flush().unwrap();
            let mut demand = [0; 161];
            output.read_exact(&mut demand).unwrap();
            assert_eq!(demand[0], 4);
            let observed_deadline = u64::from_le_bytes(demand[137..145].try_into().unwrap());
            assert!(observed_deadline <= deadline);
            if resident {
                assert!(observed_deadline < deadline);
            }
            let mut length = [0; 4];
            output.read_exact(&mut length).unwrap();
            let mut payload = vec![0; u32::from_le_bytes(length) as usize];
            output.read_exact(&mut payload).unwrap();
            assert_eq!(payload, b"selected-stage-0");
            // The disconnect control has a long deadline so it must independently
            // unblock the relay. Otherwise retain the writer through expiry.
            let held_input = if disconnected {
                if let Some(socket) = &socket_write_half {
                    socket.shutdown(std::net::Shutdown::Write).unwrap();
                }
                drop(input);
                None
            } else {
                Some(input)
            };
            let mut terminal = [0];
            let observed = output.read(&mut terminal).unwrap();
            if resident {
                assert_eq!(observed, 1);
                assert_eq!(terminal, [1]);
            } else {
                assert_eq!(observed, 0);
            }
            assert!(start.elapsed() < Duration::from_secs(3));
            if let Some(child) = &mut direct {
                assert!(!child.0.wait().unwrap().success());
            }
            drop(held_input);
            drop(output);
            if let Some(daemon) = &mut daemon {
                stop(&root, daemon);
            }
            assert_reaped(&root);
        }
    }
}

fn frame(writer: &mut impl Write, bytes: &[u8]) {
    writer
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .unwrap();
    writer.write_all(bytes).unwrap();
}

fn preflight_epoch(root: &Path) -> [u8; 32] {
    let mut socket = std::os::unix::net::UnixStream::connect(root.join("compiler.sock")).unwrap();
    socket.write_all(b"TPDPF001").unwrap();
    let mut identity = [0; 104];
    socket.read_exact(&mut identity).unwrap();
    assert_eq!(&identity[..8], b"TPDPI004");
    identity[72..104].try_into().unwrap()
}

#[test]
fn world_preparation_on_eof_endpoint_refuses_before_worker_work() {
    use std::sync::Arc;
    use tidepool_extract_cmd::{WorldCellPrepareRequest, WorldCellRecipe, WorldRealizationRef};
    let _environment = Environment::set();
    let scratch = tempfile::tempdir().unwrap();
    let fixture = fixture(scratch.path());
    let root = scratch.path().join("one-shot");
    setup(&root, &fixture, 20);
    let mut command =
        ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved(root.join("launcher")));
    // Valid transport-only data cannot confer the missing exchange capability.
    command
        .world_cell_prepare(WorldCellPrepareRequest {
            world: "a".repeat(64),
            realization: WorldRealizationRef::parse(&format!("{}.1", "b".repeat(64))).unwrap(),
            recipe: Arc::new(WorldCellRecipe {
                source: "pure 1".into(),
                template: "{{CELL_BODY}}".into(),
                turn_templates: Vec::new(),
            }),
            operand: None,
            plan: Arc::from(&b"transport-plan"[..]),
            admission: [4; 32],
            defining_scope: 1,
            slots: Vec::new(),
            directory: root.join("operation"),
            output: root.join("prepared"),
        })
        .unwrap();
    let error = command
        .bind_direct()
        .unwrap()
        .execute(&command)
        .unwrap_err();
    assert!(error.definitely_unsubmitted());
    assert_eq!(error.source.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        !root.join("worker-pids").exists(),
        "capability refusal started native worker work"
    );
}
