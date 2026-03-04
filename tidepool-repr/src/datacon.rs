use crate::types::DataConId;

/// Strictness annotation for a data constructor field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SrcBang {
    /// No annotation (lazy by default in Haskell)
    NoSrcBang,
    /// Strict annotation (!)
    SrcBang,
    /// Unpack annotation ({-# UNPACK #-})
    SrcUnpack,
}

/// Metadata for a single data constructor.
/// Extracted from GHC's DataCon during serialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataCon {
    /// Unique identifier for this constructor
    pub id: DataConId,
    /// Human-readable name (e.g., "Just", "Nothing", ":", "[]")
    pub name: String,
    /// 1-based constructor tag (from `dataConTag`). First constructor of a type is 1.
    pub tag: u32,
    /// Representation arity — number of fields after worker/wrapper transformation.
    /// This is `length (dataConRepArgTys dc)`, NOT source arity.
    pub rep_arity: u32,
    /// Strictness per field (from `dataConSrcBangs`). For debugging/pretty-printing only.
    pub field_bangs: Vec<SrcBang>,
    /// Module-qualified name (e.g., "Data.Map.Bin"). None for legacy CBOR without this field.
    pub qualified_name: Option<String>,
}
