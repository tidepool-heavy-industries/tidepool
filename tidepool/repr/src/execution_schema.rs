//! Invariant-bearing prepared-STG execution schema.
//!
//! Wire decoding is deliberately kept in this module: callers can construct a
//! [`WireProgram`] for encoding and tests, but executable consumers only receive
//! a validated [`PreparedProgram`] and an atomically linked [`LinkedProgram`].

use std::collections::BTreeMap;
use std::sync::Arc;

mod shared_content;
use shared_content::SharedContent;

use crate::session_ids::SessionVarId;
use crate::type_graph::TypeGraph;
pub use crate::type_graph::TypeNode;

pub const SCHEMA_VERSION: u64 = 17;
pub const EXECUTION_ABI_VERSION: u64 = 9;

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
dense_id!(TypeNodeId);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Architecture {
    X86_64,
    Aarch64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Endianness {
    Little,
    Big,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TargetDescriptor {
    pub architecture: Architecture,
    pub endianness: Endianness,
    pub pointer_width: u8,
    pub word_width: u8,
    pub abi: String,
    pub features: Vec<String>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ProgramEnvelope {
    pub schema_version: u64,
    pub projection_profile: String,
    pub toolchain: String,
    pub execution_abi_version: u64,
    pub target: TargetDescriptor,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SymbolIdentity {
    pub unit: String,
    pub module: String,
    pub namespace: String,
    pub occurrence: String,
    /// GHC record-field parent; absent for ordinary names and internal binders.
    pub record_parent: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RuntimeRep {
    Void,
    LiftedRef,
    UnliftedRef,
    Address,
    Int(u8),
    Word(u8),
    Float(u8),
}

impl RuntimeRep {
    /// Same machine value: identical, or a same-width signed/unsigned pair.
    #[must_use]
    pub fn same_bits(self, other: Self) -> bool {
        self == other
            || matches!((self, other), (Self::Int(a), Self::Word(b)) | (Self::Word(a), Self::Int(b)) if a == b)
    }
}

/// Known successful results, caller-chosen results, or authoritative evidence
/// that saturation cannot return. `Returns([])` is a successful zero-result
/// call, distinct from both other cases. Partial application still produces a
/// lifted function value regardless of the saturated result contract.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ResultContract {
    Returns(Vec<RuntimeRep>),
    NoSuccess,
    /// The caller supplies a concrete successful result representation.
    CallerResult,
}

impl ResultContract {
    pub fn is_caller_result(&self) -> bool {
        matches!(self, Self::CallerResult)
    }

    /// Known logical results. None means either nonreturning or caller-chosen;
    /// consumers lowering an ABI must distinguish those contracts explicitly.
    pub fn returned_reps(&self) -> Option<&[RuntimeRep]> {
        match self {
            Self::Returns(reps) => Some(reps),
            Self::NoSuccess | Self::CallerResult => None,
        }
    }

    /// A nonreturning expression satisfies any continuation demand. A demand
    /// alone is not evidence: ordinary returning expressions must match exactly.
    pub fn satisfies(&self, demanded: &Self) -> bool {
        matches!(self, Self::NoSuccess) || self == demanded
    }

    /// [`Self::satisfies`], also admitting a same-width signed/unsigned
    /// integer in place of the demanded one: GHC erases `Int#`/`Word#`
    /// coercions, and both share one bit pattern and machine type.
    pub fn satisfies_physically(&self, demanded: &Self) -> bool {
        self.satisfies(demanded)
            || matches!((self, demanded), (Self::Returns(actual), Self::Returns(expected))
                if actual.len() == expected.len()
                    && actual.iter().zip(expected).all(|(a, e)| a.same_bits(*e)))
    }

    /// Meet branches at a case continuation without inventing results for a
    /// nonreturning branch. Different successful representations are incompatible.
    pub fn merge_alternative(&self, other: &Self) -> Option<Self> {
        match (self, other) {
            (Self::NoSuccess, result) | (result, Self::NoSuccess) => Some(result.clone()),
            _ if self == other => Some(self.clone()),
            // Same machine values under an erased Int#/Word# coercion.
            _ if self.satisfies_physically(other) => Some(self.clone()),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Signature {
    pub arguments: Vec<RuntimeRep>,
    pub results: ResultContract,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum LayoutError {
    #[error("unsupported storage representation {0:?}")]
    UnsupportedRepresentation(RuntimeRep),
    #[error("invalid target pointer width {0}")]
    InvalidPointerWidth(u8),
    #[error("storage layout size overflow")]
    Overflow,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
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
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
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

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct FieldLayout {
    pub rep: RuntimeRep,
    pub offset: u32,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CheckedLayout {
    pub fields: Vec<FieldLayout>,
    pub alignment: u32,
    pub payload_size: u32,
    pub root_mask: Vec<bool>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
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

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SiteDelivery {
    HostAnswer,
    LiveReentry,
    ExitCellFill,
    TerminalCapture,
}

/// Compiler-attested reply interpretation for an exact request constructor.
/// `AtSite` alone authorizes the first field as an erased `RequestSite reply`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ConstructorReply {
    Static(TypeNodeId),
    /// A closed reply and a separately authenticated original input site.
    StaticWithSite {
        reply: TypeNodeId,
        field: u32,
        payload_field: u32,
        capture_input: Option<u32>,
    },
    AtSite,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SiteRow {
    pub site: u64,
    pub origin: String,
    pub ordinal: u64,
    pub delivery: SiteDelivery,
    pub wire: TypeNodeId,
    pub inputs: Vec<TypeNodeId>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct GlobalDecl {
    pub identity: SymbolIdentity,
    pub rep: RuntimeRep,
    /// Required entry evidence when GHC knows the closure's entry arity.
    /// Unknown lifted values must not acquire an entry from their full type.
    pub entry_signature: Option<SignatureId>,
    pub required_evaluated: bool,
    pub required_generation: Option<u64>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ValueRef {
    Local(ValueId),
    Global(GlobalId),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
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

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
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

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Group<T> {
    NonRecursive(T),
    Recursive(Vec<T>),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
/// Thunk entry policy only. GHC's `ReEntrant` is represented by
/// `HeapRhs::Function`, not a third thunk policy; `JumpedTo` belongs to joins.
pub enum UpdatePolicy {
    Memoize,
    SingleEntry,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct HeapBinding<B = usize> {
    pub id: ValueId,
    pub rhs: HeapRhs<B>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
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

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct JoinBinding<B = usize> {
    pub id: JoinId,
    pub signature: SignatureId,
    pub parameters: Vec<ValueId>,
    pub body: B,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum AlternativePattern {
    Default,
    Constructor(ConstructorId),
    Literal(ScalarLiteral),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
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
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum CaseKind {
    Algebraic(SymbolIdentity),
    Primitive(RuntimeRep),
    /// One tuple alternative binds the returned physical components directly.
    MultiValue,
    /// A single DEFAULT demands the scrutinee without inspecting its shape.
    Polymorphic,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
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

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct OperationDecl {
    pub identity: OperationIdentity,
    pub signature: SignatureId,
}

/// The compiler-authenticated representation of the vendored JSON value
/// family.  The type parameter lets each boundary retain the same named roles
/// while translating local schema IDs to runtime IDs or descriptors.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct JsonLayout<T = ConstructorId> {
    pub object: T,
    pub array: T,
    pub string: T,
    pub number: T,
    pub bool_: T,
    pub null: T,
    pub map_bin: T,
    pub map_tip: T,
    pub true_: T,
    pub false_: T,
    pub cons: T,
    pub nil: T,
    pub scientific: T,
    pub integer_small: T,
    pub integer_positive: T,
    pub integer_negative: T,
    pub text: T,
    pub int: T,
}

impl<T> JsonLayout<T> {
    pub const ROLE_COUNT: usize = 18;

    pub fn map<U>(self, mut map: impl FnMut(T) -> U) -> JsonLayout<U> {
        JsonLayout {
            object: map(self.object),
            array: map(self.array),
            string: map(self.string),
            number: map(self.number),
            bool_: map(self.bool_),
            null: map(self.null),
            map_bin: map(self.map_bin),
            map_tip: map(self.map_tip),
            true_: map(self.true_),
            false_: map(self.false_),
            cons: map(self.cons),
            nil: map(self.nil),
            scientific: map(self.scientific),
            integer_small: map(self.integer_small),
            integer_positive: map(self.integer_positive),
            integer_negative: map(self.integer_negative),
            text: map(self.text),
            int: map(self.int),
        }
    }

    pub fn as_ref(&self) -> JsonLayout<&T> {
        JsonLayout {
            object: &self.object,
            array: &self.array,
            string: &self.string,
            number: &self.number,
            bool_: &self.bool_,
            null: &self.null,
            map_bin: &self.map_bin,
            map_tip: &self.map_tip,
            true_: &self.true_,
            false_: &self.false_,
            cons: &self.cons,
            nil: &self.nil,
            scientific: &self.scientific,
            integer_small: &self.integer_small,
            integer_positive: &self.integer_positive,
            integer_negative: &self.integer_negative,
            text: &self.text,
            int: &self.int,
        }
    }

    pub fn try_map<U, E>(self, mut map: impl FnMut(T) -> Result<U, E>) -> Result<JsonLayout<U>, E> {
        Ok(JsonLayout {
            object: map(self.object)?,
            array: map(self.array)?,
            string: map(self.string)?,
            number: map(self.number)?,
            bool_: map(self.bool_)?,
            null: map(self.null)?,
            map_bin: map(self.map_bin)?,
            map_tip: map(self.map_tip)?,
            true_: map(self.true_)?,
            false_: map(self.false_)?,
            cons: map(self.cons)?,
            nil: map(self.nil)?,
            scientific: map(self.scientific)?,
            integer_small: map(self.integer_small)?,
            integer_positive: map(self.integer_positive)?,
            integer_negative: map(self.integer_negative)?,
            text: map(self.text)?,
            int: map(self.int)?,
        })
    }
}

/// Primops and admitted foreign capabilities occupy distinct identity spaces.
/// The declaration signature completes the operation's identity; the same
/// primop may occur at more than one instantiated signature.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum OperationIdentity {
    PrimOp(String),
    Intrinsic {
        symbol: String,
        convention: ForeignConvention,
    },
    JsonDecode {
        left: ConstructorId,
        right: ConstructorId,
    },
    JsonEncode,
    /// An explicitly catalogued missing runtime capability, never an arbitrary
    /// unresolved import. Keeps GHC's Returns signature; execution fails without
    /// publishing a result. Native admission checks the exact name/signature.
    Capability {
        name: String,
    },
    /// Authoritative GHC wired-in identity lowered by the producer to an ordinary
    /// callable top. Its saturated signature is Address -> NoSuccess, except the
    /// nullary AbsentSumField worker. Runtime disposition is not encoded here.
    WiredInError {
        kind: WiredInErrorKind,
    },
}

/// Stable wire tags in declaration order (0..10). Match producer GHC keys, not
/// user-visible occurrence strings. The impossible/absent members signal violated
/// compiler invariants; the remaining members are recoverable language failures.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum WiredInErrorKind {
    PatternMatch = 0,
    NonExhaustiveGuards = 1,
    RecordSelector = 2,
    RecordConstruction = 3,
    NoMethodBinding = 4,
    DeferredType = 5,
    Impossible = 6,
    ImpossibleConstraint = 7,
    Absent = 8,
    AbsentConstraint = 9,
    AbsentSumField = 10,
}

impl WiredInErrorKind {
    pub fn is_integrity_failure(self) -> bool {
        matches!(
            self,
            Self::Impossible
                | Self::ImpossibleConstraint
                | Self::Absent
                | Self::AbsentConstraint
                | Self::AbsentSumField
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ForeignConvention {
    CCall,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TopBinding {
    pub identity: SymbolIdentity,
    pub binding: HeapBinding,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct WireProgram {
    pub envelope: ProgramEnvelope,
    pub signatures: Vec<Signature>,
    pub globals: Vec<GlobalDecl>,
    pub constructors: Vec<ConstructorDecl>,
    pub operations: Vec<OperationDecl>,
    pub expressions: Expr,
    pub bindings: Vec<Group<TopBinding>>,
    pub entry: ValueId,
    pub types: Arc<TypeGraph>,
    pub sites: Vec<SiteRow>,
    /// Exact request constructors paired with compiler-issued reply evidence.
    pub constructor_replies: Vec<(ConstructorId, ConstructorReply)>,
    /// Compiler-issued JSON constructor evidence.  It is carried even when a
    /// program only mounts or answers JSON and has no JSON intrinsic call.
    pub json_layout: Option<JsonLayout>,
}

/// The entry-free definition and table portion shared by complete programs
/// and projected module groups. A neutral product cannot name an executable
/// root until graph ownership has been checked and a real entry is selected.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ProgramDefinitions {
    pub envelope: ProgramEnvelope,
    pub signatures: Vec<Signature>,
    pub globals: Vec<GlobalDecl>,
    pub constructors: Vec<ConstructorDecl>,
    pub operations: Vec<OperationDecl>,
    pub expressions: Expr,
    pub bindings: Vec<Group<TopBinding>>,
    pub types: Arc<TypeGraph>,
    pub sites: Vec<SiteRow>,
    pub constructor_replies: Vec<(ConstructorId, ConstructorReply)>,
    pub json_layout: Option<JsonLayout>,
}

impl ProgramDefinitions {
    fn into_wire(self, entry: ValueId) -> WireProgram {
        WireProgram {
            envelope: self.envelope,
            signatures: self.signatures,
            globals: self.globals,
            constructors: self.constructors,
            operations: self.operations,
            expressions: self.expressions,
            bindings: self.bindings,
            entry,
            types: self.types,
            sites: self.sites,
            constructor_replies: self.constructor_replies,
            json_layout: self.json_layout,
        }
    }
}

/// One borrowed semantic view for full programs and entry-free groups. The
/// validator receives the entry separately, so neutral definitions never
/// carry a fabricated root.
#[derive(Clone, Copy)]
pub struct DefinitionsView<'a> {
    envelope: &'a ProgramEnvelope,
    signatures: &'a Vec<Signature>,
    globals: &'a Vec<GlobalDecl>,
    constructors: &'a Vec<ConstructorDecl>,
    operations: &'a Vec<OperationDecl>,
    expressions: &'a Expr,
    bindings: &'a Vec<Group<TopBinding>>,
    types: &'a Arc<TypeGraph>,
    sites: &'a Vec<SiteRow>,
    constructor_replies: &'a Vec<(ConstructorId, ConstructorReply)>,
    json_layout: &'a Option<JsonLayout>,
}

impl<'a> DefinitionsView<'a> {
    pub fn envelope(self) -> &'a ProgramEnvelope {
        self.envelope
    }
    pub fn signatures(self) -> &'a [Signature] {
        self.signatures
    }
    pub fn globals(self) -> &'a [GlobalDecl] {
        self.globals
    }
    pub fn constructors(self) -> &'a [ConstructorDecl] {
        self.constructors
    }
    pub fn operations(self) -> &'a [OperationDecl] {
        self.operations
    }
    pub fn expressions(self) -> &'a Expr {
        self.expressions
    }
    pub fn bindings(self) -> &'a [Group<TopBinding>] {
        self.bindings
    }
    pub fn types(self) -> &'a Arc<TypeGraph> {
        self.types
    }
    pub fn sites(self) -> &'a [SiteRow] {
        self.sites
    }
    pub fn constructor_replies(self) -> &'a [(ConstructorId, ConstructorReply)] {
        self.constructor_replies
    }
    pub fn json_layout(self) -> Option<&'a JsonLayout> {
        self.json_layout.as_ref()
    }
}

macro_rules! definitions_view {
    ($value:expr) => {
        DefinitionsView {
            envelope: &$value.envelope,
            signatures: &$value.signatures,
            globals: &$value.globals,
            constructors: &$value.constructors,
            operations: &$value.operations,
            expressions: &$value.expressions,
            bindings: &$value.bindings,
            types: &$value.types,
            sites: &$value.sites,
            constructor_replies: &$value.constructor_replies,
            json_layout: &$value.json_layout,
        }
    };
}

impl<'a> From<&'a WireProgram> for DefinitionsView<'a> {
    fn from(value: &'a WireProgram) -> Self {
        definitions_view!(value)
    }
}

impl<'a> From<&'a ProgramDefinitions> for DefinitionsView<'a> {
    fn from(value: &'a ProgramDefinitions) -> Self {
        definitions_view!(value)
    }
}

/// Validated but not yet linked program. Its fields remain private so every
/// executable consumer crosses the same validation boundary.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PreparedProgram {
    wire: SharedContent<WireProgram>,
}

impl PreparedProgram {
    pub fn definitions(&self) -> DefinitionsView<'_> {
        DefinitionsView::from(self.wire.as_ref())
    }

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
    pub fn types(&self) -> &Arc<TypeGraph> {
        &self.wire.types
    }
    pub fn sites(&self) -> &[SiteRow] {
        &self.wire.sites
    }
    pub fn constructor_replies(&self) -> &[(ConstructorId, ConstructorReply)] {
        &self.wire.constructor_replies
    }
    pub fn json_layout(&self) -> Option<&JsonLayout> {
        self.wire.json_layout.as_ref()
    }
    pub fn site(&self, site: u64) -> Option<&SiteRow> {
        self.wire.sites.iter().find(|row| row.site == site)
    }
    pub fn type_node(&self, id: TypeNodeId) -> Option<&TypeNode> {
        self.wire
            .types
            .graph()
            .node_weight(crate::type_graph::TypeNodeId::new(id.0 as usize))
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ImportedValue {
    pub identity: SymbolIdentity,
    pub rep: RuntimeRep,
    /// Semantic signature supplied by the binding owner. Signature IDs are
    /// module-local table indices and therefore cannot cross the link boundary.
    pub entry_signature: Option<Signature>,
    pub evaluated: bool,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
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

#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct MachineImports {
    pub values: BTreeMap<SymbolIdentity, ImportedValue>,
}

/// A content identity assigned to a complete ordinary/boot module graph
/// before exact import edges are annotated. The digest includes its scoped
/// compiler context and SCC closure, never mutable binding values.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ModuleVersion(pub [u8; 32]);

/// The semantic owner of a projected external value. A spelling and physical
/// signature can corroborate an owner but cannot select one.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ImportOwner {
    Source {
        version: ModuleVersion,
        binder: SymbolIdentity,
    },
    Retained {
        id: SessionVarId,
        generation: u64,
    },
    /// An immutable native export selected from the owning machine's live
    /// ledger. `root_id` is its existing process-unique rooted-value identity,
    /// not a session binding id or a serialized compiler authority.
    CodeExport {
        binder: SymbolIdentity,
        generation: u64,
        root_id: u64,
        /// Producer-authenticated package provenance, when required by a
        /// retained-package certificate. It never authorizes a missing export.
        interface_digest: Option<[u8; 32]>,
    },
    Package {
        unit: String,
        module: String,
        binder: SymbolIdentity,
        interface_digest: [u8; 32],
    },
}

/// One original recursive-group arena with no selected executable entry.
/// Its imports remain unavailable until the graph inventory supplies one
/// exact owner per declared external value.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ProjectedGroup {
    original_ordinal: u32,
    binders: SharedContent<Vec<SymbolIdentity>>,
    definitions: SharedContent<ProgramDefinitions>,
}

impl ProjectedGroup {
    pub fn definitions(&self) -> DefinitionsView<'_> {
        DefinitionsView::from(self.definitions.as_ref())
    }

    #[must_use]
    pub fn original_ordinal(&self) -> u32 {
        self.original_ordinal
    }

    #[must_use]
    pub fn binders(&self) -> &[SymbolIdentity] {
        &self.binders
    }

    #[must_use]
    pub fn globals(&self) -> &[GlobalDecl] {
        &self.definitions.globals
    }
}

/// Immutable provenance of one reusable source module product. Every digest
/// is filled from worker-certified bytes before native compilation.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CachedHomeOwner {
    pub unit: String,
    pub module: String,
    pub module_version: ModuleVersion,
    pub skinny_iface_sha256: [u8; 32],
    pub product_sha256: [u8; 32],
}

/// One neutral recursive group with its original owner and an exact owner for
/// each global in the group's local declaration order.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CertifiedGroup {
    owner: Arc<CachedHomeOwner>,
    group: ProjectedGroup,
    imports: Arc<[ImportOwner]>,
}

/// Native compilation identity issued from a checked source group. It keeps
/// exact immutable source provenance and definitions; machine-local import
/// owners remain in CertifiedGroup and are checked for every installation.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CertifiedGroupCode {
    owner: Arc<CachedHomeOwner>,
    group: ProjectedGroup,
}

impl CertifiedGroupCode {
    /// Validate immutable source identity without resolving installation imports.
    pub fn admit(owner: CachedHomeOwner, group: ProjectedGroup) -> Result<Self, ParseError> {
        if owner.unit.is_empty() || owner.module.is_empty() {
            return Err(ParseError::InvalidReference(
                "empty cached home owner".into(),
            ));
        }
        let tops: Vec<_> = group
            .definitions
            .bindings
            .iter()
            .flat_map(|binding| match binding {
                Group::NonRecursive(top) => std::slice::from_ref(top),
                Group::Recursive(tops) => tops.as_slice(),
            })
            .map(|top| &top.identity)
            .collect();
        let unique: std::collections::BTreeSet<_> = tops.iter().copied().collect();
        if unique.len() != tops.len()
            || tops.len() < group.binders.len()
            || tops[tops.len() - group.binders.len()..] != group.binders.iter().collect::<Vec<_>>()
            || group
                .binders
                .iter()
                .any(|binder| binder.unit != owner.unit || binder.module != owner.module)
        {
            return Err(ParseError::InvalidReference(
                "cached group binder inventory differs from its definitions".into(),
            ));
        }
        Ok(Self {
            owner: Arc::new(owner),
            group,
        })
    }

    pub fn owner(&self) -> &CachedHomeOwner {
        &self.owner
    }
    pub fn original_ordinal(&self) -> u32 {
        self.group.original_ordinal()
    }
    pub fn binders(&self) -> &[SymbolIdentity] {
        self.group.binders()
    }
    pub fn definitions(&self) -> DefinitionsView<'_> {
        self.group.definitions()
    }
}

