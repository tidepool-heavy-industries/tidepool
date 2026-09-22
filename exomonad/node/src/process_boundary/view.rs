//! Prepare a filesystem before actor bootstrap, retaining descriptors rather
//! than a keeper process. This never starts a model or an interactive worker.

use std::io;
use std::os::fd::OwnedFd;
use std::process::{Child, Command, Stdio};
use std::time::Instant;

use super::{OverlayMountMode, ProcessInvocation, ProcessMountBoundary};
use crate::{MountNamespace, OverlayRotation};

struct Bootstrap(Child);

impl Drop for Bootstrap {
    fn drop(&mut self) {
        // Only this directly owned, fixed bootstrap command is affected. An
        // error is not a resource-cleanup receipt for its backing directories.
        drop(self.0.stdin.take());
        let _ = self.0.kill();
        let _ = self.0.try_wait();
    }
}

impl ProcessMountBoundary {
    /// Materialize this complete view before running an actor's entry program.
    /// The fixed bootstrap announces readiness only after all mounts and cwd
    /// setup succeed, then exits once its descriptors have been acquired.
    ///
    /// The caller retains the backing resources before invoking this operation;
    /// failure does not prove storage is reclaimable. The returned handle owns
    /// filesystem access, not live native-process or publication authority.
    pub fn prepare_view(
        &self,
        bubblewrap: impl Into<String>,
        deadline: Instant,
    ) -> io::Result<MountNamespace> {
        if deadline <= Instant::now() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "view preparation deadline elapsed",
            ));
        }
        let mut bootstrap_boundary = self.clone();
        let mut empty_lowers = Vec::new();
        for overlay in &mut bootstrap_boundary.overlay_views {
            if overlay.layers.len() == 1 {
                let parent = overlay
                    .upper
                    .parent()
                    .ok_or_else(|| io::Error::other("overlay upper has no parent"))?;
                let empty = tempfile::Builder::new()
                    .prefix("bootstrap-empty-")
                    .tempdir_in(parent)?;
                overlay.layers.insert(0, empty.path().to_owned());
                empty_lowers.push(empty);
            }
        }
        let invocation = bootstrap_boundary.wrap_with_options(
            bubblewrap.into(),
            ProcessInvocation {
                program: "/bin/sh".into(),
                args: vec![
                    "-c".into(),
                    "printf '%s\n' \"$$\"; read release || exit 0".into(),
                ],
            },
            &[],
            OverlayMountMode::Prepared,
        );
        let mut bootstrap = Bootstrap(
            Command::new(invocation.program)
                .args(invocation.args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()?,
        );
        let stdout: OwnedFd = bootstrap
            .0
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("view bootstrap stdout unavailable"))?
            .into();
        let mut receipt = Vec::new();
        loop {
            super::service_scope::wait_readable(&stdout, deadline)?;
            let mut bytes = [0; 32];
            match rustix::io::read(&stdout, &mut bytes) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "view bootstrap exited before readiness",
                    ))
                }
                Ok(count) => receipt.extend_from_slice(&bytes[..count]),
                Err(rustix::io::Errno::INTR) => continue,
                Err(error) => return Err(error.into()),
            }
            if receipt.len() > 32 {
                return Err(io::Error::other("oversized view bootstrap receipt"));
            }
            if receipt.ends_with(b"\n") {
                break;
            }
        }
        let pid = std::str::from_utf8(&receipt)
            .map_err(io::Error::other)?
            .trim()
            .parse()
            .map_err(io::Error::other)?;
        let namespace = MountNamespace::capture_bootstrap(pid, bootstrap.0.id())?;
        let mut overlays = self.overlay_views.iter().collect::<Vec<_>>();
        overlays.sort_by_key(|overlay| overlay.target.components().count());
        for overlay in overlays {
            let preserved = self.preserved_mounts_under(&overlay.target);
            let rotation = OverlayRotation::prepare(
                &overlay.target,
                &overlay.layers,
                &overlay.upper,
                &overlay.work,
            )?
            .preserving_mounts(&preserved)?;
            namespace.mount_initial_overlay(
                rotation,
                self.project_read_only && overlay.target == self.project_root,
            )?;
        }
        let monitor_pid = i32::try_from(bootstrap.0.id())
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .ok_or_else(|| io::Error::other("invalid bootstrap monitor PID"))?;
        let monitor =
            rustix::process::pidfd_open(monitor_pid, rustix::process::PidfdFlags::empty())?;
        drop(bootstrap.0.stdin.take());
        super::service_scope::wait_readable(&monitor, deadline)?;
        if !bootstrap.0.wait()?.success() {
            return Err(io::Error::other("view bootstrap failed after capture"));
        }
        // The covered read-only mount retains these empty lowerdirs until the
        // namespace retires; their resource owner removes them afterward.
        for lower in empty_lowers {
            let _path = lower.keep();
        }
        Ok(namespace)
    }
}
