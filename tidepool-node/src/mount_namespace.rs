//! Retained namespace access for host-side operations on a mounted workspace.
//!
//! Namespace descriptors, rather than a rendered PID path, keep commands bound
//! to the observed filesystem even if the original process exits or its PID is
//! reused. Actor authority and process retirement remain with their owners.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::fs::{Mode, OFlags};
use rustix::thread::{CapabilitySet, CapabilitySets, LinkNameSpaceType};

/// A retained view into the mount namespace of an owned live process.
///
/// This is access to a filesystem, not proof that any process has stopped or
/// that its writable layers may be deleted.
#[derive(Clone, Debug)]
pub struct MountNamespace {
    descriptors: Arc<Descriptors>,
}

#[derive(Debug)]
struct Descriptors {
    user: OwnedFd,
    mount: OwnedFd,
    root: OwnedFd,
    process: OwnedFd,
}

impl Descriptors {
    fn owner_is_live(&self) -> rustix::io::Result<bool> {
        let mut poll = [PollFd::new(&self.process, PollFlags::IN)];
        Ok(rustix::event::poll(&mut poll, Some(&Timespec::default()))? == 0)
    }
}

impl MountNamespace {
    /// Capture namespaces through a pinned proc directory and confirm that the
    /// process is still live. The caller must already own the supplied process.
    pub fn capture(pid: u32) -> io::Result<Self> {
        let pid = i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid owner PID"))?;
        let directory = rustix::fs::open(
            format!("/proc/{pid}"),
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let process = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty())?;
        // A proc directory opened before PID reuse does not retarget the new
        // process. Opening these entries after pidfd_open rejects that race.
        let mount = rustix::fs::openat(
            &directory,
            "ns/mnt",
            OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        // The workload may occupy a nested user namespace that does not own
        // its mount namespace. Ask the kernel for the actual mount owner.
        // SAFETY: GetUserNamespace implements the namespace FD ioctl contract.
        let user = unsafe { rustix::ioctl::ioctl(&mount, GetUserNamespace)? };
        let root = rustix::fs::openat(
            &directory,
            "root",
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let namespace = Self {
            descriptors: Arc::new(Descriptors {
                user,
                mount,
                root,
                process,
            }),
        };
        namespace.require_live_owner()?;
        Ok(namespace)
    }

    pub fn require_live_owner(&self) -> io::Result<()> {
        if !self.descriptors.owner_is_live()? {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "workspace namespace owner exited",
            ));
        }
        Ok(())
    }

    /// Prepare a trusted host command inside this view. The command retains the
    /// namespace descriptors through spawn and changes directory only after
    /// entering the namespace. Do not use this to bypass actor launch policy.
    pub fn host_command(&self, directory: &Path, program: &std::ffi::OsStr) -> io::Result<Command> {
        if !directory.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "namespace working directory must be absolute",
            ));
        }
        self.require_live_owner()?;
        let directory = CString::new(directory.as_os_str().as_bytes())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let descriptors = self.descriptors.clone();
        let mut command = Command::new(program);
        // SAFETY: only syscall wrappers with preconstructed arguments run
        // between fork and exec. The command retains every referenced FD.
        unsafe {
            command.pre_exec(move || {
                if !descriptors.owner_is_live()? {
                    return Err(io::Error::from_raw_os_error(
                        rustix::io::Errno::SRCH.raw_os_error(),
                    ));
                }
                rustix::thread::move_into_link_name_space(
                    descriptors.user.as_fd(),
                    Some(LinkNameSpaceType::User),
                )?;
                rustix::thread::move_into_link_name_space(
                    descriptors.mount.as_fd(),
                    Some(LinkNameSpaceType::Mount),
                )?;
                rustix::process::fchdir(&descriptors.root)?;
                rustix::process::chroot(c".")?;
                rustix::process::chdir(directory.as_c_str())?;
                // Git and its hooks need filesystem access, not the mount
                // capabilities obtained while entering the user namespace.
                rustix::thread::set_no_new_privs(true)?;
                rustix::thread::set_capabilities(
                    None,
                    CapabilitySets {
                        effective: CapabilitySet::empty(),
                        permitted: CapabilitySet::empty(),
                        inheritable: CapabilitySet::empty(),
                    },
                )?;
                Ok(())
            });
        }
        Ok(command)
    }
}

// NS_GET_USERNS takes no argument and returns a newly owned CLOEXEC FD.
struct GetUserNamespace;

// SAFETY: the opcode takes no pointer and returns an FD on success; rustix
// checks syscall errors before invoking output_from_ptr.
unsafe impl rustix::ioctl::Ioctl for GetUserNamespace {
    type Output = OwnedFd;
    const IS_MUTATING: bool = false;

    fn opcode(&self) -> rustix::ioctl::Opcode {
        rustix::ioctl::opcode::none(0xb7, 0x1)
    }

    fn as_ptr(&mut self) -> *mut std::ffi::c_void {
        std::ptr::null_mut()
    }

    unsafe fn output_from_ptr(
        output: rustix::ioctl::IoctlOutput,
        _: *mut std::ffi::c_void,
    ) -> rustix::io::Result<OwnedFd> {
        // SAFETY: successful NS_GET_USERNS returns a fresh owned descriptor.
        Ok(unsafe { OwnedFd::from_raw_fd(output) })
    }
}
