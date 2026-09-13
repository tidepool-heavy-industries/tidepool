//! Safe, copy-based access to GHC's pinned MD5 context ABI.
//!
//! This module owns no VM pointers or external-storage admission. Callers must
//! copy authenticated bytes into slices before entering this kernel.

use std::ffi::c_int;

pub(crate) const CONTEXT_BYTES: usize = 88;

/// The native C layout from GHC 9.12.2's `md5.h`.
///
/// Context byte conversion copies each native-endian word. It never casts a
/// possibly unaligned borrowed byte buffer to this type.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Context {
    state: [u32; 4],
    byte_count: [u32; 2],
    pending: [u32; 16],
}

const _: () = assert!(size_of::<Context>() == CONTEXT_BYTES);

unsafe extern "C" {
    fn __hsbase_MD5Init(context: *mut Context);
    fn __hsbase_MD5Update(context: *mut Context, bytes: *const u8, len: c_int);
    fn __hsbase_MD5Final(digest: *mut u8, context: *mut Context);
}

impl Context {
    pub(crate) fn initialized() -> Self {
        let mut context = Self::zeroed();
        // SAFETY: `context` has the exact aligned C layout and is exclusively
        // borrowed for the duration of initialization.
        unsafe { __hsbase_MD5Init(&mut context) };
        context
    }

    pub(crate) fn from_bytes(bytes: [u8; CONTEXT_BYTES]) -> Self {
        let word = |index: usize| {
            let offset = index * size_of::<u32>();
            u32::from_ne_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ])
        };
        Self {
            state: std::array::from_fn(word),
            byte_count: std::array::from_fn(|index| word(index + 4)),
            pending: std::array::from_fn(|index| word(index + 6)),
        }
    }

    pub(crate) fn to_bytes(self) -> [u8; CONTEXT_BYTES] {
        let mut bytes = [0; CONTEXT_BYTES];
        for (destination, word) in bytes.chunks_exact_mut(size_of::<u32>()).zip(
            self.state
                .into_iter()
                .chain(self.byte_count)
                .chain(self.pending),
        ) {
            destination.copy_from_slice(&word.to_ne_bytes());
        }
        bytes
    }

    pub(crate) fn update(mut self, bytes: &[u8]) -> Self {
        for chunk in bytes.chunks(c_int::MAX as usize) {
            // SAFETY: the context is aligned and exclusively borrowed. Each
            // chunk is a valid readable slice whose length fits the C `int`.
            unsafe {
                __hsbase_MD5Update(&mut self, chunk.as_ptr(), chunk.len() as c_int);
            }
        }
        self
    }

    /// Return the digest and the context after GHC's finalizer zeroizes it.
    pub(crate) fn finalize(mut self) -> ([u8; 16], Self) {
        let mut digest = [0; 16];
        // SAFETY: both outputs are aligned, initialized, exclusively borrowed,
        // and have the exact extents required by the C function.
        unsafe { __hsbase_MD5Final(digest.as_mut_ptr(), &mut self) };
        (digest, self)
    }

    const fn zeroed() -> Self {
        Self {
            state: [0; 4],
            byte_count: [0; 2],
            pending: [0; 16],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(bytes: &[u8]) -> [u8; 16] {
        Context::initialized().update(bytes).finalize().0
    }

    fn hex(bytes: [u8; 16]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn known_vectors_cover_empty_short_and_multiple_blocks() {
        assert_eq!(hex(digest(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(digest(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hex(digest(
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
            )),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
    }

    #[test]
    fn incremental_updates_match_one_shot() {
        let incremental = Context::initialized()
            .update(b"The quick brown ")
            .update(b"fox jumps over ")
            .update(b"the lazy dog")
            .finalize()
            .0;
        assert_eq!(
            incremental,
            digest(b"The quick brown fox jumps over the lazy dog")
        );
    }

    #[test]
    fn context_bytes_round_trip_and_finalization_zeroizes() {
        let context = Context::initialized().update(b"prefix-");
        let restored = Context::from_bytes(context.to_bytes());
        let (actual, cleared) = restored.update(b"suffix").finalize();
        assert_eq!(actual, digest(b"prefix-suffix"));
        assert_eq!(cleared.to_bytes(), [0; CONTEXT_BYTES]);
    }
}
