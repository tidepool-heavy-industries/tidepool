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

mod entry;
pub use entry::NamespaceEntry;
mod overlay;
pub use overlay::{
    copy_overlay_root_metadata, OverlayRecovery, OverlayRotation, OverlayRotationOutcome,
    PreparedOverlayRotation,
};

/// A retained filesystem view captured from an owned live process.
///
/// This is access to a filesystem, not proof that any process has stopped or
/// that its writable layers may be deleted.
#[derive(Clone, Debug)]
pub struct MountNamespace {
    preparation: Option<Arc<Descriptors>>,
    descriptors: Arc<Descriptors>,
}

#[derive(Clone, Copy)]
enum OwnerRequirement {
    RetainedView,
    LiveProcess,
}

#[derive(Debug)]
struct Descriptors {
    user: OwnedFd,
    mount: OwnedFd,
    root: OwnedFd,
    process: OwnedFd,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ViewIdentity {
    namespaces_and_root: [(u64, u64); 3],
    root_mount: u64,
}

impl Descriptors {
    fn enter_mount_root(&self) -> rustix::io::Result<()> {
        rustix::thread::move_into_link_name_space(
            self.mount.as_fd(),
            Some(LinkNameSpaceType::Mount),
        )?;
        rustix::process::fchdir(&self.root)?;
        rustix::process::chroot(c".")?;
        rustix::process::chdir(c"/")
    }

    fn owner_is_live(&self) -> rustix::io::Result<bool> {
        let mut poll = [PollFd::new(&self.process, PollFlags::IN)];
        Ok(rustix::event::poll(&mut poll, Some(&Timespec::default()))? == 0)
    }
}

impl MountNamespace {
    /// Capture namespaces through a pinned proc directory and confirm that the
    /// process is still live. The caller must already own the supplied process.
    pub fn capture(pid: u32) -> io::Result<Self> {
        Self::capture_inner(pid, None, None)
    }

    /// Match a native receipt against pinned kernel process and namespace state.
    pub fn capture_matching(
        pid: u32,
        start_ticks: u64,
        mount_namespace_inode: u64,
    ) -> io::Result<Self> {
        Self::capture_inner(pid, Some((start_ticks, mount_namespace_inode)), None)
    }

    pub(crate) fn capture_bootstrap(pid: u32, monitor: u32) -> io::Result<Self> {
        Self::capture_inner(pid, None, (pid != monitor).then_some(monitor))
    }

