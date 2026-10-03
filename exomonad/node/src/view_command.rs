//! One command in a retained mount view, launched without forking the host.
//!
//! The host passes a sealed v3 `NamespaceEntry` by descriptor to a small
//! executable. That executable acquires the exact view and replaces itself
//! with the requested program. The host owns the child, pipes, and wait.

use std::collections::BTreeMap;
use std::ffi::{CString, OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Output};

use crate::{MountNamespace, NamespaceEntry, MOUNT_HELPER_COMMAND};

const HELPER_NAME: &str = "exomonad-view-helper";
const ENTER_COMMAND: &str = "__enter_retained_view_command";
const ENTRY_FD: i32 = 3;
const SETUP_FD: i32 = 4;
const MAX_ENTRY_BYTES: i64 = 64 * 1024;

/// The companion executable selected by the current host build.
/// Run launchers retain this file beside the copied host executable.
pub fn helper_path() -> io::Result<PathBuf> {
    let executable = std::env::current_exe()?;
    let directory = executable
        .parent()
        .ok_or_else(|| io::Error::other("current executable has no directory"))?;
    let directory = if directory.file_name() == Some(OsStr::new("deps")) {
        directory
            .parent()
            .ok_or_else(|| io::Error::other("test executable has no target directory"))?
    } else {
        directory
    };
    Ok(directory.join(HELPER_NAME))
}

fn cstring(value: &OsStr) -> io::Result<CString> {
    CString::new(value.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in command argument"))
}

fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).map_err(Into::into)
}

fn high_fd(fd: &impl AsFd) -> io::Result<OwnedFd> {
    // Keep action sources away from targets 0..=4. Every duplicate retains
    // CLOEXEC; posix_spawn's dup2 makes only its target visible to the helper.
    rustix::io::fcntl_dupfd_cloexec(fd, 100).map_err(Into::into)
}

fn sealed_entry(entry: &NamespaceEntry) -> io::Result<File> {
    let fd = rustix::fs::memfd_create(
        c"exomonad-view-entry",
        rustix::fs::MemfdFlags::CLOEXEC | rustix::fs::MemfdFlags::ALLOW_SEALING,
    )?;
    let mut file = File::from(fd);
    serde_json::to_writer(&mut file, entry).map_err(io::Error::other)?;
    if file.metadata()?.len() > MAX_ENTRY_BYTES as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "retained-view entry exceeds control limit",
        ));
    }
    file.seek(SeekFrom::Start(0))?;
    let seals = rustix::fs::SealFlags::SEAL
        | rustix::fs::SealFlags::SHRINK
        | rustix::fs::SealFlags::GROW
        | rustix::fs::SealFlags::WRITE;
    rustix::fs::fcntl_add_seals(&file, seals)?;
    Ok(file)
}

struct FileActions(libc::posix_spawn_file_actions_t);

impl FileActions {
    fn new() -> io::Result<Self> {
        let mut actions = MaybeUninit::uninit();
        // SAFETY: posix_spawn_file_actions_init initializes the output value.
        let error = unsafe { libc::posix_spawn_file_actions_init(actions.as_mut_ptr()) };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        // SAFETY: initialization succeeded.
        Ok(Self(unsafe { actions.assume_init() }))
    }

    fn dup2(&mut self, source: &impl AsRawFd, target: i32) -> io::Result<()> {
        // SAFETY: actions is initialized and source remains open through spawn.
        let error = unsafe {
            libc::posix_spawn_file_actions_adddup2(&mut self.0, source.as_raw_fd(), target)
        };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        Ok(())
    }
}

impl Drop for FileActions {
    fn drop(&mut self) {
        // SAFETY: FileActions is constructed only after successful init.
        unsafe { libc::posix_spawn_file_actions_destroy(&mut self.0) };
    }
}

struct SpawnAttributes(libc::posix_spawnattr_t);

