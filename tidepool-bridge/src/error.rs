use thiserror::Error;
use tidepool_repr::DataConId;

/// Errors that can occur when bridging between Rust types and Core Values.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum BridgeError {
    /// The outer constructor is unknown or does not belong to the decoded type.
    #[error("Unknown DataConId: {0:?}")]
    UnknownDataCon(DataConId),
    /// The `DataConId` was found, but it has an unexpected name.
    #[error("Unknown DataCon name: {0}")]
    UnknownDataConName(String),
    /// Lookup by (name, arity) failed — no constructor with this name has the
    /// expected representation arity. Emitted by derived `FromCore`/`ToCore`
    /// impls to disambiguate constructors sharing an unqualified name.
    #[error("Unknown DataCon name: {name} (arity {arity})")]
    UnknownDataConNameArity {
        /// The unqualified constructor name.
        name: String,
        /// The expected representation arity.
        arity: usize,
    },
    /// Lookup by (name, arity) found MORE THAN ONE distinct constructor —
    /// insertion order would otherwise silently decide which one is used
    /// (the class of bug that let a wrong-type `Value::Con` reach the
    /// runtime with metadata/field arity disagreeing). Emitted by derived
    /// `FromCore`/`ToCore` impls instead of picking a candidate arbitrarily;
    /// disambiguate with a `#[core(module = "...")]` attribute.
    #[error(
        "ambiguous DataCon name+arity: {name} (arity {arity}) matches {candidates:?} — \
         use a module-qualified #[core(module = \"...\")] attribute or \
         get_by_qualified_name to disambiguate"
    )]
    AmbiguousDataConNameArity {
        /// The unqualified constructor name.
        name: String,
        /// The expected representation arity.
        arity: usize,
        /// Module-qualified identity (falling back to unqualified name) of
        /// every constructor that matched both the name and the arity.
        candidates: Vec<String>,
    },
    /// Lookup by module-qualified name failed. Emitted by derived
    /// `FromCore`/`ToCore` impls when a variant carries a
    /// `#[core(module = "...", name = "...")]` attribute and the computed
    /// `<module>.<name>` is absent from the `DataConTable`. Used to
    /// disambiguate constructors that share both unqualified name and arity
    /// across source modules (e.g. `Pattern.Memory.Read` vs
    /// `Pattern.File.Read`).
    #[error("Unknown DataCon qualified name: {qualified_name}")]
    UnknownDataConQualified {
        /// The fully-qualified constructor name (`Module.Constructor`).
        qualified_name: String,
    },
    /// The number of fields in a constructor does not match the expected arity.
    #[error("Arity mismatch for DataCon {con:?}: expected {expected}, got {got}")]
    ArityMismatch {
        /// The constructor identifier.
        con: DataConId,
        /// The expected number of fields.
        expected: usize,
        /// The actual number of fields received.
        got: usize,
    },
    /// A constructor matched, but one of its fields could not be decoded.
    /// Keeping the outer match distinct prevents composed decoders from
    /// mistaking a nested unknown constructor for an unrelated outer variant.
    #[error("could not decode field {field} of {constructor} (observed {observed}): {source}")]
    FieldDecode {
        /// Module-qualified constructor identity when the derive declared it.
        constructor: String,
        /// One-based source field position.
        field: usize,
        /// Constructor identity or value shape actually present in the field.
        observed: String,
        /// The nested bridge failure.
        #[source]
        source: Box<BridgeError>,
    },
    /// The value has an unexpected type (e.g., expected a Literal, got a Con).
    #[error("Type mismatch: expected {expected}, got {got}")]
    TypeMismatch {
        /// A description of the expected type.
        expected: String,
        /// A description of the actual type received.
        got: String,
    },
    /// The type is not supported by the bridge.
    #[error("Unsupported type: {0}")]
    UnsupportedType(String),
    /// Internal invariant violation (should never happen).
    #[error("Internal error: {0}")]
    InternalError(String),
}