impl CertifiedGroup {
    pub fn admit(
        owner: CachedHomeOwner,
        group: ProjectedGroup,
        imports: Vec<ImportOwner>,
    ) -> Result<Self, ParseError> {
        let code = CertifiedGroupCode::admit(owner, group)?;
        let group = &code.group;
        if imports.len() != group.globals().len() {
            return Err(ParseError::InvalidReference(
                "cached group import count differs from its globals".into(),
            ));
        }
        for (declaration, origin) in group.globals().iter().zip(&imports) {
            let consistent = match origin {
                ImportOwner::Source { binder, .. } => {
                    binder == &declaration.identity && declaration.required_generation.is_none()
                }
                ImportOwner::Retained { generation, .. } => {
                    declaration.required_generation == Some(*generation)
                }
                ImportOwner::CodeExport {
                    binder, generation, ..
                } => {
                    binder == &declaration.identity
                        && declaration.required_generation == Some(*generation)
                }
                ImportOwner::Package {
                    unit,
                    module,
                    binder,
                    ..
                } => {
                    binder == &declaration.identity
                        && &binder.unit == unit
                        && &binder.module == module
                        && declaration.required_generation.is_none()
                }
            };
            if !consistent {
                return Err(ParseError::InvalidReference(format!(
                    "cached group import owner differs from {:?}",
                    declaration.identity
                )));
            }
        }
        Ok(Self {
            owner: code.owner,
            group: code.group,
            imports: imports.into(),
        })
    }

