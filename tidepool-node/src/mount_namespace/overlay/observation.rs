//! Kernel mount identity and OverlayFS recipe observation in the owned namespace.

use std::ffi::CString;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;

use rustix::fs::{AtFlags, StatxFlags};
use rustix::io::Errno;

use super::{MountNamespace, OverlayRotation};

#[derive(Debug)]
pub(super) struct Observation {
    pub(super) id: u64,
    pub(super) readonly: bool,
    options: Vec<(Vec<u8>, Vec<u8>)>,
}

impl Observation {
    pub(super) fn matches(&self, rotation: &OverlayRotation) -> bool {
        let values = |key: &[u8]| {
            self.options
                .iter()
                .filter(move |(name, _)| name == key)
                .map(|(_, value)| value.as_slice())
                .collect::<Vec<_>>()
        };
        values(b"upperdir") == [rotation.upper_option.as_bytes()]
            && values(b"workdir") == [rotation.work_option.as_bytes()]
            && values(b"lowerdir+")
                == rotation
                    .lower
                    .iter()
                    .map(|path| path.as_bytes())
                    .collect::<Vec<_>>()
            && values(b"lowerdir").is_empty()
            && values(b"datadir+").is_empty()
    }
}

pub(super) fn mount_id(path: &std::ffi::CStr) -> rustix::io::Result<u64> {
    let stat = rustix::fs::statx(rustix::fs::CWD, path, AtFlags::empty(), StatxFlags::MNT_ID)?;
    if stat.stx_mask & StatxFlags::MNT_ID.bits() == 0 {
        return Err(Errno::OPNOTSUPP);
    }
    Ok(stat.stx_mnt_id)
}

impl MountNamespace {
    pub(super) fn observe_overlay(&self, target: &CString) -> io::Result<Observation> {
        let target = target.clone();
        let expected_target = target.clone();
        let mut command = self.host_command(Path::new("/"), "cat".as_ref())?;
        // SAFETY: runs after namespace entry with preallocated arguments and
        // syscalls only. The nine-byte header cannot fill the stdout pipe while
        // Command waits for exec; cat subsequently appends its own mountinfo.
        unsafe {
            command.pre_exec(move || {
                let id = mount_id(target.as_c_str())?;
                let flags = rustix::fs::statvfs(target.as_c_str())?.f_flag;
                let mut header = [0u8; 9];
                header[..8].copy_from_slice(&id.to_le_bytes());
                header[8] = u8::from(flags.contains(rustix::fs::StatVfsMountFlags::RDONLY));
                loop {
                    match rustix::io::write(rustix::stdio::stdout(), &header) {
                        Ok(9) => return Ok(()),
                        Err(Errno::INTR) => continue,
                        Err(error) => return Err(error.into()),
                        Ok(_) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
                    }
                }
            });
        }
        let output = command.arg("/proc/self/mountinfo").output()?;
        if !output.status.success() || output.stdout.len() < 9 {
            return Err(invalid("mount inspection did not complete"));
        }
        let id = u64::from_le_bytes(
            output.stdout[..8]
                .try_into()
                .map_err(|_| invalid("mount identity"))?,
        );
        let readonly = match output.stdout[8] {
            0 => false,
            1 => true,
            _ => return Err(invalid("mount flags")),
        };
        let record = output.stdout[9..]
            .split(|byte| *byte == b'\n')
            .find(|line| {
                line.split(|byte| *byte == b' ')
                    .next()
                    .and_then(|word| std::str::from_utf8(word).ok())
                    .and_then(|word| word.parse::<u64>().ok())
                    == Some(id)
            })
            .ok_or_else(|| invalid("visible mount identity absent from mountinfo"))?;
        let fields = record.split(|byte| *byte == b' ').collect::<Vec<_>>();
        let separator = fields
            .iter()
            .position(|field| *field == b"-")
            .ok_or_else(|| invalid("mountinfo separator"))?;
        if separator < 6
            || fields.len() != separator + 4
            || fields[separator + 1] != b"overlay"
            || unescape(fields[3])? != b"/"
            || unescape(fields[4])? != expected_target.as_bytes()
        {
            return Err(invalid("target is not an OverlayFS mount root"));
        }
        let options = fields[separator + 3]
            .split(|byte| *byte == b',')
            .map(|option| {
                let mut parts = option.splitn(2, |byte| *byte == b'=');
                let name = parts.next().ok_or_else(|| invalid("mount option name"))?;
                Ok((unescape(name)?, unescape(parts.next().unwrap_or_default())?))
            })
            .collect::<io::Result<_>>()?;
        Ok(Observation {
            id,
            readonly,
            options,
        })
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

// proc mountinfo and seq_show_option encode delimiters as three octal digits.
// Decode after splitting fields/options so escaped commas remain path bytes.
fn unescape(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let mut result = Vec::with_capacity(bytes.len());
    let mut remaining = bytes;
    while let Some((&byte, tail)) = remaining.split_first() {
        if byte != b'\\' {
            result.push(byte);
            remaining = tail;
            continue;
        }
        let digits = tail
            .get(..3)
            .ok_or_else(|| invalid("short mountinfo escape"))?;
        if !digits.iter().all(|byte| (b'0'..=b'7').contains(byte)) || digits[0] > b'3' {
            return Err(invalid("invalid mountinfo escape"));
        }
        result.push((digits[0] - b'0') * 64 + (digits[1] - b'0') * 8 + digits[2] - b'0');
        remaining = &tail[3..];
    }
    Ok(result)
}
