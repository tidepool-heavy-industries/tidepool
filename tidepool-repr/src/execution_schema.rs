//! Invariant-bearing prepared-STG execution schema.
//!
//! Wire decoding is deliberately kept in this module: callers can construct a
//! [`WireProgram`] for encoding and tests, but executable consumers only receive
//! a validated [`PreparedProgram`] and an atomically linked [`LinkedProgram`].

use std::collections::BTreeMap;

pub const SCHEMA_VERSION: u64 = 7;
pub const EXECUTION_ABI_VERSION: u64 = 5;

macro_rules! dense_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(pub u32);
    };
}

dense_id!(ValueId);
dense_id!(JoinId);
dense_id!(GlobalId);
dense_id!(ConstructorId);
dense_id!(OperationId);
dense_id!(SignatureId);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Architecture {
    X86_64,
    Aarch64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Endianness {
    Little,
    Big,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetDescriptor {
    pub architecture: Architecture,
    pub endianness: Endianness,
    pub pointer_width: u8,
    pub word_width: u8,
    pub abi: String,
    pub features: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProgramEnvelope {
    pub schema_version: u64,
    pub projection_profile: String,
    pub toolchain: String,
    pub execution_abi_version: u64,
    pub target: TargetDescriptor,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SymbolIdentity {
    pub unit: String,
    pub module: String,
    pub namespace: String,
    pub occurrence: String,
    /// GHC record-field parent; absent for ordinary names and internal binders.
    pub record_parent: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RuntimeRep {
    Void,
    LiftedRef,
    UnliftedRef,
    Address,
    Int(u8),
    Word(u8),
    Float(u8),
}

/// Successful result representations, or authoritative evidence that saturation
/// cannot return successfully. `Returns([])` is a successful zero-result call;
/// it is never interchangeable with `NoSuccess`. Partial application does not
/// discharge the latter contract: it still produces a lifted function value.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ResultContract {
    Returns(Vec<RuntimeRep>),
    NoSuccess,
}

impl ResultContract {
    /// Successful logical result representations, if the expression may return.
    pub fn returned_reps(&self) -> Option<&[RuntimeRep]> {
        match self {
            Self::Returns(reps) => Some(reps),
            Self::NoSuccess => None,
        }
    }

    /// A nonreturning expression satisfies any continuation demand. A demand
    /// alone is not evidence: ordinary returning expressions must match exactly.
    pub fn satisfies(&self, demanded: &Self) -> bool {
        matches!(self, Self::NoSuccess) || self == demanded
    }

    /// Meet branches at a case continuation without inventing results for a
    /// nonreturning branch. Different successful representations are incompatible.
    pub fn merge_alternative(&self, other: &Self) -> Option<Self> {
        match (self, other) {
            (Self::NoSuccess, result) | (result, Self::NoSuccess) => Some(result.clone()),
            _ if self == other => Some(self.clone()),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Signature {
    pub arguments: Vec<RuntimeRep>,
    pub results: ResultContract,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LayoutError {
    #[error("unsupported storage representation {0:?}")]
    UnsupportedRepresentation(RuntimeRep),
    #[error("invalid target pointer width {0}")]
    InvalidPointerWidth(u8),
    #[error("storage layout size overflow")]
    Overflow,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageField {
    logical_index: u32,
    rep: RuntimeRep,
    offset: u32,
    size: u32,
    alignment: u32,
}

impl StorageField {
    pub fn logical_index(&self) -> u32 {
        self.logical_index
    }

    pub fn rep(&self) -> RuntimeRep {
        self.rep
    }

    pub fn offset(&self) -> u32 {
        self.offset
    }

    pub fn size(&self) -> u32 {
        self.size
    }

    pub fn alignment(&self) -> u32 {
        self.alignment
    }
}

/// The sole semantic-representation to byte-storage calculation.
///
/// `Void` remains present in `logical_to_stored` but occupies no bytes. Raw
/// addresses have pointer-sized storage but are deliberately absent from
/// `managed_root_offsets`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageLayout {
    logical_to_stored: Vec<Option<u32>>,
    fields: Vec<StorageField>,
    payload_size: u32,
    alignment: u32,
    managed_root_offsets: Vec<u32>,
}

impl StorageLayout {
    pub fn for_reps(target: &TargetDescriptor, reps: &[RuntimeRep]) -> Result<Self, LayoutError> {
        let pointer_size = match target.pointer_width {
            width if width > 0 && width % 8 == 0 => u32::from(width / 8),
            width => return Err(LayoutError::InvalidPointerWidth(width)),
        };
        let mut logical_to_stored = Vec::with_capacity(reps.len());
        let mut fields = Vec::new();
        let mut managed_root_offsets = Vec::new();
        let mut cursor = 0u32;
        let mut max_alignment = 1u32;

        for (logical_index, rep) in reps.iter().copied().enumerate() {
            if rep == RuntimeRep::Void {
                logical_to_stored.push(None);
                continue;
            }
            let size = storage_size(rep, pointer_size)?;
            let alignment = size;
            cursor = align_up(cursor, alignment)?;
            let stored_index = u32::try_from(fields.len()).map_err(|_| LayoutError::Overflow)?;
            logical_to_stored.push(Some(stored_index));
            if matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
                managed_root_offsets.push(cursor);
            }
            fields.push(StorageField {
                logical_index: u32::try_from(logical_index).map_err(|_| LayoutError::Overflow)?,
                rep,
                offset: cursor,
                size,
                alignment,
            });
            cursor = cursor.checked_add(size).ok_or(LayoutError::Overflow)?;
            max_alignment = max_alignment.max(alignment);
        }

        Ok(Self {
            logical_to_stored,
            fields,
            payload_size: align_up(cursor, max_alignment)?,
            alignment: max_alignment,
            managed_root_offsets,
        })
    }

    pub fn logical_to_stored(&self) -> &[Option<u32>] {
        &self.logical_to_stored
    }

    pub fn fields(&self) -> &[StorageField] {
        &self.fields
    }

    pub fn payload_size(&self) -> u32 {
        self.payload_size
    }

    pub fn alignment(&self) -> u32 {
        self.alignment
    }

    pub fn managed_root_offsets(&self) -> &[u32] {
        &self.managed_root_offsets
    }
}

fn storage_size(rep: RuntimeRep, pointer_size: u32) -> Result<u32, LayoutError> {
    match rep {
        RuntimeRep::Void => Ok(0),
        RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef | RuntimeRep::Address => Ok(pointer_size),
        RuntimeRep::Int(bits) | RuntimeRep::Word(bits) if matches!(bits, 8 | 16 | 32 | 64) => {
            Ok(u32::from(bits / 8))
        }
        RuntimeRep::Float(32) => Ok(4),
        RuntimeRep::Float(64) => Ok(8),
        other => Err(LayoutError::UnsupportedRepresentation(other)),
    }
}

fn align_up(value: u32, alignment: u32) -> Result<u32, LayoutError> {
    let mask = alignment.checked_sub(1).ok_or(LayoutError::Overflow)?;
    if !alignment.is_power_of_two() {
        return Err(LayoutError::Overflow);
    }
    value
        .checked_add(mask)
        .map(|sum| sum & !mask)
        .ok_or(LayoutError::Overflow)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldLayout {
    pub rep: RuntimeRep,
    pub offset: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedLayout {
    pub fields: Vec<FieldLayout>,
    pub alignment: u32,
    pub payload_size: u32,
    pub root_mask: Vec<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConstructorDecl {
    pub identity: SymbolIdentity,
    /// Existing bridge identity minted by Tidepool.Identity.varId on GHC's
    /// constructor worker. Never substitute the family-relative constructor tag.
    pub host_id: crate::DataConId,
    pub family: SymbolIdentity,
    pub result_rep: RuntimeRep,
    pub field_reps: Vec<RuntimeRep>,
    pub strict_fields: Vec<bool>,
    pub layout: CheckedLayout,
    /// GHC's one-based tag in the complete algebraic constructor family.
    pub tag: u32,
    /// Authoritative family cardinality, not the number of declarations in this artifact.
    pub family_size: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlobalDecl {
    pub identity: SymbolIdentity,
    pub rep: RuntimeRep,
    /// Required entry evidence when GHC knows the closure's entry arity.
    /// Unknown lifted values must not acquire an entry from their full type.
    pub entry_signature: Option<SignatureId>,
    pub required_evaluated: bool,
    pub required_generation: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValueRef {
    Local(ValueId),
    Global(GlobalId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScalarLiteral {
    Int {
        bits: u8,
        bytes: Vec<u8>,
    },
    Word {
        bits: u8,
        bytes: Vec<u8>,
    },
    Float {
        bits: u8,
        bytes: Vec<u8>,
    },
    Bytes(Vec<u8>),
    /// The raw `Addr#` null value, never a managed reference.
    NullAddress,
}

impl ScalarLiteral {
    pub fn rep(&self) -> RuntimeRep {
        match self {
            Self::Int { bits, .. } => RuntimeRep::Int(*bits),
            Self::Word { bits, .. } => RuntimeRep::Word(*bits),
            Self::Float { bits, .. } => RuntimeRep::Float(*bits),
            Self::Bytes(_) | Self::NullAddress => RuntimeRep::Address,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Atom {
    Ref(ValueRef),
    Scalar(ScalarLiteral),
    Void,
    /// An absent value with GHC's post-unarisation representation. It may be
    /// transported in an unused slot, but is not an ordinary zero/null value.
    /// Managed rubbish must remain distinguishable when transported or traced:
    /// entering it produces typed integrity failure, never a memory access.
    Rubbish(RuntimeRep),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Group<T> {
    NonRecursive(T),
    Recursive(Vec<T>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Thunk entry policy only. GHC's `ReEntrant` is represented by
/// `HeapRhs::Function`, not a third thunk policy; `JumpedTo` belongs to joins.
pub enum UpdatePolicy {
    Memoize,
    SingleEntry,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeapBinding<B = usize> {
    pub id: ValueId,
    pub rhs: HeapRhs<B>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HeapRhs<B = usize> {
    /// Immutable module-owned bytes (GHC StgTopStringLit), not a thunk.
    Bytes(Vec<u8>),
    Function {
        signature: SignatureId,
        parameters: Vec<ValueId>,
        captures: Vec<ValueRef>,
        body: B,
    },
    Thunk {
        signature: SignatureId,
        update: UpdatePolicy,
        captures: Vec<ValueRef>,
        body: B,
    },
    Constructor {
        constructor: ConstructorId,
        fields: Vec<Atom>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JoinBinding<B = usize> {
    pub id: JoinId,
    pub signature: SignatureId,
    pub parameters: Vec<ValueId>,
    pub body: B,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AlternativePattern {
    Default,
    Constructor(ConstructorId),
    Literal(ScalarLiteral),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Alternative<B = usize> {
    pub pattern: AlternativePattern,
    pub binders: Vec<ValueId>,
    pub body: B,
}

/// GHC's post-unarisation alternative classification, without GHC types.
///
/// Family identity proves agreement, not exhaustiveness: declarations contain
/// only encountered constructors. If no alternative matches, execution reports
/// an integrity failure, including when an upstream refinement was violated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CaseKind {
    Algebraic(SymbolIdentity),
    Primitive(RuntimeRep),
    /// One tuple alternative binds the returned physical components directly.
    MultiValue,
    /// A single DEFAULT demands the scrutinee without inspecting its shape.
    Polymorphic,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExprFrame<A> {
    Return(Vec<Atom>),
    Enter {
        callee: Atom,
        signature: SignatureId,
    },
    Call {
        callee: Atom,
        signature: SignatureId,
        arguments: Vec<Atom>,
    },
    Operation {
        operation: OperationId,
        arguments: Vec<Atom>,
    },
    Construct {
        constructor: ConstructorId,
        fields: Vec<Atom>,
    },
    Case {
        scrutinee: A,
        binder: ValueId,
        /// Empty alternatives may demand NoSuccess when GHC cannot resolve the
        /// binder's representation. Validation must prove that demand from the
        /// scrutinee, not infer it merely from the absence of alternatives.
        scrutinee_results: ResultContract,
        kind: CaseKind,
        alternatives: Vec<Alternative<A>>,
    },
    Let {
        bindings: Group<HeapBinding<A>>,
        body: A,
    },
    LetJoins {
        bindings: Group<JoinBinding<A>>,
        body: A,
    },
    Jump {
        join: JoinId,
        arguments: Vec<Atom>,
    },
}

/// The program's flat, postorder expression arena. All syntactic descendants,
/// including local closure and join bodies, belong to this arena. Language recursion is
/// expressed through binder references, never through expression-index cycles.
pub type Expr = crate::tree::RecursiveTree<ExprFrame<usize>>;

impl recursion::MappableFrame for ExprFrame<recursion::PartiallyApplied> {
    type Frame<X> = ExprFrame<X>;

    fn map_frame<A, B>(input: ExprFrame<A>, mut f: impl FnMut(A) -> B) -> ExprFrame<B> {
        match input {
            ExprFrame::Return(atoms) => ExprFrame::Return(atoms),
            ExprFrame::Enter { callee, signature } => ExprFrame::Enter { callee, signature },
            ExprFrame::Call {
                callee,
                signature,
                arguments,
            } => ExprFrame::Call {
                callee,
                signature,
                arguments,
            },
            ExprFrame::Operation {
                operation,
                arguments,
            } => ExprFrame::Operation {
                operation,
                arguments,
            },
            ExprFrame::Construct {
                constructor,
                fields,
            } => ExprFrame::Construct {
                constructor,
                fields,
            },
            ExprFrame::Jump { join, arguments } => ExprFrame::Jump { join, arguments },
            ExprFrame::Case {
                scrutinee,
                binder,
                scrutinee_results,
                kind,
                alternatives,
            } => ExprFrame::Case {
                scrutinee: f(scrutinee),
                binder,
                scrutinee_results,
                kind,
                alternatives: alternatives
                    .into_iter()
                    .map(|alt| Alternative {
                        pattern: alt.pattern,
                        binders: alt.binders,
                        body: f(alt.body),
                    })
                    .collect(),
            },
            ExprFrame::Let { bindings, body } => ExprFrame::Let {
                bindings: bindings.map(|binding| HeapBinding {
                    id: binding.id,
                    rhs: binding.rhs.map_body(&mut f),
                }),
                body: f(body),
            },
            ExprFrame::LetJoins { bindings, body } => ExprFrame::LetJoins {
                bindings: bindings.map(|binding| JoinBinding {
                    id: binding.id,
                    signature: binding.signature,
                    parameters: binding.parameters,
                    body: f(binding.body),
                }),
                body: f(body),
            },
        }
    }
}

impl<T> Group<T> {
    pub fn map<U>(self, mut f: impl FnMut(T) -> U) -> Group<U> {
        match self {
            Self::NonRecursive(value) => Group::NonRecursive(f(value)),
            Self::Recursive(values) => Group::Recursive(values.into_iter().map(f).collect()),
        }
    }
}

impl<A> HeapRhs<A> {
    pub fn map_body<B>(self, mut f: impl FnMut(A) -> B) -> HeapRhs<B> {
        match self {
            Self::Bytes(bytes) => HeapRhs::Bytes(bytes),
            Self::Constructor {
                constructor,
                fields,
            } => HeapRhs::Constructor {
                constructor,
                fields,
            },
            Self::Function {
                signature,
                parameters,
                captures,
                body,
            } => HeapRhs::Function {
                signature,
                parameters,
                captures,
                body: f(body),
            },
            Self::Thunk {
                signature,
                update,
                captures,
                body,
            } => HeapRhs::Thunk {
                signature,
                update,
                captures,
                body: f(body),
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationDecl {
    pub identity: OperationIdentity,
    pub signature: SignatureId,
}

/// Primops and admitted foreign capabilities occupy distinct identity spaces.
/// The declaration signature completes the operation's identity; the same
/// primop may occur at more than one instantiated signature.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum OperationIdentity {
    PrimOp(String),
    Intrinsic {
        symbol: String,
        convention: ForeignConvention,
    },
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ForeignConvention {
    CCall,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TopBinding {
    pub identity: SymbolIdentity,
    pub binding: HeapBinding,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireProgram {
    pub envelope: ProgramEnvelope,
    pub signatures: Vec<Signature>,
    pub globals: Vec<GlobalDecl>,
    pub constructors: Vec<ConstructorDecl>,
    pub operations: Vec<OperationDecl>,
    pub expressions: Expr,
    pub bindings: Vec<Group<TopBinding>>,
    pub entry: ValueId,
}

/// Validated but not yet linked program. Its fields remain private so every
/// executable consumer crosses the same validation boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedProgram {
    wire: WireProgram,
}

impl PreparedProgram {
    pub fn envelope(&self) -> &ProgramEnvelope {
        &self.wire.envelope
    }
    pub fn entry(&self) -> ValueId {
        self.wire.entry
    }
    pub fn bindings(&self) -> &[Group<TopBinding>] {
        &self.wire.bindings
    }
    pub fn expressions(&self) -> &Expr {
        &self.wire.expressions
    }
    pub fn signatures(&self) -> &[Signature] {
        &self.wire.signatures
    }
    pub fn constructors(&self) -> &[ConstructorDecl] {
        &self.wire.constructors
    }
    pub fn operations(&self) -> &[OperationDecl] {
        &self.wire.operations
    }
    pub fn globals(&self) -> &[GlobalDecl] {
        &self.wire.globals
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedValue {
    pub identity: SymbolIdentity,
    pub rep: RuntimeRep,
    /// Semantic signature supplied by the binding owner. Signature IDs are
    /// module-local table indices and therefore cannot cross the link boundary.
    pub entry_signature: Option<Signature>,
    pub evaluated: bool,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkedProgram {
    prepared: PreparedProgram,
    imports: Vec<ImportedValue>,
}

impl LinkedProgram {
    pub fn prepared(&self) -> &PreparedProgram {
        &self.prepared
    }
    pub fn imports(&self) -> &[ImportedValue] {
        &self.imports
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MachineImports {
    pub values: BTreeMap<SymbolIdentity, ImportedValue>,
}

/// Producer and target facts accepted by this execution consumer.
///
/// The decoder compares the artifact envelope with this value before
/// publishing a [`PreparedProgram`]. It must not infer target facts from the
/// decoder process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProgramRequirements {
    pub schema_version: u64,
    pub projection_profile: String,
    pub toolchain: String,
    pub execution_abi_version: u64,
    pub target: TargetDescriptor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodeLimits {
    pub max_bytes: usize,
    pub max_nodes: usize,
    pub max_table_entries: usize,
    pub max_string_bytes: usize,
    pub max_work: usize,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_bytes: 16 << 20,
            max_nodes: 1 << 20,
            max_table_entries: 1 << 18,
            max_string_bytes: 1 << 20,
            max_work: 1 << 24,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ParseError {
    #[error("prepared program exceeds {limit} byte limit ({actual})")]
    ByteLimit { limit: usize, actual: usize },
    #[error("truncated prepared program")]
    Truncated,
    #[error("trailing bytes after prepared program")]
    TrailingBytes,
    #[error("invalid prepared program tag {0}")]
    InvalidTag(u64),
    #[error("unsupported prepared schema version {0}")]
    UnsupportedVersion(u64),
    #[error("unsupported execution target: {0}")]
    UnsupportedTarget(String),
    #[error("prepared program limit exceeded: {0}")]
    LimitExceeded(&'static str),
    #[error("invalid prepared program reference: {0}")]
    InvalidReference(String),
    #[error("invalid prepared program scope: {0}")]
    InvalidScope(String),
    #[error("invalid prepared program signature: {0}")]
    InvalidSignature(String),
    #[error("invalid prepared program layout: {0}")]
    InvalidLayout(String),
    #[error("duplicate prepared program definition: {0}")]
    DuplicateDefinition(String),
    #[error("malformed prepared program: {0}")]
    Malformed(String),
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LinkError {
    #[error("missing imported value {0:?}")]
    MissingImport(SymbolIdentity),
    #[error("imported value contract mismatch for {0:?}")]
    ImportContract(SymbolIdentity),
}

mod codec;
mod decode;
mod link;
mod validation;

pub mod testing;

pub use decode::parse_program;
pub use link::link_program;

// The decoder and linker live below this shared contract. Keeping these
// constructors crate-private prevents a partially checked program escaping
// while the implementation is split across focused waves.
pub(super) fn prepared_from_validated(wire: WireProgram) -> PreparedProgram {
    PreparedProgram { wire }
}

pub(super) fn linked_from_validated(
    prepared: PreparedProgram,
    imports: Vec<ImportedValue>,
) -> LinkedProgram {
    LinkedProgram { prepared, imports }
}