    #[must_use]
    pub fn code_identity(&self) -> CertifiedGroupCode {
        CertifiedGroupCode {
            owner: Arc::clone(&self.owner),
            group: self.group.clone(),
        }
    }

    pub fn owner(&self) -> &CachedHomeOwner {
        &self.owner
    }
    pub fn original_ordinal(&self) -> u32 {
        self.group.original_ordinal()
    }
    pub fn binders(&self) -> &[SymbolIdentity] {
        self.group.binders()
    }
    pub fn imports(&self) -> &[ImportOwner] {
        &self.imports
    }
    pub fn definitions(&self) -> DefinitionsView<'_> {
        self.group.definitions()
    }
}

/// Parse a neutral group without selecting an entry. The shared prepared
/// validator checks all expressions, layouts and tables, omitting only the
/// entry-specific function result rule.
pub fn parse_projected_group(
    bytes: &[u8],
    requirements: &ProgramRequirements,
    limits: DecodeLimits,
) -> Result<ProjectedGroup, ParseError> {
    let mut budget = OperationBudget::new(limits.max_work);
    parse_projected_group_with_budget(bytes, requirements, limits, &mut budget)
}

fn parse_projected_group_with_budget(
    bytes: &[u8],
    requirements: &ProgramRequirements,
    limits: DecodeLimits,
    budget: &mut OperationBudget,
) -> Result<ProjectedGroup, ParseError> {
    let (original_ordinal, binders, definitions) = codec::decode_group_wire(bytes, limits, budget)?;
    validation::validate_group_with_budget(&definitions, requirements, limits, budget)?;
    budget.charge(binders.len())?;
    let unique: std::collections::BTreeSet<_> = binders.iter().collect();
    if unique.len() != binders.len() {
        return Err(ParseError::DuplicateDefinition(
            "projected group binder".into(),
        ));
    }
    Ok(ProjectedGroup {
        original_ordinal,
        binders: SharedContent::new(binders),
        definitions: SharedContent::new(definitions),
    })
}

