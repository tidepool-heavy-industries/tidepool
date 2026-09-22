use std::ffi::CStr;
use std::os::fd::BorrowedFd;

use rustix::fs::*;
use rustix::io::{Errno, Result};
use rustix::process::{Gid, Uid};

/// Scratch storage allocated before fork for copying visible root metadata.
pub(super) struct RootMetadata {
    source_names: Vec<u8>,
    destination_names: Vec<u8>,
    value: Vec<u8>,
}

impl RootMetadata {
    pub(super) fn new() -> Self {
        // Linux limits both an xattr value and the returned name list to 64 KiB.
        Self {
            source_names: vec![0; 65_536],
            destination_names: vec![0; 65_536],
            value: vec![0; 65_536],
        }
    }

    // This method runs after fork and only borrows its preallocated storage.
    pub(super) fn copy(
        &mut self,
        source: BorrowedFd<'_>,
        destination: BorrowedFd<'_>,
    ) -> Result<()> {
        let stat = fstat(source)?;
        let destination_stat = fstat(destination)?;
        if (stat.st_uid, stat.st_gid) != (destination_stat.st_uid, destination_stat.st_gid) {
            fchown(
                destination,
                Some(Uid::from_raw(stat.st_uid)),
                Some(Gid::from_raw(stat.st_gid)),
            )?;
        }
        fchmod(destination, Mode::from_raw_mode(stat.st_mode))?;
        let source_count = flistxattr(source, &mut self.source_names[..])?;
        let destination_count = flistxattr(destination, &mut self.destination_names[..])?;
        let source_names = &self.source_names[..source_count];
        for bytes in self.destination_names[..destination_count].split_inclusive(|byte| *byte == 0)
        {
            if !source_names
                .split_inclusive(|byte| *byte == 0)
                .any(|source| source == bytes)
            {
                let name = CStr::from_bytes_with_nul(bytes).map_err(|_| Errno::INVAL)?;
                fremovexattr(destination, name)?;
            }
        }
        for bytes in source_names.split_inclusive(|byte| *byte == 0) {
            let name = CStr::from_bytes_with_nul(bytes).map_err(|_| Errno::INVAL)?;
            let count = fgetxattr(source, name, &mut self.value[..])?;
            fsetxattr(destination, name, &self.value[..count], XattrFlags::empty())?;
        }
        futimens(
            destination,
            &Timestamps {
                last_access: Timespec {
                    tv_sec: stat.st_atime,
                    tv_nsec: stat.st_atime_nsec as _,
                },
                last_modification: Timespec {
                    tv_sec: stat.st_mtime,
                    tv_nsec: stat.st_mtime_nsec as _,
                },
            },
        )
    }
}
