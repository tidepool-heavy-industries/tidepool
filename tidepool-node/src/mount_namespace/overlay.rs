//! Short mount transitions inside an existing owned namespace.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use rustix::io::Errno;
use rustix::mount::MountFlags;

use super::MountNamespace;

mod root_metadata;
use root_metadata::RootMetadata;

/// A prepared replacement over retained layers, ordered oldest first.
///
/// The resource owner must keep the layers immutable and exclude new native
/// writes through the transition. An open writable FD is an additional kernel
/// refusal boundary, not proof that background work has finished.
#[derive(Debug)]
pub struct OverlayRotation {
    target: CString,
    lower: Vec<CString>,
    upper: CString,
    work: CString,
    backing_target: CString,
    preserved_mounts: Vec<CString>,
}

#[derive(Debug)]
pub enum OverlayRotationOutcome {
    /// The previous overlay is read-only and the replacement is writable.
    Rotated,
    /// The kernel refused the freeze; the original writable view is unchanged.
    Busy,
    /// The freeze failed before changing the mount.
    Unchanged(io::Error),
    /// Replacement failed, and the original writable view was restored.
    Restored(io::Error),
    /// Keep resources and reconcile mount state before publishing or retrying.
    Unconfirmed(String),
}

// The helper uses typed control flow; integers appear only in its fixed-size
// receipt, which must be written without allocating after fork.
enum Transition {
    Rotated,
    Busy,
    Unchanged(Errno),
    Restored(Errno),
    RestoreFailed { replacement: Errno, restore: Errno },
}

impl Transition {
    fn encode(self) -> [i32; 3] {
        match self {
            Self::Rotated => [0, 0, 0],
            Self::Busy => [1, 0, 0],
            Self::Unchanged(error) => [2, error.raw_os_error(), 0],
            Self::Restored(error) => [3, error.raw_os_error(), 0],
            Self::RestoreFailed {
                replacement,
                restore,
            } => [4, replacement.raw_os_error(), restore.raw_os_error()],
        }
    }
}

