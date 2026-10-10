//! Process-local immutable backing for already authenticated compiler inputs.
//!
//! One generation owns one descriptor and its memfd pages. Slices share that
//! owner; dropping a producer cannot revoke a separately retained slice. The
//! endpoint is read transport, never a logical artifact identity or recovery path.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum ArenaError {
    #[error("immutable compiler input arenas require Linux memfd sealing")]
    UnsupportedPlatform,
    #[error("arena byte limit exceeds the supported file extent")]
    InvalidLimit,
    #[error("immutable input parts must contain at least one byte")]
    EmptyPart,
    #[error("immutable input range overflows")]
    RangeOverflow,
    #[error("immutable input range exceeds its arena byte bound")]
    RangeOutsideArena,
    #[error("input slice belongs to a different arena generation")]
    WrongGeneration,
    #[error("arena extent differs from its issued input ranges")]
    ExtentChanged,
    #[error("arena write did not complete; this generation cannot be sealed")]
    IncompleteWrite,
    #[error("immutable input endpoint is unavailable: {0}")]
    EndpointUnavailable(#[source] io::Error),
    #[error("immutable input arena I/O failed: {0}")]
    Io(#[from] io::Error),
}

/// A generation is mutable only while this affine builder owns it. Empty
/// generations may be sealed, but empty input parts are explicitly refused.
#[derive(Debug)]
pub(crate) struct OwnedInputArenaBuilder {
    file: File,
    generation: Arc<()>,
    max_bytes: u64,
    extent: u64,
    incomplete_write: bool,
}

/// Issued only after append has written and hashed this generation's bytes.
#[derive(Debug)]
pub(crate) struct PendingInputSlice {
    generation: Arc<()>,
    offset: u64,
    length: u64,
    sha256: [u8; 32],
}

/// The physical owner accounts for one FD and `len()` immutable backing bytes.
/// Requests and detached selections retain this same owner through their slices.
#[derive(Debug)]
pub(crate) struct OwnedInputArena {
    _file: Arc<File>,
    generation: Arc<()>,
    endpoint: PathBuf,
    extent: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct OwnedInputSlice {
    arena: Arc<OwnedInputArena>,
    offset: u64,
    length: u64,
    sha256: [u8; 32],
}

fn checked_end(offset: u64, length: u64, extent: u64) -> Result<u64, ArenaError> {
    if length == 0 {
        return Err(ArenaError::EmptyPart);
    }
    let end = offset
        .checked_add(length)
        .ok_or(ArenaError::RangeOverflow)?;
    if end > extent {
        return Err(ArenaError::RangeOutsideArena);
    }
    Ok(end)
}

fn open_endpoint(path: &Path) -> Result<File, ArenaError> {
    File::open(path).map_err(ArenaError::EndpointUnavailable)
}

impl OwnedInputArenaBuilder {
    pub(crate) fn new(max_bytes: u64) -> Result<Self, ArenaError> {
        if max_bytes > i64::MAX as u64 {
            return Err(ArenaError::InvalidLimit);
        }
        #[cfg(target_os = "linux")]
        {
            let file = File::from(
                rustix::fs::memfd_create(
                    c"tidepool-owned-compiler-inputs",
                    rustix::fs::MemfdFlags::CLOEXEC | rustix::fs::MemfdFlags::ALLOW_SEALING,
                )
                .map_err(io::Error::from)?,
            );
            Ok(Self {
                file,
                generation: Arc::new(()),
                max_bytes,
                extent: 0,
                incomplete_write: false,
            })
        }
        #[cfg(not(target_os = "linux"))]
        Err(ArenaError::UnsupportedPlatform)
    }

    pub(crate) fn append(&mut self, bytes: &[u8]) -> Result<PendingInputSlice, ArenaError> {
        if self.incomplete_write {
            return Err(ArenaError::IncompleteWrite);
        }
        let length = u64::try_from(bytes.len()).map_err(|_| ArenaError::RangeOverflow)?;
        let end = checked_end(self.extent, length, self.max_bytes)?;
        self.incomplete_write = true;
        self.file.write_all(bytes)?;
        self.incomplete_write = false;
        let pending = PendingInputSlice {
            generation: Arc::clone(&self.generation),
            offset: self.extent,
            length,
            sha256: Sha256::digest(bytes).into(),
        };
        self.extent = end;
        Ok(pending)
    }

    pub(crate) fn finish(self) -> Result<Arc<OwnedInputArena>, ArenaError> {
        if self.incomplete_write {
            return Err(ArenaError::IncompleteWrite);
        }
        if self.file.metadata()?.len() != self.extent {
            return Err(ArenaError::ExtentChanged);
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let seals = rustix::fs::SealFlags::WRITE
                | rustix::fs::SealFlags::GROW
                | rustix::fs::SealFlags::SHRINK
                | rustix::fs::SealFlags::SEAL;
            rustix::fs::fcntl_add_seals(&self.file, seals).map_err(io::Error::from)?;
            if !rustix::fs::fcntl_get_seals(&self.file)
                .map_err(io::Error::from)?
                .contains(seals)
                || self.file.metadata()?.len() != self.extent
            {
                return Err(ArenaError::ExtentChanged);
            }
            let endpoint = PathBuf::from(format!(
                "/proc/{}/fd/{}",
                std::process::id(),
                self.file.as_raw_fd()
            ));
            // Refuse an unavailable proc transport at issuance, before offering
            // a range. Other processes still authenticate their own exact reads.
            if open_endpoint(&endpoint)?.metadata()?.len() != self.extent {
                return Err(ArenaError::ExtentChanged);
            }
            Ok(Arc::new(OwnedInputArena {
                _file: Arc::new(self.file),
                generation: self.generation,
                endpoint,
                extent: self.extent,
            }))
        }
        #[cfg(not(target_os = "linux"))]
        Err(ArenaError::UnsupportedPlatform)
    }
}

impl OwnedInputArena {
    pub(crate) fn len(&self) -> u64 {
        self.extent
    }

    pub(crate) fn issue_slice(
        self: &Arc<Self>,
        pending: PendingInputSlice,
    ) -> Result<OwnedInputSlice, ArenaError> {
        if !Arc::ptr_eq(&self.generation, &pending.generation) {
            return Err(ArenaError::WrongGeneration);
        }
        checked_end(pending.offset, pending.length, self.extent)?;
        Ok(OwnedInputSlice {
            arena: Arc::clone(self),
            offset: pending.offset,
            length: pending.length,
            sha256: pending.sha256,
        })
    }
}

impl OwnedInputSlice {
    /// Keep only the sealed physical storage through the compiler transaction's
    /// confirmed close. This does not issue input selection or certificate
    /// authority, and clones the owner without duplicating its descriptor.
    pub(crate) fn compiler_file_lease(&self) -> Arc<File> {
        Arc::clone(&self.arena._file)
    }

    pub(crate) fn endpoint(&self) -> &Path {
        &self.arena.endpoint
    }

    pub(crate) fn offset(&self) -> u64 {
        self.offset
    }

    pub(crate) fn len(&self) -> u64 {
        self.length
    }

    pub(crate) fn arena_len(&self) -> u64 {
        self.arena.len()
    }

    pub(crate) fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::FileExt;

    use proptest::prelude::*;

    fn read_slice(slice: &OwnedInputSlice) -> Vec<u8> {
        let mut bytes = vec![0; usize::try_from(slice.len()).unwrap()];
        open_endpoint(slice.endpoint())
            .unwrap()
            .read_exact_at(&mut bytes, slice.offset())
            .unwrap();
        bytes
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

        #[test]
        fn append_histories_match_independent_byte_vector(
            limit in 0usize..4096,
            chunks in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..128), 1..64),
            keep in prop::collection::vec(any::<bool>(), 1..64),
        ) {
            let mut builder = OwnedInputArenaBuilder::new(limit as u64).unwrap();
            let mut model = Vec::new();
            let mut accepted = Vec::new();
            let mut empty_refusals = 0;
            let mut bound_refusals = 0;
            for bytes in chunks {
                let offset = model.len();
                match builder.append(&bytes) {
                    Ok(pending) => {
                        prop_assert!(!bytes.is_empty() && model.len() + bytes.len() <= limit);
                        model.extend_from_slice(&bytes);
                        accepted.push((pending, offset, bytes));
                    }
                    Err(ArenaError::EmptyPart) => {
                        prop_assert!(bytes.is_empty());
                        empty_refusals += 1;
                    }
                    Err(ArenaError::RangeOutsideArena) => {
                        prop_assert!(!bytes.is_empty() && model.len() + bytes.len() > limit);
                        bound_refusals += 1;
                    }
                    other => prop_assert!(false, "unexpected append outcome: {other:?}"),
                }
            }
            let arena = builder.finish().unwrap();
            prop_assert_eq!(arena.len(), model.len() as u64);
            let weak = Arc::downgrade(&arena);
            let mut slices = Vec::new();
            for (index, (pending, offset, bytes)) in accepted.into_iter().enumerate() {
                let slice = arena.issue_slice(pending).unwrap();
                prop_assert_eq!(slice.offset(), offset as u64);
                prop_assert_eq!(slice.arena_len(), model.len() as u64);
                prop_assert_eq!(read_slice(&slice), bytes.clone());
                let expected_sha: [u8; 32] = Sha256::digest(&model[offset..offset + bytes.len()]).into();
                prop_assert_eq!(slice.sha256(), &expected_sha);
                if keep[index % keep.len()] {
                    slices.push((slice.clone(), bytes));
                }
            }
            drop(arena);
            prop_assert_eq!(weak.upgrade().is_some(), !slices.is_empty());
            for (slice, bytes) in &slices {
                prop_assert_eq!(read_slice(slice), bytes.clone());
            }
            drop(slices);
            prop_assert!(weak.upgrade().is_none());
            // Rejections are part of the generated histories, rather than
            // operations silently discarded by the driver.
            prop_assert!(empty_refusals + bound_refusals <= 64);
        }

        #[test]
        fn range_arithmetic_matches_wide_integer_model(
            offset in any::<u64>(), length in any::<u64>(), bound in any::<u64>(),
        ) {
            let mathematical_end = u128::from(offset) + u128::from(length);
            match checked_end(offset, length, bound) {
                Ok(end) => {
                    prop_assert!(length > 0);
                    prop_assert_eq!(u128::from(end), mathematical_end);
                    prop_assert!(mathematical_end <= u128::from(bound));
                }
                Err(ArenaError::EmptyPart) => prop_assert_eq!(length, 0),
                Err(ArenaError::RangeOverflow) => prop_assert!(length > 0 && mathematical_end > u128::from(u64::MAX)),
                Err(ArenaError::RangeOutsideArena) => prop_assert!(length > 0 && mathematical_end <= u128::from(u64::MAX) && mathematical_end > u128::from(bound)),
                other => prop_assert!(false, "unexpected range outcome: {other:?}"),
            }
        }
    }

    #[test]
    fn seals_and_generation_refusals_preserve_bytes() {
        assert!(matches!(
            OwnedInputArenaBuilder::new(u64::MAX),
            Err(ArenaError::InvalidLimit)
        ));
        let mut empty = OwnedInputArenaBuilder::new(0).unwrap();
        assert!(matches!(empty.append(&[]), Err(ArenaError::EmptyPart)));
        assert!(matches!(
            empty.append(b"x"),
            Err(ArenaError::RangeOutsideArena)
        ));
        assert_eq!(empty.finish().unwrap().len(), 0);
        assert!(matches!(
            checked_end(u64::MAX, 1, u64::MAX),
            Err(ArenaError::RangeOverflow)
        ));

        let mut builder = OwnedInputArenaBuilder::new(3).unwrap();
        let part = builder.append(b"abc").unwrap();
        let arena = builder.finish().unwrap();
        let mut other = OwnedInputArenaBuilder::new(3).unwrap();
        let foreign = other.append(b"abc").unwrap();
        let other = other.finish().unwrap();
        assert!(matches!(
            arena.issue_slice(foreign),
            Err(ArenaError::WrongGeneration)
        ));
        drop(other);
        let slice = arena.issue_slice(part).unwrap();
        assert_eq!(
            slice.sha256(),
            &[
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ]
        );
        let seals = rustix::fs::fcntl_get_seals(&arena._file).unwrap();
        assert!(seals.contains(
            rustix::fs::SealFlags::WRITE
                | rustix::fs::SealFlags::GROW
                | rustix::fs::SealFlags::SHRINK
                | rustix::fs::SealFlags::SEAL
        ));
        let peer = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(slice.endpoint())
            .unwrap();
        assert_eq!(
            peer.write_at(b"x", 0).unwrap_err().raw_os_error(),
            Some(libc::EPERM)
        );
        assert_eq!(
            peer.set_len(2).unwrap_err().raw_os_error(),
            Some(libc::EPERM)
        );
        assert_eq!(
            peer.set_len(4).unwrap_err().raw_os_error(),
            Some(libc::EPERM)
        );
        assert_eq!(read_slice(&slice), b"abc");
        assert_eq!(arena._file.metadata().unwrap().len(), 3);
        assert!(rustix::io::fcntl_getfd(&arena._file)
            .unwrap()
            .contains(rustix::io::FdFlags::CLOEXEC));

        let damaged = OwnedInputArenaBuilder::new(10).unwrap();
        damaged.file.set_len(1).unwrap();
        assert!(matches!(damaged.finish(), Err(ArenaError::ExtentChanged)));
        let mut failed = OwnedInputArenaBuilder::new(10).unwrap();
        failed.incomplete_write = true;
        assert!(matches!(
            failed.append(b"abc"),
            Err(ArenaError::IncompleteWrite)
        ));
        assert!(matches!(failed.finish(), Err(ArenaError::IncompleteWrite)));
    }

    #[test]
    fn selected_slices_keep_backing_until_last_release() {
        let mut builder = OwnedInputArenaBuilder::new(100).unwrap();
        let origin = tempfile::tempdir().unwrap();
        let original_path = origin.path().join("acquired-original");
        std::fs::write(&original_path, b"first-original").unwrap();
        let first = builder
            .append(&std::fs::read(&original_path).unwrap())
            .unwrap();
        let second = builder.append(b"second-original").unwrap();
        let arena = builder.finish().unwrap();
        let weak = Arc::downgrade(&arena);
        let first = arena.issue_slice(first).unwrap();
        let second = arena.issue_slice(second).unwrap();
        assert_eq!(first.endpoint(), second.endpoint());
        std::fs::write(&original_path, b"changed-original").unwrap();
        assert_eq!(read_slice(&first), b"first-original");
        std::fs::remove_file(&original_path).unwrap();
        assert_eq!(read_slice(&first), b"first-original");
        let endpoint = first.endpoint().to_owned();
        let detached = second.clone();
        drop(first);
        drop(second);
        drop(arena);
        assert!(weak.upgrade().is_some());
        assert_eq!(read_slice(&detached), b"second-original");
        drop(detached);
        assert!(weak.upgrade().is_none());
        match open_endpoint(&endpoint) {
            Err(ArenaError::EndpointUnavailable(error)) => {
                assert_eq!(error.kind(), io::ErrorKind::NotFound)
            }
            outcome => panic!("released endpoint remains available: {outcome:?}"),
        }
    }

    #[test]
    fn peer_range_reads_are_offset_independent() {
        const ENDPOINT: &str = "TIDEPOOL_ARENA_PEER_ENDPOINT";
        if let Some(endpoint) = std::env::var_os(ENDPOINT) {
            let file = open_endpoint(Path::new(&endpoint)).unwrap();
            let mut bytes = [0; 6];
            file.read_exact_at(&mut bytes, 7).unwrap();
            assert_eq!(&bytes, b"middle");
            let mut prefix = [0; 6];
            file.read_exact_at(&mut prefix, 0).unwrap();
            assert_eq!(&prefix, b"prefix");
            assert_eq!(file.metadata().unwrap().len(), 20);
            return;
        }
        let mut builder = OwnedInputArenaBuilder::new(20).unwrap();
        let pending = builder.append(b"prefix-middle-suffix").unwrap();
        let arena = builder.finish().unwrap();
        let slice = arena.issue_slice(pending).unwrap();
        let owner_fd = arena._file.as_raw_fd();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "owned_input_arena::tests::peer_range_reads_are_offset_independent",
                "--nocapture",
            ])
            .env(ENDPOINT, slice.endpoint())
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(arena._file.as_raw_fd(), owner_fd);
        use std::io::Seek;
        assert_eq!(arena._file.as_ref().stream_position().unwrap(), arena.len());
        assert_eq!(read_slice(&slice), b"prefix-middle-suffix");
    }

    #[test]
    fn compiler_file_lease_retains_storage_without_arena_metadata() {
        let mut builder = OwnedInputArenaBuilder::new(100).unwrap();
        let pending = builder.append(b"transaction-owned-original").unwrap();
        let arena = builder.finish().unwrap();
        let weak_arena = Arc::downgrade(&arena);
        let slice = arena.issue_slice(pending).unwrap();
        let endpoint = slice.endpoint().to_owned();
        let lease = slice.compiler_file_lease();
        let other_lease = slice.compiler_file_lease();
        let weak_file = Arc::downgrade(&lease);
        assert!(Arc::ptr_eq(&lease, &other_lease));
        assert_eq!(lease.as_raw_fd(), arena._file.as_raw_fd());
        drop(slice);
        drop(arena);
        assert!(weak_arena.upgrade().is_none());
        assert!(weak_file.upgrade().is_some());
        assert_eq!(
            lease.set_len(0).unwrap_err().raw_os_error(),
            Some(libc::EPERM)
        );
        let read_original = || {
            let mut bytes = [0; 26];
            open_endpoint(&endpoint)
                .unwrap()
                .read_exact_at(&mut bytes, 0)
                .unwrap();
            assert_eq!(&bytes, b"transaction-owned-original");
        };
        read_original();
        drop(lease);
        read_original();
        drop(other_lease);
        assert!(weak_file.upgrade().is_none());
        assert!(matches!(open_endpoint(&endpoint),
            Err(ArenaError::EndpointUnavailable(error)) if error.kind() == io::ErrorKind::NotFound));
    }
}