/// Version of the module-product envelope; group payloads retain their own
/// current prepared schema and execution ABI.
pub const MODULE_PRODUCTS_VERSION: u64 = 1;

/// One source module's skinny GHC interface and entry-free definitions as
/// emitted by a single compiler transaction. This is not cache admission:
/// the toolchain must still attach complete graph evidence and exact owners.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawModuleProduct {
    pub unit: String,
    pub module: String,
    pub interface: Vec<u8>,
    pub groups: Vec<ProjectedGroup>,
}

/// Read the worker sidecar that atomically pairs skinny interfaces with
/// neutral definitions. Every group crosses the normal bounded semantic
/// validator before the bundle is returned.
pub fn parse_module_products(
    bytes: &[u8],
    requirements: &ProgramRequirements,
    limits: InventoryDecodeLimits,
) -> Result<Vec<RawModuleProduct>, ParseError> {
    InventoryOperation::new(limits).parse_module_products(bytes, requirements)
}

/// Decode products and normalize each singleton sidecar from the same bounded
/// wire value. Interface and opaque projected-group bytes are preserved.
pub fn parse_module_products_with_framing(
    bytes: &[u8],
    requirements: &ProgramRequirements,
    limits: InventoryDecodeLimits,
) -> Result<(Vec<RawModuleProduct>, Vec<Vec<u8>>), ParseError> {
    InventoryOperation::new(limits).parse_module_products_with_framing(bytes, requirements)
}