impl OverlayRotation {
    pub fn prepare(
        target: &Path,
        layers: &[PathBuf],
        upper: &Path,
        work: &Path,
    ) -> io::Result<Self> {
        if !target.is_absolute()
            || target
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
            || layers.is_empty()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid overlay rotation",
            ));
        }
        let paths = layers
            .iter()
            .map(PathBuf::as_path)
            .chain([upper, work])
            .map(Path::canonicalize)
            .collect::<io::Result<Vec<_>>>()?;
        if paths
            .iter()
            .any(|path| !path.is_dir() || target.starts_with(path) || path.starts_with(target))
            || paths.iter().enumerate().any(|(i, a)| {
                paths
                    .iter()
                    .skip(i + 1)
                    .any(|b| a.starts_with(b) || b.starts_with(a))
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "overlapping overlay backing directories",
            ));
        }
        let parent = paths[layers.len()]
            .ancestors()
            .skip(1)
            .find(|parent| paths[layers.len() + 1].starts_with(parent))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "no common backing parent")
            })?;
        let backing_target =
            CString::new(parent.as_os_str().as_bytes()).map_err(io::Error::other)?;
        let aliases = paths
            .iter()
            .map(|path| {
                CString::new(path.as_os_str().as_bytes())
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
            })
            .collect::<io::Result<Vec<_>>>()?;
        Ok(Self {
            target: CString::new(target.as_os_str().as_bytes())
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?,
            lower: aliases[..layers.len()].iter().rev().cloned().collect(),
            upper: aliases[layers.len()].clone(),
            work: aliases[layers.len() + 1].clone(),
            backing_target,
            preserved_mounts: Vec::new(),
        })
    }

    /// Preserve independently owned mount trees below this view. The caller
    /// supplies the mount layout already owned by the process boundary.
    pub fn preserving_mounts(mut self, paths: &[PathBuf]) -> io::Result<Self> {
        let target = Path::new(std::ffi::OsStr::from_bytes(self.target.as_bytes()));
        if paths.iter().any(|path| {
            path == target
                || !path.starts_with(target)
                || path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
        }) || paths.iter().enumerate().any(|(i, a)| {
            paths
                .iter()
                .skip(i + 1)
                .any(|b| a.starts_with(b) || b.starts_with(a))
        }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid preserved mount layout",
            ));
        }
        self.preserved_mounts = paths
            .iter()
            .map(|path| CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other))
            .collect::<io::Result<_>>()?;
        Ok(self)
    }

    fn mount_next(
        &self,
        namespace: &super::Descriptors,
        preserved: &[Option<OwnedFd>],
        source_root: BorrowedFd<'_>,
        root_metadata: &mut RootMetadata,
    ) -> rustix::io::Result<()> {
        use rustix::mount::*;
        // Only the helper receives writable backing aliases. The detached
        // overlay can then be attached to the original workload namespace.
        // SAFETY: the child is single-threaded and does not unshare its FD table.
        unsafe {
            rustix::thread::unshare_unsafe(rustix::thread::UnshareFlags::NEWNS)?;
        }
        mount_change(
            c"/",
            MountPropagationFlags::PRIVATE | MountPropagationFlags::REC,
        )?;
        mount_bind(
            self.backing_target.as_c_str(),
            self.backing_target.as_c_str(),
        )?;
        mount_remount(self.backing_target.as_c_str(), MountFlags::BIND, c"")?;
        let upper = rustix::fs::open(
            self.upper.as_c_str(),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?;
        root_metadata.copy(source_root, upper.as_fd())?;
        let context = fsopen(c"overlay", FsOpenFlags::FSOPEN_CLOEXEC)?;
        for lower in &self.lower {
            fsconfig_set_string(&context, c"lowerdir+", lower.as_c_str())?;
        }
        fsconfig_set_string(&context, c"upperdir", self.upper.as_c_str())?;
        fsconfig_set_string(&context, c"workdir", self.work.as_c_str())?;
        fsconfig_set_flag(&context, c"userxattr")?;
        fsconfig_create(&context)?;
        let mount = fsmount(
            &context,
            FsMountFlags::FSMOUNT_CLOEXEC,
            MountAttrFlags::empty(),
        )?;
        // Assemble the full view privately, then publish it with one graft.
        move_mount(
            &mount,
            c"",
            rustix::fs::CWD,
            self.target.as_c_str(),
            MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH,
        )?;
        for (path, saved) in self.preserved_mounts.iter().zip(preserved) {
            let saved = saved.as_ref().ok_or(Errno::IO)?;
            move_mount(
                saved,
                c"",
                rustix::fs::CWD,
                path.as_c_str(),
                MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH,
            )?;
        }
        let tree = open_tree(
            rustix::fs::CWD,
            self.target.as_c_str(),
            OpenTreeFlags::OPEN_TREE_CLONE
                | OpenTreeFlags::OPEN_TREE_CLOEXEC
                | OpenTreeFlags::AT_RECURSIVE,
        )?;
        namespace.enter_mount_root()?;
        move_mount(
            &tree,
            c"",
            rustix::fs::CWD,
            self.target.as_c_str(),
            MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH,
        )
    }

    // Called after fork: syscall wrappers and stack-only data, no allocation.
    fn apply(
        &self,
        namespace: &super::Descriptors,
        preserved: &mut [Option<OwnedFd>],
        metadata: &mut RootMetadata,
    ) -> Transition {
        let target = self.target.as_c_str();
        // Linux UAPI OVERLAYFS_SUPER_MAGIC. Never remount an ordinary source
        // filesystem, or turn an already frozen view writable during rollback.
        const OVERLAYFS_SUPER_MAGIC: i64 = 0x794c7630;
        match rustix::fs::statfs(target) {
            Ok(stat) if stat.f_type == OVERLAYFS_SUPER_MAGIC => {}
            Ok(_) => return Transition::Unchanged(Errno::INVAL),
            Err(error) => return Transition::Unchanged(error),
        }
        match rustix::fs::statvfs(target) {
            Ok(stat) if !stat.f_flag.contains(rustix::fs::StatVfsMountFlags::RDONLY) => {}
            Ok(_) => return Transition::Unchanged(Errno::ROFS),
            Err(error) => return Transition::Unchanged(error),
        }
        let source_root = match rustix::fs::open(
            target,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        ) {
            Ok(source_root) => source_root,
            Err(error) => return Transition::Unchanged(error),
        };
        for (path, saved) in self.preserved_mounts.iter().zip(preserved.iter_mut()) {
            *saved = match rustix::mount::open_tree(
                rustix::fs::CWD,
                path.as_c_str(),
                rustix::mount::OpenTreeFlags::OPEN_TREE_CLONE
                    | rustix::mount::OpenTreeFlags::OPEN_TREE_CLOEXEC
                    | rustix::mount::OpenTreeFlags::AT_RECURSIVE,
            ) {
                Ok(mount) => Some(mount),
                Err(error) => return Transition::Unchanged(error),
            };
        }
        match rustix::mount::mount_remount(target, MountFlags::RDONLY, c"") {
            Err(Errno::BUSY) => return Transition::Busy,
            Err(error) => return Transition::Unchanged(error),
            Ok(()) => {}
        }
        match self.mount_next(namespace, preserved, source_root.as_fd(), metadata) {
            Ok(()) => Transition::Rotated,
            Err(error) => match namespace
                .enter_mount_root()
                .and_then(|()| rustix::mount::mount_remount(target, MountFlags::empty(), c""))
            {
                Ok(()) => Transition::Restored(error),
                Err(restore) => Transition::RestoreFailed {
                    replacement: error,
                    restore,
                },
            },
        }
    }
}

