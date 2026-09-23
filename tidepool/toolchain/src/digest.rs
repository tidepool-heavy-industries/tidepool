//! Length-prefixed blake3 "digest of parts" — the one framing primitive
//! every content-identity computation in the workspace hashes with. Framing
//! each field by its own byte length before hashing it means distinct
//! partitions of the same total bytes can never collide (e.g. `["ab", "c"]`
//! and `["a", "bc"]` hash differently).

use blake3::Hasher;

/// Hash one length-prefixed field into `hasher`. Exposed for callers that
/// build a digest from more structure than a flat list of parts — a domain
/// tag followed by a variable-length nested manifest, say. Simple callers
/// should prefer [`of_parts`].
pub fn frame(hasher: &mut Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// Digest of a flat, ordered list of byte-string parts: each part is
/// length-prefixed before hashing, so different part boundaries over the same
/// total bytes never collide.
pub fn of_parts<'a>(parts: impl IntoIterator<Item = &'a [u8]>) -> String {
    let mut hasher = Hasher::new();
    for part in parts {
        frame(&mut hasher, part);
    }
    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_boundaries_cannot_collide() {
        let a = of_parts([b"ab".as_slice(), b"c".as_slice()]);
        let b = of_parts([b"a".as_slice(), b"bc".as_slice()]);
        assert_ne!(a, b);
    }

    #[test]
    fn same_parts_hash_the_same() {
        let a = of_parts([b"x".as_slice(), b"y".as_slice()]);
        let b = of_parts([b"x".as_slice(), b"y".as_slice()]);
        assert_eq!(a, b);
    }
}
