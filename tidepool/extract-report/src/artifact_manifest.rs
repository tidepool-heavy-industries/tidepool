//! Integrity manifest for one exact set of extractor artifacts.
//!
//! The proc macro and the toolchain cache store extractor outputs in different
//! layouts, but a hit means the same thing in both places: the expected names
//! are present and every present artifact still has the digest recorded when
//! the set was published. This module owns that shared wire contract without
//! owning either caller's cache or filesystem policy.

const MANIFEST_TAG: &[u8] = b"artifact-set-v1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactManifest {
    entries: Vec<ArtifactEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactEntry {
    name: String,
    digest: Option<[u8; 32]>,
}

impl ArtifactManifest {
    pub fn from_artifacts<'a>(
        artifacts: impl IntoIterator<Item = (&'a str, Option<&'a [u8]>)>,
    ) -> Self {
        Self {
            entries: artifacts
                .into_iter()
                .map(|(name, bytes)| ArtifactEntry {
                    name: name.to_string(),
                    digest: bytes.map(|bytes| *blake3::hash(bytes).as_bytes()),
                })
                .collect(),
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ManifestDecodeError> {
        let mut decoder = Decoder::new(bytes);
        if decoder.frame()? != MANIFEST_TAG {
            return Err(ManifestDecodeError::InvalidTag);
        }
        let count = u64::from_le_bytes(
            decoder
                .frame()?
                .try_into()
                .map_err(|_| ManifestDecodeError::InvalidCount)?,
        );
        let count = usize::try_from(count).map_err(|_| ManifestDecodeError::InvalidCount)?;
        let mut entries = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            let name = String::from_utf8(decoder.frame()?.to_vec())
                .map_err(|_| ManifestDecodeError::InvalidName)?;
            let digest = match decoder.byte()? {
                0 => None,
                1 => Some(
                    decoder
                        .frame()?
                        .try_into()
                        .map_err(|_| ManifestDecodeError::InvalidDigest)?,
                ),
                tag => return Err(ManifestDecodeError::InvalidPresence(tag)),
            };
            entries.push(ArtifactEntry { name, digest });
        }
        decoder.finish()?;
        Ok(Self { entries })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        frame(&mut out, MANIFEST_TAG);
        frame(&mut out, &(self.entries.len() as u64).to_le_bytes());
        for entry in &self.entries {
            frame(&mut out, entry.name.as_bytes());
            match entry.digest {
                Some(digest) => {
                    out.push(1);
                    frame(&mut out, &digest);
                }
                None => out.push(0),
            }
        }
        out
    }

    pub fn entries(&self) -> &[ArtifactEntry] {
        &self.entries
    }
}

impl ArtifactEntry {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn digest(&self) -> Option<&[u8; 32]> {
        self.digest.as_ref()
    }

    pub fn matches(&self, bytes: &[u8]) -> bool {
        self.digest
            .is_some_and(|digest| blake3::hash(bytes).as_bytes() == &digest)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ManifestDecodeError {
    #[error("invalid artifact manifest tag")]
    InvalidTag,
    #[error("truncated artifact manifest")]
    Truncated,
    #[error("invalid artifact count")]
    InvalidCount,
    #[error("artifact name is not UTF-8")]
    InvalidName,
    #[error("invalid artifact presence tag {0}")]
    InvalidPresence(u8),
    #[error("invalid artifact digest")]
    InvalidDigest,
    #[error("trailing artifact manifest bytes")]
    TrailingBytes,
}

fn frame(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}

struct Decoder<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> Decoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], ManifestDecodeError> {
        let end = self
            .cursor
            .checked_add(count)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(ManifestDecodeError::Truncated)?;
        let value = &self.bytes[self.cursor..end];
        self.cursor = end;
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, ManifestDecodeError> {
        Ok(self.take(1)?[0])
    }

    fn frame(&mut self) -> Result<&'a [u8], ManifestDecodeError> {
        let len = u64::from_le_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| ManifestDecodeError::Truncated)?,
        );
        let len = usize::try_from(len).map_err(|_| ManifestDecodeError::Truncated)?;
        self.take(len)
    }

    fn finish(self) -> Result<(), ManifestDecodeError> {
        if self.cursor == self.bytes.len() {
            Ok(())
        } else {
            Err(ManifestDecodeError::TrailingBytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_preserves_names_presence_and_digests() {
        let manifest = ArtifactManifest::from_artifacts([
            ("meta.cbor", Some(b"meta".as_slice())),
            ("asks.json", None),
        ]);
        assert_eq!(ArtifactManifest::decode(&manifest.encode()), Ok(manifest));
    }

    #[test]
    fn malformed_or_trailing_data_is_rejected() {
        assert_eq!(
            ArtifactManifest::decode(b"not a manifest"),
            Err(ManifestDecodeError::Truncated)
        );
        let mut encoded = ArtifactManifest::from_artifacts([("x", Some(b"x".as_slice()))]).encode();
        encoded.push(0);
        assert_eq!(
            ArtifactManifest::decode(&encoded),
            Err(ManifestDecodeError::TrailingBytes)
        );
    }
}