/// Resource policy for an inventory of independently bounded original owners.
/// Program limits apply to each embedded executable, never to the inventory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InventoryDecodeLimits {
    pub max_bytes: usize,
    pub max_module_bytes: usize,
    pub program: DecodeLimits,
    pub max_work: usize,
}

impl Default for InventoryDecodeLimits {
    fn default() -> Self {
        Self {
            max_bytes: 4 << 30,
            max_module_bytes: 64 << 20,
            program: DecodeLimits {
                max_bytes: 64 << 20,
                ..DecodeLimits::default()
            },
            max_work: 4 << 30,
        }
    }
}

/// One non-cloneable accounting owner for an entire inventory admission.
/// Charges cover visits, reserved container storage and copies, not process RSS.
/// The cell permits nested receipt owners to share accounting without issuing
/// independently reset budgets. It contains no compiler or publication authority.
pub struct InventoryOperation {
    limits: InventoryDecodeLimits,
    budget: std::sync::Mutex<OperationBudget>,
}

impl InventoryOperation {
    pub fn new(limits: InventoryDecodeLimits) -> Self {
        Self {
            limits,
            budget: std::sync::Mutex::new(OperationBudget::new(limits.max_work)),
        }
    }

    pub fn limits(&self) -> InventoryDecodeLimits {
        self.limits
    }

