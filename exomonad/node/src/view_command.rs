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
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
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

fn helper_path() -> io::Result<PathBuf> {
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
    let mut pair = [0; 2];
    // SAFETY: `pair` has room for both descriptors returned by pipe2.
    if unsafe { libc::pipe2(pair.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful pipe2 transferred ownership of both descriptors.
    Ok(unsafe { (OwnedFd::from_raw_fd(pair[0]), OwnedFd::from_raw_fd(pair[1])) })
}

fn high_fd(fd: &impl AsRawFd) -> io::Result<OwnedFd> {
    // Keep action sources away from targets 0..=4. Every duplicate retains
    // CLOEXEC; posix_spawn's dup2 makes only its target visible to the helper.
    let duplicate = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 100) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fcntl returned a new descriptor owned by this call.
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

fn sealed_entry(entry: &NamespaceEntry) -> io::Result<File> {
    // SAFETY: memfd_create copies the fixed NUL-terminated name.
    let fd = unsafe {
        libc::memfd_create(
            c"exomonad-view-entry".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful memfd_create returned a new owned descriptor.
    let mut file = unsafe { File::from_raw_fd(fd) };
    serde_json::to_writer(&mut file, entry).map_err(io::Error::other)?;
    if file.metadata()?.len() > MAX_ENTRY_BYTES as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "retained-view entry exceeds control limit",
        ));
    }
    file.seek(SeekFrom::Start(0))?;
    let seals = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
    // SAFETY: F_ADD_SEALS applies to the owned memfd after its final write.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
        return Err(io::Error::last_os_error());
    }
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
    pid: libc::pid_t,
    reaped: bool,
}

impl Child {
    fn wait(&mut self) -> io::Result<ExitStatus> {
        let mut status = 0;
        loop {
            // SAFETY: this owner waits for the exact PID returned by spawn.
            let result = unsafe { libc::waitpid(self.pid, &mut status, 0) };
            if result == self.pid {
                self.reaped = true;
                return Ok(ExitStatus::from_raw(status));
            }
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    if error.raw_os_error() == Some(libc::ECHILD) {
                        self.reaped = true;
                    }
                    return Err(error);
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
            // SAFETY: this PID belongs to the unreaped child.
            unsafe { libc::kill(self.pid, libc::SIGKILL) };
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
        let mut stat = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: fstat initializes `stat` for this owned descriptor.
        if unsafe { libc::fstat(entry_file.as_raw_fd(), stat.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fstat succeeded.
        let stat = unsafe { stat.assume_init() };
        if stat.st_mode & libc::S_IFMT != libc::S_IFREG
            || stat.st_size <= 0
            || stat.st_size > MAX_ENTRY_BYTES
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid view entry size",
            ));
        }
        // SAFETY: F_GET_SEALS queries the owned memfd.
        let seals = unsafe { libc::fcntl(entry_file.as_raw_fd(), libc::F_GET_SEALS) };
        let required =
            libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
        if seals < 0 || seals & required != required {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsealed view entry",
            ));
        }
        let entry: NamespaceEntry =
            serde_json::from_reader(entry_file).map_err(io::Error::other)?;
        // The command may use descriptor 4 itself. Close it on successful exec
        // but keep it live to report an `exec`/namespace-entry failure.
        // SAFETY: F_SETFD applies to the owned setup descriptor.
        if unsafe { libc::fcntl(setup.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
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