impl SpawnAttributes {
    fn new() -> io::Result<Self> {
        let mut attributes = MaybeUninit::uninit();
        // SAFETY: posix_spawnattr_init initializes the output value.
        let error = unsafe { libc::posix_spawnattr_init(attributes.as_mut_ptr()) };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        // SAFETY: initialization succeeded.
        let mut attributes = Self(unsafe { attributes.assume_init() });
        let mut mask = MaybeUninit::<libc::sigset_t>::uninit();
        let mut defaults = MaybeUninit::<libc::sigset_t>::uninit();
        // SAFETY: sigemptyset initializes both sets before they are passed to
        // posix_spawn; the host's ignored SIGPIPE must not reach the payload.
        unsafe {
            libc::sigemptyset(mask.as_mut_ptr());
            libc::sigemptyset(defaults.as_mut_ptr());
        }
        // SAFETY: defaults is initialized above.
        let mut defaults = unsafe { defaults.assume_init() };
        // SAFETY: sigaddset mutates this owned set.
        if unsafe { libc::sigaddset(&mut defaults, libc::SIGPIPE) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: mask is initialized above; attr is owned and initialized.
        let mask = unsafe { mask.assume_init() };
        for error in [
            unsafe { libc::posix_spawnattr_setsigmask(&mut attributes.0, &mask) },
            unsafe { libc::posix_spawnattr_setsigdefault(&mut attributes.0, &defaults) },
            unsafe {
                libc::posix_spawnattr_setflags(
                    &mut attributes.0,
                    (libc::POSIX_SPAWN_SETSIGMASK | libc::POSIX_SPAWN_SETSIGDEF) as i16,
                )
            },
        ] {
            if error != 0 {
                return Err(io::Error::from_raw_os_error(error));
            }
        }
        Ok(attributes)
    }
}

impl Drop for SpawnAttributes {
    fn drop(&mut self) {
        // SAFETY: constructed only after successful posix_spawnattr_init.
        unsafe { libc::posix_spawnattr_destroy(&mut self.0) };
    }
}

struct Child {
    pid: rustix::process::Pid,
    reaped: bool,
}

impl Child {
    fn wait(&mut self) -> io::Result<ExitStatus> {
        loop {
            match rustix::process::waitpid(Some(self.pid), rustix::process::WaitOptions::empty()) {
                Ok(Some((pid, status))) if pid == self.pid => {
                    self.reaped = true;
                    return Ok(ExitStatus::from_raw(status.as_raw()));
                }
                Ok(Some(_)) => return Err(io::Error::other("waitpid returned a different child")),
                Ok(None) => continue,
                Err(error) => {
                    if error == rustix::io::Errno::INTR {
                        continue;
                    }
                    if error == rustix::io::Errno::CHILD {
                        self.reaped = true;
                    }
                    return Err(error.into());
                }
            }
        }
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        if !self.reaped {
            // Match Command::output ownership: reap this exact child on a
            // failed capture. Git hooks or detached descendants are separate
            // processes and are not falsely reported as settled by this wait.
            rustix::process::kill_process(self.pid, rustix::process::Signal::KILL).ok();
            let _ = self.wait();
        }
    }
}

fn reader(fd: OwnedFd) -> std::thread::JoinHandle<io::Result<Vec<u8>>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        File::from(fd).read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

fn joined(reader: std::thread::JoinHandle<io::Result<Vec<u8>>>) -> io::Result<Vec<u8>> {
    reader
        .join()
        .map_err(|_| io::Error::other("view command output reader panicked"))?
}

/// Run one command in `view` and retain its output and exit status. The host
/// never installs a pre-exec callback; all namespace syscalls run after the
/// small helper has replaced the host's large executable image. Standard input
/// is `/dev/null`; standard output and error are captured as bytes.
pub fn output_in_view(
    view: &MountNamespace,
    directory: &Path,
    program: &OsStr,
    args: &[OsString],
    environment: &[(OsString, Option<OsString>)],
) -> io::Result<Output> {
    if !directory.is_absolute() || program == OsStr::new("/proc/self/exe") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid retained-view command or directory",
        ));
    }
    let entry = sealed_entry(&view.entry()?)?;
    let helper = cstring(helper_path()?.as_os_str())?;
    let mut arguments = vec![
        helper.clone(),
        CString::new(ENTER_COMMAND).expect("fixed marker"),
        cstring(directory.as_os_str())?,
        cstring(program)?,
    ];
    arguments.extend(
        args.iter()
            .map(|arg| cstring(arg.as_os_str()))
            .collect::<io::Result<Vec<_>>>()?,
    );
    let argv = arguments
        .iter()
        .map(|arg| arg.as_ptr().cast_mut())
        .chain(std::iter::once(std::ptr::null_mut()))
        .collect::<Vec<_>>();

    let mut variables: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    for (key, value) in environment {
        if key.as_bytes().is_empty() || key.as_bytes().contains(&b'=') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid environment key",
            ));
        }
        match value {
            Some(value) => {
                variables.insert(key.clone(), value.clone());
            }
            None => {
                variables.remove(key);
            }
        }
    }
    let environment = variables
        .into_iter()
        .map(|(key, value)| {
            let mut row = key.into_encoded_bytes();
            row.push(b'=');
            row.extend(value.into_encoded_bytes());
            CString::new(row)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in environment"))
        })
        .collect::<io::Result<Vec<_>>>()?;
    let envp = environment
        .iter()
        .map(|row| row.as_ptr().cast_mut())
        .chain(std::iter::once(std::ptr::null_mut()))
        .collect::<Vec<_>>();

    let (stdout_read, stdout_write) = pipe()?;
    let (stderr_read, stderr_write) = pipe()?;
    let (setup_read, setup_write) = pipe()?;
    let null = File::open("/dev/null")?;
    let entry_source = high_fd(&entry)?;
    let setup_source = high_fd(&setup_write)?;
    let stdout_source = high_fd(&stdout_write)?;
    let stderr_source = high_fd(&stderr_write)?;
    let null_source = high_fd(&null)?;
    let mut actions = FileActions::new()?;
    actions.dup2(&null_source, libc::STDIN_FILENO)?;
    actions.dup2(&stdout_source, libc::STDOUT_FILENO)?;
    actions.dup2(&stderr_source, libc::STDERR_FILENO)?;
    actions.dup2(&entry_source, ENTRY_FD)?;
    actions.dup2(&setup_source, SETUP_FD)?;
    let attributes = SpawnAttributes::new()?;
    let mut pid = 0;
    // SAFETY: argv/envp are null-terminated and all pointed-to CStrings and
    // file-action sources remain alive for the duration of posix_spawn.
    let error = unsafe {
        libc::posix_spawn(
            &mut pid,
            helper.as_ptr(),
            &actions.0,
            &attributes.0,
            argv.as_ptr(),
            envp.as_ptr(),
        )
    };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error));
    }
    let pid = rustix::process::Pid::from_raw(pid)
        .ok_or_else(|| io::Error::other("posix_spawn returned an invalid child pid"))?;
    let mut child = Child { pid, reaped: false };
    drop(actions);
    drop(entry_source);
    drop(setup_source);
    drop(stdout_source);
    drop(stderr_source);
    drop(null_source);
    drop(stdout_write);
    drop(stderr_write);
    drop(setup_write);
    let stdout = reader(stdout_read);
    let stderr = reader(stderr_read);
    let setup = reader(setup_read);
    let status = child.wait()?;
    let stdout = joined(stdout)?;
    let stderr = joined(stderr)?;
    let setup = joined(setup)?;
    // `R` is written by the helper's final pre-exec callback after namespace
    // entry. A signal in the tiny interval before target exec remains
    // indistinguishable from an equally signalled target; the exit status is
    // reported without inventing confirmation of which process ran.
    if setup.first() != Some(&b'R') || setup.len() > 1 {
        return Err(io::Error::other(format!(
            "retained-view command setup failed: {}",
            String::from_utf8_lossy(&setup)
        )));
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// Entry of the small helper executable. It has no Tokio runtime and only
/// acquires one pre-existing v3 view before replacing itself with the payload.
pub fn helper_main() {
    let mut arguments = std::env::args_os();
    let _executable = arguments.next();
    let marker = arguments.next();
    if marker.as_deref() == Some(OsStr::new(MOUNT_HELPER_COMMAND)) {
        return;
    }
    if marker.as_deref() != Some(OsStr::new(ENTER_COMMAND)) {
        eprintln!("view command helper: unknown mode");
        std::process::exit(2);
    }
    // SAFETY: the parent maps its setup pipe to descriptor 4.
    let mut setup = unsafe { File::from_raw_fd(SETUP_FD) };
    let result = (|| -> io::Result<()> {
        let directory = PathBuf::from(
            arguments
                .next()
                .ok_or_else(|| io::Error::other("missing view directory"))?,
        );
        let program = arguments
            .next()
            .ok_or_else(|| io::Error::other("missing view program"))?;
        if program == OsStr::new("/proc/self/exe") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "recursive view helper payload",
            ));
        }
        // SAFETY: the parent maps descriptor 3 to a sealed memfd. Validate it
        // before JSON decoding so a malformed launcher cannot stream forever.
        let entry_file = unsafe { File::from_raw_fd(ENTRY_FD) };
        let stat = rustix::fs::fstat(&entry_file)?;
        if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile
            || stat.st_size <= 0
            || stat.st_size > MAX_ENTRY_BYTES
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid view entry size",
            ));
        }
        let required = rustix::fs::SealFlags::SEAL
            | rustix::fs::SealFlags::SHRINK
            | rustix::fs::SealFlags::GROW
            | rustix::fs::SealFlags::WRITE;
        if !rustix::fs::fcntl_get_seals(&entry_file)?.contains(required) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsealed view entry",
            ));
        }
        let entry: NamespaceEntry =
            serde_json::from_reader(entry_file).map_err(io::Error::other)?;
        // The command may use descriptor 4 itself. Close it on successful exec
        // but keep it live to report an `exec`/namespace-entry failure.
        rustix::io::fcntl_setfd(&setup, rustix::io::FdFlags::CLOEXEC)?;
        let mut command = entry.command(&directory, &program)?;
        command.args(arguments);
        // SAFETY: this callback runs in the small helper immediately after
        // `NamespaceEntry::command`'s namespace setup and before target exec.
        // `write` is async-signal-safe; one byte fits within PIPE_BUF.
        unsafe {
            command.pre_exec(|| {
                let marker = b"R";
                if libc::write(SETUP_FD, marker.as_ptr().cast(), 1) == 1 {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
            });
        }
        Err(command.exec())
    })();
    if let Err(error) = result {
        let detail = error.to_string();
        let _ = setup.write_all(&detail.as_bytes()[..detail.len().min(4096)]);
        eprintln!("view command helper: {error}");
        std::process::exit(127);
    }
}
