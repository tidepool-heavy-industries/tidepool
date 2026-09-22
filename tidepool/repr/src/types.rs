//! Shared identifiers and literals used by prepared-STG programs.

/// High-byte tag marking an external (Option-C session/library) binder id.
/// A real external under Option C: `stableVarId = 0xFE<<56 | fingerprint`.
pub const EXTERNAL_TAG: u8 = 0xFE;

/// Variable identifier. Wraps a numeric ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VarId(pub u64);

/// Decoded high-byte tag of a [`VarId`]. Replaces bare byte
/// comparisons (`v >> 56 == 0x..`) at resolution sites with an exhaustive match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarKind {
    /// `0xFE` — a real external (library unfolding OR a session binder).
    External,
    /// Any other high byte — an ordinary local binder.
    Local,
}

impl VarId {
    /// The high byte of the id (`self.0 >> 56`).
    #[must_use]
    pub fn tag(self) -> u8 {
        (self.0 >> 56) as u8
    }

    /// Decode the high-byte tag into a [`VarKind`].
    #[must_use]
    pub fn kind(self) -> VarKind {
        match self.tag() {
            EXTERNAL_TAG => VarKind::External,
            _ => VarKind::Local,
        }
    }
}

/// Data constructor identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DataConId(pub u64);

/// Literal values. Matches GHC's post-O2 literal types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Literal {
    /// 64-bit signed integer.
    LitInt(i64),
    /// 64-bit unsigned integer.
    LitWord(u64),
    /// Unicode character.
    LitChar(char),
    /// UTF-8 or raw byte string.
    LitString(Vec<u8>),
    /// Raw `ByteArray#` literal (e.g. a `BigNat#` payload): the bytes ARE the
    /// array contents. Distinct from `LitString` so it lowers with the
    /// ByteArray# layout (`sizeofByteArray#` reads the length prefix; no
    /// unpackCString# `+8` adjustment) instead of the string layout.
    LitByteArray(Vec<u8>),
    /// 32-bit floating point (stored as IEEE 754 bits).
    LitFloat(u64),
    /// 64-bit floating point (stored as IEEE 754 bits).
    LitDouble(u64),
}

impl std::fmt::Display for VarId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "v_{}", self.0)
    }
}

impl std::fmt::Display for DataConId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Con_{}", self.0)
    }
}

impl std::fmt::Display for Literal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Literal::LitInt(n) => write!(f, "{}#", n),
            Literal::LitWord(n) => write!(f, "{}##", n),
            Literal::LitChar(c) => write!(f, "'{}'#", c),
            Literal::LitString(bs) => match std::str::from_utf8(bs) {
                Ok(s) => write!(f, "\"{}\"#", s),
                Err(_) => write!(f, "<bytes len={}>", bs.len()),
            },
            Literal::LitByteArray(bs) => write!(f, "<bytearray len={}>", bs.len()),
            Literal::LitFloat(bits) => write!(f, "{}#", f32::from_bits(*bits as u32)),
            Literal::LitDouble(bits) => write!(f, "{}##", f64::from_bits(*bits)),
        }
    }
}

impl From<i64> for Literal {
    fn from(n: i64) -> Self {
        Literal::LitInt(n)
    }
}

impl From<u64> for Literal {
    fn from(n: u64) -> Self {
        Literal::LitWord(n)
    }
}

impl From<char> for Literal {
    fn from(c: char) -> Self {
        Literal::LitChar(c)
    }
}

impl From<f64> for Literal {
    fn from(f: f64) -> Self {
        Literal::LitDouble(f.to_bits())
    }
}

impl From<f32> for Literal {
    fn from(f: f32) -> Self {
        Literal::LitFloat(f.to_bits() as u64)
    }
}

impl From<Vec<u8>> for Literal {
    fn from(bs: Vec<u8>) -> Self {
        Literal::LitString(bs)
    }
}