    /// Cumulative admission work, for observation only; never resets the owner.
    pub fn work_usage(&self) -> Result<(usize, usize), ParseError> {
        let budget = self
            .budget
            .lock()
            .map_err(|_| ParseError::LimitExceeded("accounting owner"))?;
        let remaining = budget.remaining();
        Ok((self.limits.max_work - remaining, remaining))
    }

    pub fn charge(&self, amount: usize) -> Result<(), ParseError> {
        self.budget
            .lock()
            .map_err(|_| ParseError::LimitExceeded("accounting owner"))?
            .charge(amount)
    }

    pub fn reserve<T>(&self, count: usize) -> Result<(), ParseError> {
        self.budget
            .lock()
            .map_err(|_| ParseError::LimitExceeded("accounting owner"))?
            .reserve::<T>(count)
    }

    /// Reserve decoded-container and payload copies before a nested owner
    /// constructs its typed representation. Map nodes include conservative
    /// pointer/link storage in addition to each value slot.
    pub fn charge_value_copies(
        &self,
        value: &ciborium::value::Value,
        copies: usize,
    ) -> Result<(), ParseError> {
        use ciborium::value::Value;
        self.reserve::<Value>(
            copies
                .checked_mul(4)
                .ok_or(ParseError::LimitExceeded("work"))?,
        )?;
        match value {
            Value::Bytes(bytes) => self.charge(
                bytes
                    .len()
                    .checked_mul(copies)
                    .ok_or(ParseError::LimitExceeded("work"))?,
            )?,
            Value::Text(text) => self.charge(
                text.len()
                    .checked_mul(copies)
                    .ok_or(ParseError::LimitExceeded("work"))?,
            )?,
            Value::Array(values) => {
                for value in values {
                    self.charge_value_copies(value, copies)?;
                }
            }
            Value::Map(values) => {
                for (key, value) in values {
                    self.charge_value_copies(key, copies)?;
                    self.charge_value_copies(value, copies)?;
                }
            }
            Value::Tag(_, value) => self.charge_value_copies(value, copies)?,
            _ => (),
        }
        Ok(())
    }

