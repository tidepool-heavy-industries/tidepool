//! Rust-side mint of `Tidepool.Identity.stableVarId`.
//!
//! A host carrier needs a session value binder's `var_id` for a `name` that
//! was never compiled through GHC (its module is a hand-written source stub,
//! not an extract-minted iface). This reproduces the extract's stable-id
//! formula exactly, using the same MD5 kernel the fingerprint primops use,
//! so the two mints agree bit-for-bit. See
//! `bridge/haskell/src/Tidepool/Identity.hs::stableVarId` and
//! `GHC.Utils.Fingerprint.fingerprintString`.

use super::md5_kernel::Context;

/// GHC's external-name tag byte (`Translate.stableVarId`'s `0xFE`).
const EXTERNAL_TAG: u64 = 0xFE;

/// The stable `VarId` GHC's `Tidepool.Identity.stableVarId` mints for the
/// session value binder `<module>.<occurrence>` — the exact id a compiled
/// `Val.G<gen>` binder or a hand-written stub module's reference turn
/// carries in its Core `NVar`.
///
/// Reproduces, bit for bit:
/// - `GHC.Utils.Fingerprint.fingerprintString (module ++ ":" ++ occurrence)`,
///   which MD5s each `Char` as its code point in 4 big-endian bytes (UTF-32BE,
///   NOT UTF-8 -- every `Char`, including ASCII, is 4 bytes here);
/// - `Fingerprint high _ = ...`, where `high` is the digest's first 8 bytes
///   read big-endian;
/// - `stableVarId = 0xFE << 56 | (high & 0x00FF_FFFF_FFFF_FFFF)`.
///
/// Session value binders never carry `Tidepool.Identity.fieldParentDisamb`
/// text (that disambiguator is for record fields only), so this mints
/// exactly `stableVarId` applied to a session binder's `Name` -- no
/// additional suffix.
#[must_use]
pub fn session_var_id(module: &str, occurrence: &str) -> u64 {
    let mut bytes = Vec::with_capacity((module.len() + 1 + occurrence.len()) * 4);
    push_fingerprint_chars(&mut bytes, module);
    push_fingerprint_chars(&mut bytes, ":");
    push_fingerprint_chars(&mut bytes, occurrence);
    let (digest, _) = Context::initialized().update(&bytes).finalize();
    let mut high_bytes = [0u8; 8];
    high_bytes.copy_from_slice(&digest[..8]);
    let high = u64::from_be_bytes(high_bytes);
    (EXTERNAL_TAG << 56) | (high & 0x00FF_FFFF_FFFF_FFFF)
}

/// `fingerprintString`'s per-`Char` encoding: 4 big-endian bytes of the
/// Unicode code point, for every `char` in `text` (ASCII included).
fn push_fingerprint_chars(bytes: &mut Vec<u8>, text: &str) {
    for ch in text.chars() {
        bytes.extend_from_slice(&(ch as u32).to_be_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_byte_is_always_external() {
        let id = session_var_id("Tidepool.Session.Val.G1", "x");
        assert_eq!(id >> 56, EXTERNAL_TAG);
    }

    #[test]
    fn distinct_occurrences_mint_distinct_ids() {
        let a = session_var_id("Tidepool.Session.Val.G1", "x");
        let b = session_var_id("Tidepool.Session.Val.G1", "y");
        assert_ne!(a, b);
    }

    #[test]
    fn distinct_modules_mint_distinct_ids_for_the_same_name() {
        let a = session_var_id("Tidepool.Session.Val.G1", "x");
        let b = session_var_id("Tidepool.Session.Val.G2", "x");
        assert_ne!(a, b);
    }

    #[test]
    fn is_deterministic() {
        let a = session_var_id("Tidepool.Session.Val.G7", "job");
        let b = session_var_id("Tidepool.Session.Val.G7", "job");
        assert_eq!(a, b);
    }
}