impl MountNamespace {
    /// Freeze and replace an overlay without restarting its workload.
    ///
    /// Call only while the owning native write-admission gate is held. This
    /// method performs mount mechanics; it does not establish native quiescence
    /// or publish resource metadata. Run it outside the async actor loop.
    pub fn rotate_overlay(&self, rotation: OverlayRotation) -> OverlayRotationOutcome {
        match self.rotate_overlay_inner(rotation) {
            Ok(outcome) => outcome,
            Err(error) => OverlayRotationOutcome::Unconfirmed(error.to_string()),
        }
    }

    fn rotate_overlay_inner(
        &self,
        rotation: OverlayRotation,
    ) -> io::Result<OverlayRotationOutcome> {
        let (read, write) = rustix::pipe::pipe_with(
            rustix::pipe::PipeFlags::CLOEXEC | rustix::pipe::PipeFlags::NONBLOCK,
        )?;
        // Command owns/reaps the short helper. All mount work happens in its
        // pre-exec syscall phase; the shell only exits after capabilities drop.
        let descriptors = self.descriptors.clone();
        let mut preserved = std::iter::repeat_with(|| None)
            .take(rotation.preserved_mounts.len())
            .collect::<Vec<Option<OwnedFd>>>();
        let mut metadata = RootMetadata::new();
        // SAFETY: apply and receipt encoding use syscall wrappers and
        // preconstructed or stack-only arguments, with no allocation or locks.
        let mut command = unsafe {
            self.command_with_setup(Path::new("/"), "/bin/sh".as_ref(), move || {
                let result = rotation
                    .apply(&descriptors, &mut preserved, &mut metadata)
                    .encode();
                let mut frame = [0u8; 12];
                for (word, bytes) in result.iter().zip(frame.chunks_exact_mut(4)) {
                    bytes.copy_from_slice(&word.to_le_bytes());
                }
                loop {
                    match rustix::io::write(&write, &frame) {
                        Err(Errno::INTR) => continue,
                        Ok(12) => return Ok(()),
                        Ok(_) => {
                            return Err(io::Error::from_raw_os_error(Errno::IO.raw_os_error()))
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
            })?
        };
        let status = command
            .args(["-c", ":"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        drop(command);
        let mut frame = [0u8; 12];
        let count = loop {
            match rustix::io::read(&read, &mut frame) {
                Err(Errno::INTR) => continue,
                result => break result?,
            }
        };
        if count != frame.len() {
            return Ok(OverlayRotationOutcome::Unconfirmed(format!(
                "mount helper returned {count} receipt bytes; process result: {status:?}"
            )));
        }
        let mut words = [0; 3];
        for (word, bytes) in words.iter_mut().zip(frame.chunks_exact(4)) {
            *word = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        Ok(match words {
            [0, 0, 0] => OverlayRotationOutcome::Rotated,
            [1, 0, 0] => OverlayRotationOutcome::Busy,
            [2, error, 0] => OverlayRotationOutcome::Unchanged(io::Error::from_raw_os_error(error)),
            [3, error, 0] => OverlayRotationOutcome::Restored(io::Error::from_raw_os_error(error)),
            [4, error, restore] => OverlayRotationOutcome::Unconfirmed(format!(
                "replacement errno {error}; restore errno {restore}"
            )),
            _ => OverlayRotationOutcome::Unconfirmed("invalid mount helper receipt".into()),
        })
    }
}