    fn capture_inner(
        pid: u32,
        expected: Option<(u64, u64)>,
        parent: Option<u32>,
    ) -> io::Result<Self> {
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
        if let Some(parent) = parent {
            if entry::process_status(&directory)?.parent != parent {
                return Err(io::Error::other(
                    "view bootstrap is not the owned monitor's child",
                ));
            }
        }
        // The workload may occupy a nested user namespace that does not own
        // its mount namespace. Ask the kernel for the actual mount owner.
        if let Some((start_ticks, mount_namespace_inode)) = expected {
            let observed = entry::process_status(&directory)?.start_ticks;
            if observed != start_ticks || rustix::fs::fstat(&mount)?.st_ino != mount_namespace_inode
            {
                return Err(io::Error::other(
                    "native publication process or mount namespace changed",
                ));
            }
        }
        // SAFETY: GetUserNamespace implements the namespace FD ioctl contract.
        let user = unsafe { rustix::ioctl::ioctl(&mount, GetUserNamespace)? };
        let root = rustix::fs::openat(
            &directory,
            "root",
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let namespace = Self {
            preparation: None,
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

    /// Compare retained kernel namespace and root identities, not current file
    /// contents or process liveness. Separate captures of the same view compare
    /// equal without relying on a PID or the allocation identity of this handle.
    pub fn same_view_as(&self, other: &Self) -> io::Result<bool> {
        Ok(self.view_identity()? == other.view_identity()?)
    }

    /// Bind a freshly captured live publisher to this retained filesystem
    /// authority. Exporter liveness must not substitute for publisher liveness.
    pub fn bind_live_view(&self, observed: Self) -> io::Result<Self> {
        observed.require_live_owner()?;
        if !self.same_view_as(&observed)? {
            return Err(io::Error::other(
                "publisher differs from the activated workspace view",
            ));
        }
        Ok(Self {
            descriptors: observed.descriptors,
            preparation: self.preparation.clone(),
        })
    }

    /// The supervisor alone relates its final view to the preparation whose
    /// OverlayFS superblocks and backing mounts it inherited.
    pub(crate) fn with_preparation(mut self, prepared: &Self) -> Self {
        self.preparation = Some(prepared.descriptors.clone());
        self
    }

    fn view_identity(&self) -> io::Result<ViewIdentity> {
        let mut identities = [(0, 0); 3];
        for (identity, fd) in identities.iter_mut().zip([
            &self.descriptors.user,
            &self.descriptors.mount,
            &self.descriptors.root,
        ]) {
            let stat = rustix::fs::fstat(fd)?;
            *identity = (stat.st_dev, stat.st_ino);
        }
        // Bind roots can share an inode while exposing different nested mounts.
        let stat = rustix::fs::statx(
            &self.descriptors.root,
            c"",
            rustix::fs::AtFlags::EMPTY_PATH,
            rustix::fs::StatxFlags::MNT_ID,
        )?;
        if stat.stx_mask & rustix::fs::StatxFlags::MNT_ID.bits() == 0 {
            return Err(io::Error::other("root mount identity unavailable"));
        }
        Ok(ViewIdentity {
            namespaces_and_root: identities,
            root_mount: stat.stx_mnt_id,
        })
    }

    /// Prepare a trusted host command inside this view. The command retains the
    /// namespace descriptors through spawn and changes directory only after
    /// entering the namespace. Retained descriptors keep access valid after the
    /// captured process exits; this says nothing about remaining writers or safe
    /// layer deletion. Do not use this to bypass actor launch policy.
    pub fn host_command(&self, directory: &Path, program: &std::ffi::OsStr) -> io::Result<Command> {
        // SAFETY: the empty setup callback performs no operations.
        unsafe {
            self.command_with_setup(
                directory,
                program,
                OwnerRequirement::RetainedView,
                || Ok(()),
            )
        }
    }

    /// Inspect a path in this view, including symlinks resolved within its root.
    /// Missing paths are distinct from inaccessible or unavailable views.
    pub fn try_exists(&self, path: &Path) -> io::Result<bool> {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected absolute view path",
            ));
        }
        let path = CString::new(path.as_os_str().as_bytes())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let mut command = self.host_command(Path::new("/"), "/bin/sh".as_ref())?;
        // Run after namespace entry and capability dropping, matching Git access.
        // SAFETY: the callback uses only syscalls and preallocated arguments.
        // Its one-byte stdout receipt cannot fill the pipe while exec waits.
        unsafe {
            command.pre_exec(move || {
                let exists = match rustix::fs::stat(path.as_c_str()) {
                    Ok(_) => true,
                    Err(rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR) => false,
                    Err(error) => return Err(error.into()),
                };
                let receipt = [u8::from(exists)];
                loop {
                    match rustix::io::write(rustix::stdio::stdout(), &receipt) {
                        Ok(1) => return Ok(()),
                        Err(rustix::io::Errno::INTR) => continue,
                        Err(error) => return Err(error.into()),
                        Ok(_) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
                    }
                }
            });
        }
        let output = command.args(["-c", ":"]).output()?;
        match (output.status.success(), output.stdout.as_slice()) {
            (true, [0]) => Ok(false),
            (true, [1]) => Ok(true),
            _ => Err(io::Error::other("unconfirmed namespace path inspection")),
        }
    }

    /// # Safety
    /// `setup` runs after fork and must use only async-signal-safe operations
    /// with preconstructed arguments. It must not allocate or acquire locks.
    unsafe fn command_with_setup(
        &self,
        directory: &Path,
        program: &std::ffi::OsStr,
        owner: OwnerRequirement,
        mut setup: impl FnMut() -> io::Result<()> + Send + Sync + 'static,
    ) -> io::Result<Command> {
        if !directory.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "namespace working directory must be absolute",
            ));
        }
        if matches!(owner, OwnerRequirement::LiveProcess) {
            self.require_live_owner()?;
        }
        let directory = CString::new(directory.as_os_str().as_bytes())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let descriptors = self.descriptors.clone();
        let authority = self
            .preparation
            .clone()
            .unwrap_or_else(|| descriptors.clone());
        let mut command = Command::new(program);
        // SAFETY: only syscall wrappers with preconstructed arguments run
        // between fork and exec. The command retains every referenced FD.
        unsafe {
            command.pre_exec(move || {
                if matches!(owner, OwnerRequirement::LiveProcess) && !descriptors.owner_is_live()? {
                    return Err(io::Error::from_raw_os_error(
                        rustix::io::Errno::SRCH.raw_os_error(),
                    ));
                }
                rustix::thread::move_into_link_name_space(
                    authority.user.as_fd(),
                    Some(LinkNameSpaceType::User),
                )?;
                descriptors.enter_mount_root()?;
                rustix::process::chdir(directory.as_c_str())?;
                setup()?;
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