    /// Decode another representation belonging to this operation. All CBOR
    /// containers and payloads are reserved before the general decoder allocates.
    pub fn decode_value(
        &self,
        bytes: &[u8],
        max_bytes: usize,
    ) -> Result<ciborium::value::Value, ParseError> {
        let limits = DecodeLimits {
            max_bytes,
            ..self.limits.program
        };
        let mut budget = self
            .budget
            .lock()
            .map_err(|_| ParseError::LimitExceeded("accounting owner"))?;
        codec::decode_value(bytes, limits, &mut *budget)
    }

    pub fn parse_module_products(
        &self,
        bytes: &[u8],
        requirements: &ProgramRequirements,
    ) -> Result<Vec<RawModuleProduct>, ParseError> {
        let mut budget = self
            .budget
            .lock()
            .map_err(|_| ParseError::LimitExceeded("accounting owner"))?;
        parse_module_products_inner(bytes, requirements, self.limits, &mut *budget, |_, _, _| {
            Ok(())
        })
    }

    pub fn parse_module_products_with_framing(
        &self,
        bytes: &[u8],
        requirements: &ProgramRequirements,
    ) -> Result<(Vec<RawModuleProduct>, Vec<Vec<u8>>), ParseError> {
        let mut sidecars = Vec::new();
        let mut budget = self
            .budget
            .lock()
            .map_err(|_| ParseError::LimitExceeded("accounting owner"))?;
        let products = parse_module_products_inner(
            bytes,
            requirements,
            self.limits,
            &mut *budget,
            |row, size, budget| {
                budget.charge(size)?;
                budget.reserve::<Vec<u8>>(1)?;
                let mut singleton = Vec::with_capacity(size);
                ciborium::ser::into_writer(
                    &("TPMOD", MODULE_PRODUCTS_VERSION, [row]),
                    &mut singleton,
                )
                .map_err(|error| {
                    ParseError::Malformed(format!("module product framing: {error}"))
                })?;
                sidecars.push(singleton);
                Ok(())
            },
        )?;
        Ok((products, sidecars))
    }
}

