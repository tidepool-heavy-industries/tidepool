//! A short-lived reference for a separately launched process to acquire an
//! existing view. This is not a durable namespace or a process-launch receipt.

use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use rustix::fs::{Mode, OFlags};

use super::{Descriptors, MountNamespace, ViewIdentity};

/// A reference to descriptors retained by the exporting host. The exporter must
/// retain its `MountNamespace` until the receiving process acquires the view.
/// Deserialization conveys no mount capabilities: acquisition checks the holder
/// incarnation and every kernel view identity before preparing an unprivileged
/// command. An expired reference cannot be used to rediscover a replacement.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct NamespaceEntry {
    version: u32,
    preparation: Option<Box<NamespaceEntry>>,
    boot: String,
    holder: u32,
    start_ticks: u64,
    descriptors: [i32; 3],
    identity: ViewIdentity,
}

impl MountNamespace {
    pub fn entry(&self) -> io::Result<NamespaceEntry> {
        let holder = std::process::id();
        let directory = rustix::fs::open(
            format!("/proc/{holder}"),
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        Ok(NamespaceEntry {
            version: 2,
            preparation: self
                .preparation
                .as_ref()
                .map(|descriptors| {
                    MountNamespace {
                        descriptors: descriptors.clone(),
                        preparation: None,
                    }
                    .entry()
                    .map(Box::new)
                })
                .transpose()?,
            boot: std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?,
            holder,
            start_ticks: process_status(&directory)?.start_ticks,
            descriptors: [
                self.descriptors.user.as_raw_fd(),
                self.descriptors.mount.as_raw_fd(),
                self.descriptors.root.as_raw_fd(),
            ],
            identity: self.view_identity()?,
        })
    }
}

impl NamespaceEntry {
    /// Acquire independent descriptors before constructing the command. The
    /// resulting command keeps them alive through namespace entry and spawn.
    pub fn command(&self, directory: &Path, program: &std::ffi::OsStr) -> io::Result<Command> {
        self.acquire()?.host_command(directory, program)
    }

    /// Acquire the retained view after checking the exporting process and FDs.
    pub fn acquire(&self) -> io::Result<MountNamespace> {
        if self.version != 2
            || self.boot != std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?
            || self.descriptors.iter().any(|fd| *fd < 0)
        {
            return Err(io::Error::other("invalid or expired namespace entry"));
        }
        let pid = i32::try_from(self.holder)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .ok_or_else(|| io::Error::other("invalid namespace holder PID"))?;
        let proc = rustix::fs::open(
            format!("/proc/{pid}"),
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let process = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty())?;
        // Read after pinning the process, relative to the original proc inode.
        if process_status(&proc)?.start_ticks != self.start_ticks {
            return Err(io::Error::other("namespace holder incarnation changed"));
        }
        let open = |fd, flags| {
            rustix::fs::openat(
                &proc,
                format!("fd/{fd}"),
                flags | OFlags::CLOEXEC,
                Mode::empty(),
            )
        };
        let preparation = match &self.preparation {
            Some(entry) if entry.preparation.is_none() => Some(entry.acquire()?.descriptors),
            Some(_) => return Err(io::Error::other("nested preparation authority")),
            None => None,
        };
        let namespace = MountNamespace {
            preparation,
            descriptors: Arc::new(Descriptors {
                user: open(self.descriptors[0], OFlags::RDONLY)?,
                mount: open(self.descriptors[1], OFlags::RDONLY)?,
                root: open(self.descriptors[2], OFlags::PATH | OFlags::DIRECTORY)?,
                process,
            }),
        };
        namespace.require_live_owner()?;
        if namespace.view_identity()? != self.identity {
            return Err(io::Error::other("retained namespace descriptors changed"));
        }
        Ok(namespace)
    }
}

pub(super) struct ProcessStatus {
    pub start_ticks: u64,
    pub parent: u32,
}

pub(super) fn process_status(directory: &impl std::os::fd::AsFd) -> io::Result<ProcessStatus> {
    let fd = rustix::fs::openat(
        directory,
        "stat",
        OFlags::RDONLY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let mut text = String::new();
    std::fs::File::from(fd).read_to_string(&mut text)?;
    let fields = text
        .rsplit_once(')')
        .ok_or_else(|| io::Error::other("process stat fields unavailable"))?
        .1;
    let field = |index| {
        fields
            .split_whitespace()
            .nth(index)
            .ok_or_else(|| io::Error::other("process stat field unavailable"))
    };
    Ok(ProcessStatus {
        start_ticks: field(19)?.parse().map_err(io::Error::other)?,
        parent: field(1)?.parse().map_err(io::Error::other)?,
    })
}
