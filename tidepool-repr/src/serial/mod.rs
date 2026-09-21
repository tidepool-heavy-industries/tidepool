//! CBOR serialization and deserialization for constructor metadata.

pub mod read;
pub mod write;

pub use read::{read_metadata, MetaWarnings};
pub use write::write_metadata;

/// Errors that can occur during CBOR deserialization of Tidepool IR.
///
/// Wraps underlying `ciborium` errors and adds structural context.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// An error occurred in the underlying CBOR parser.
    #[error("CBOR decode error: {0}")]
    Cbor(#[from] ciborium::de::Error<std::io::Error>),
    /// The structural layout of the CBOR data does not match Tidepool IR.
    #[error("Invalid structure: {0}")]
    InvalidStructure(String),
    /// Input without the mandatory `TPLR` header — a stale or foreign payload.
    #[error(
        "Missing TPLR header: not a current-format Tidepool CBOR payload \
         (stale fixtures/caches must be regenerated, not tolerated)"
    )]
    MissingHeader,
    /// Truncated or incomplete Tidepool CBOR header.
    #[error("Truncated or incomplete Tidepool CBOR header")]
    TruncatedHeader,
    /// Unsupported CBOR version.
    #[error("Unsupported CBOR version {0}.{1}")]
    UnsupportedVersion(u16, u16),
    /// Two distinct constructors in the metadata hash to the same DataConId
    /// (a `stableVarId` collision) — loud instead of a silent table overwrite.
    #[error(transparent)]
    DataConCollision(#[from] crate::datacon_table::DataConCollision),
    /// A metadata field carries a CBOR value of the wrong shape, or a value
    /// outside the range its Rust type can represent. Names the offending
    /// field and what was expected.
    #[error("malformed metadata field `{field}`: {detail}")]
    MalformedMetadataField { field: &'static str, detail: String },
    /// The same key appears more than once in the metadata warnings map.
    #[error("duplicate metadata key: {0}")]
    DuplicateMetadataKey(String),
    /// A key in the metadata warnings map is not one this reader recognizes.
    /// Every key a conforming writer at or below this build's `VERSION_MINOR`
    /// can emit is already handled below; the version gate in `strip_header`
    /// rejects any payload with a newer minor before this code ever sees it,
    /// so an unrecognized key here cannot be a legitimate forward-compat
    /// addition — it names a foreign or corrupt payload.
    #[error("unknown metadata key: {0}")]
    UnknownMetadataKey(String),
}

/// 4-byte magic: ASCII 'TPLR'
pub const HEADER_MAGIC: [u8; 4] = [0x54, 0x50, 0x4C, 0x52];
/// Wire format major version. A payload whose major version differs from
/// this build's is rejected (`ReadError::UnsupportedVersion`) — bump this
/// only on a breaking shape change, in the same commit as the Haskell
/// serializer and the regenerated fixture corpora.
///
/// `3.0` changed every metadata entry from 8 to 9 REQUIRED elements (added
/// rendered field types, in field order),
/// so a `2.x` payload is a hard `UnsupportedVersion` reject, not a tolerated
/// short form; committed fixture corpora were regenerated in the same commit.
pub const VERSION_MAJOR: u16 = 3;
/// Wire format minor version. An older minor within the same major is
/// accepted (forward-compatible read); a newer minor than this build
/// supports is rejected.
pub const VERSION_MINOR: u16 = 0;
/// Total header length in bytes.
pub const HEADER_LEN: usize = 8;

/// Errors that can occur during CBOR serialization of Tidepool IR.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// An error occurred in the underlying CBOR serializer.
    #[error("CBOR encode error: {0}")]
    Cbor(#[from] ciborium::ser::Error<std::io::Error>),
}