/// Count the existing serde encoding before allocating a normalized row.
struct CountingWriter<'a> {
    bytes: usize,
    limit: usize,
    budget: &'a mut OperationBudget,
    error: Option<ParseError>,
}
impl std::io::Write for CountingWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("module byte count overflow"))?;
        if self.bytes > self.limit {
            return Err(std::io::Error::other("module byte limit"));
        }
        if let Err(error) = self.budget.charge(bytes.len()) {
            self.error = Some(error);
            return Err(std::io::Error::other("counting work limit"));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn normalized_module_size(
    row: &ciborium::value::Value,
    limit: usize,
    budget: &mut OperationBudget,
) -> Result<usize, ParseError> {
    let mut writer = CountingWriter {
        bytes: 0,
        limit,
        budget,
        error: None,
    };
    if ciborium::ser::into_writer(&("TPMOD", MODULE_PRODUCTS_VERSION, [row]), &mut writer).is_err()
    {
        return Err(writer.error.take().unwrap_or(ParseError::ModuleByteLimit {
            limit,
            actual: writer.bytes,
        }));
    }
    Ok(writer.bytes)
}

fn parse_module_products_inner(
    bytes: &[u8],
    requirements: &ProgramRequirements,
    inventory: InventoryDecodeLimits,
    budget: &mut OperationBudget,
    mut normalized_row: impl FnMut(
        &ciborium::value::Value,
        usize,
        &mut OperationBudget,
    ) -> Result<(), ParseError>,
) -> Result<Vec<RawModuleProduct>, ParseError> {
    use ciborium::value::Value;

    if bytes.len() > inventory.max_bytes {
        return Err(ParseError::InventoryByteLimit {
            limit: inventory.max_bytes,
            actual: bytes.len(),
        });
    }
    let limits = inventory.program;
    let outer_limits = DecodeLimits {
        max_bytes: inventory.max_bytes,
        ..limits
    };
    let Value::Array(header) = codec::decode_value(bytes, outer_limits, budget)? else {
        return Err(ParseError::Malformed(
            "module products require an array".into(),
        ));
    };
    if header.len() != 3
        || header[0] != Value::Text("TPMOD".into())
        || header[1] != Value::Integer(MODULE_PRODUCTS_VERSION.into())
    {
        return Err(ParseError::Malformed(
            "unsupported module products header".into(),
        ));
    }
    let Value::Array(modules) = &header[2] else {
        return Err(ParseError::Malformed(
            "module products require modules".into(),
        ));
    };
    budget.reserve::<RawModuleProduct>(modules.len())?;
    let mut seen_modules = std::collections::BTreeSet::new();
    let mut output = Vec::with_capacity(modules.len());
    for module in modules {
        let Value::Array(fields) = module else {
            return Err(ParseError::Malformed(
                "module product requires an array".into(),
            ));
        };
        let [Value::Text(unit), Value::Text(name), Value::Bytes(interface), Value::Array(groups)] =
            fields.as_slice()
        else {
            return Err(ParseError::Malformed(
                "invalid module product fields".into(),
            ));
        };
        if unit.is_empty()
            || name.is_empty()
            || interface.is_empty()
            || unit.len() > limits.max_string_bytes
            || name.len() > limits.max_string_bytes
        {
            return Err(ParseError::Malformed(
                "invalid module product identity or interface".into(),
            ));
        }
        let identity_bytes = unit
            .len()
            .checked_add(name.len())
            .ok_or(ParseError::LimitExceeded("work"))?;
        budget.charge(identity_bytes)?;
        if !seen_modules.insert((unit.clone(), name.clone())) {
            return Err(ParseError::DuplicateDefinition(format!(
                "module {unit}:{name}"
            )));
        }
        let normalized_size = normalized_module_size(module, inventory.max_module_bytes, budget)?;
        budget.reserve::<ProjectedGroup>(groups.len())?;
        let mut seen_ordinals = std::collections::BTreeSet::new();
        let mut seen_binders = std::collections::BTreeSet::new();
        let mut projected = Vec::with_capacity(groups.len());
        for group in groups {
            let Value::Bytes(group_bytes) = group else {
                return Err(ParseError::Malformed(
                    "projected group requires bytes".into(),
                ));
            };
            let parsed =
                parse_projected_group_with_budget(group_bytes, requirements, limits, budget)?;
            if !seen_ordinals.insert(parsed.original_ordinal()) {
                return Err(ParseError::DuplicateDefinition(
                    "original group ordinal".into(),
                ));
            }
            for binder in parsed.binders() {
                budget.charge_symbol_copy(binder)?;
                if binder.unit != *unit
                    || binder.module != *name
                    || !seen_binders.insert(binder.clone())
                {
                    return Err(ParseError::InvalidReference(format!(
                        "invalid product binder {binder:?}"
                    )));
                }
            }
            projected.push(parsed);
        }
        normalized_row(module, normalized_size, budget)?;
        budget.charge(
            identity_bytes
                .checked_add(interface.len())
                .ok_or(ParseError::LimitExceeded("work"))?,
        )?;
        output.push(RawModuleProduct {
            unit: unit.clone(),
            module: name.clone(),
            interface: interface.clone(),
            groups: projected,
        });
    }
    Ok(output)
}

/// Producer and target facts accepted by this execution consumer.
///
/// The decoder compares the artifact envelope with this value before
/// publishing a [`PreparedProgram`]. It must not infer target facts from the
/// decoder process.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ProgramRequirements {
    pub schema_version: u64,
    pub projection_profile: String,
    pub toolchain: String,
    pub execution_abi_version: u64,
    pub target: TargetDescriptor,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DecodeLimits {
    pub max_bytes: usize,
    pub max_nodes: usize,
    pub max_table_entries: usize,
    pub max_string_bytes: usize,
    pub max_work: usize,
    pub max_type_nodes: usize,
    pub max_sites: usize,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_bytes: 16 << 20,
            max_nodes: 1 << 20,
            max_table_entries: 1 << 18,
            max_string_bytes: 1 << 20,
            // Emergency ceiling for aggregate visits and copied bytes across
            // all module products and validation passes, not a memory limit.
            // Byte, table, node and depth checks remain independent.
            max_work: 4 << 30,
            max_type_nodes: 1 << 18,
            max_sites: 1 << 18,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum ParseError {
    #[error("module inventory exceeds {limit} byte limit ({actual})")]
    InventoryByteLimit { limit: usize, actual: usize },
    #[error("normalized module exceeds {limit} byte limit ({actual})")]
    ModuleByteLimit { limit: usize, actual: usize },
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

#[derive(Clone, Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum LinkError {
    #[error("missing imported value {0:?}")]
    MissingImport(SymbolIdentity),
    #[error("imported value contract mismatch for {0:?}")]
    ImportContract(SymbolIdentity),
}

/// Encode the finite type graph with the owning prepared-schema grammar.
/// Encoding describes structural data and does not issue compiler authority.
pub fn encode_type_graph_value(graph: &TypeGraph) -> ciborium::value::Value {
    codec::encode_type_graph_value(graph)
}

mod budget;
use budget::OperationBudget;

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
    PreparedProgram {
        wire: SharedContent::new(wire),
    }
}

pub(super) fn linked_from_validated(
    prepared: PreparedProgram,
    imports: Vec<ImportedValue>,
) -> LinkedProgram {
    LinkedProgram { prepared, imports }
}
