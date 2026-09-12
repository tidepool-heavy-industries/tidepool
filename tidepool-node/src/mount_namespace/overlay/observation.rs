//! Kernel mount identity and OverlayFS recipe observation in the owned namespace.

use std::ffi::CString;
use std::io;
use std::io::Read;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use rustix::fs::{AtFlags, Mode, OFlags, StatxFlags};
use rustix::io::Errno;

use super::{MountNamespace, OverlayRotation};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
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
        self.require_live_owner()?;
        let expected_target = target.clone();
        let path = Path::new(std::ffi::OsStr::from_bytes(target.as_bytes()));
        let mounted = rustix::fs::openat2(
            &self.descriptors.root,
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            rustix::fs::ResolveFlags::IN_ROOT | rustix::fs::ResolveFlags::NO_MAGICLINKS,
        )?;
        let stat = rustix::fs::statx(&mounted, c"", AtFlags::EMPTY_PATH, StatxFlags::MNT_ID)?;
        if stat.stx_mask & StatxFlags::MNT_ID.bits() == 0 {
            return Err(Errno::OPNOTSUPP.into());
        }
        let id = stat.stx_mnt_id;
        let readonly = rustix::fs::fstatvfs(mounted.as_fd())?
            .f_flag
            .contains(rustix::fs::StatVfsMountFlags::RDONLY);
        let mountinfo = rustix::fs::openat(
            &self.descriptors.proc_dir,
            c"mountinfo",
            OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let mut bytes = Vec::new();
        std::fs::File::from(mountinfo)
            .take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(invalid("mountinfo exceeds inspection limit"));
        }
        let record = bytes
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
