//! Host-built answers for parked prepared continuations.
//!
//! The session validates a bridge `Value` against a parked frame's site
//! evidence and lowers it to an [`AnswerPlan`]: constructors named by their
//! bridge `DataConId` and scalars already target-encoded. The machine resolves
//! every constructor through its interner (`by_host`) and records a compact
//! postorder build. The machine constructs one object at a time while keeping
//! completed children in fixed-address temporary root slots, so collection
//! may happen between objects and the whole answer need not fit in one span.
//!
//! A byte-backed leaf (`ByteArray#`: the backing of `Text` and of a
//! multi-limb `Integer`/`Natural`) is one external payload plus its managed
//! wrapper object. Capacity is established before allocating its payload, and
//! no collection occurs between payload initialization and publishing the
//! fully initialized wrapper in its temporary root. Once published, ordinary
//! reachability owns reclamation if a later step fails.

use std::sync::Arc;

use tidepool_heap::execution_descriptor::ObjectDescriptor;
use tidepool_heap::external_storage::ExternalStorageValidationError;
use tidepool_repr::execution_schema::RuntimeRep;
use tidepool_repr::DataConId;

use super::machine::PreparedHandle;
use crate::descriptor_bridge::{
    marshal_descriptor_object, DescriptorMarshalError, DescriptorValue,
};

/// A validated answer, ready to build. Field order is the constructor's
/// logical field order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnswerPlan {
    Constructor {
        host_id: DataConId,
        fields: Vec<AnswerPlan>,
    },
    /// A scalar field, target-encoded; only the field's declared width is
    /// written.
    Scalar { rep: RuntimeRep, bits: [u8; 16] },
    /// An unlifted `ByteArray#` field holding exactly these bytes.
    Bytes(Vec<u8>),
    /// A `Value`-carrying leaf rendered as JSON text, not yet decoded: the
    /// session's `lower_answer` produces this in place of walking an
    /// unconstructible `Tidepool.Aeson.Value.Value` node. `session::prepared`
    /// resolves every `Json` leaf of a plan to a [`Self::Handle`] (by
    /// entering the program's decode root) before the plan reaches
    /// [`FlattenedAnswer::resolve`] — a `Json` leaf reaching the builder is
    /// an invariant violation, not an ordinary refusal.
    Json(String),
    /// A value already retained elsewhere in this program's heap, borrowed as
    /// this field's reference: the decoded outer constructor of a resolved
    /// `Json` leaf, or a caller-supplied framed-handle delivery. The build
    /// does not release it; the caller's own custody governs its lifetime.
    Handle(PreparedHandle),
}

/// Why a plan could not be built. No variant publishes an answer handle.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AnswerBuildError {
    #[error("byte storage for the answer could not be allocated: {0:?}")]
    Storage(ExternalStorageValidationError),
    /// The program's own `ByteArray#` wrapper descriptor refused its single
    /// address field: an integrity failure, not an answer-shape one.
    #[error("byte-array wrapper: {0}")]
    Wrapper(DescriptorMarshalError),
    #[error("constructor {0:?} is not declared by any installed program")]
    UnknownConstructor(DataConId),
    /// The plan's root is a bare `Scalar` or `Bytes` leaf, not a
    /// `Constructor`. A host answer names the lifted value it hands back to
    /// generated code, and only a constructor is a heap object with a
    /// taggable reference; there is nothing to root otherwise.
    #[error(
        "a host answer must be a constructor; a bare scalar or byte array cannot be a lifted \
         value"
    )]
    UnboxedRoot,
    #[error("constructor {host_id:?} takes {expected} fields, the answer supplies {actual}")]
    FieldCount {
        host_id: DataConId,
        expected: usize,
        actual: usize,
    },
    #[error("constructor {host_id:?} field {index}: {error}")]
    Field {
        host_id: DataConId,
        index: usize,
        error: DescriptorMarshalError,
    },
    #[error("the answer nests deeper than {0} constructors")]
    TooDeep(usize),
    #[error("the answer needs {0} bytes, more than the nursery can hold")]
    TooLarge(usize),
    /// Generated code packs objects at word alignment; a descriptor asking
    /// for more cannot be laid out in the span the builder reserves.
    #[error("constructor {host_id:?} requires {required}-byte alignment, above the nursery's word alignment")]
    Alignment { host_id: DataConId, required: u32 },
    /// A plan still carried an unresolved [`AnswerPlan::Json`] leaf. The
    /// session's decode pre-pass resolves every one to a [`AnswerPlan::Handle`]
    /// before a plan reaches the builder; reaching this is an invariant
    /// violation in the caller, not a normal refusal.
    #[error("an answer plan reached the builder with an unresolved Value-carrying leaf")]
    UnresolvedJson,
    /// A [`AnswerPlan::Handle`] leaf named a handle no longer live in this
    /// engine's ledger (already released, or minted under another engine).
    #[error("an answer plan's borrowed handle is not live")]
    UnknownHandle,
}

/// Nesting bound for a plan: a bridge `Value` is an owned tree and the
/// builder recurses over it; a deeper tree is refused rather than risking the
/// native stack.
pub const MAX_ANSWER_DEPTH: usize = 4096;

/// One object of a flattened plan, in build (postorder) order.
struct PlannedObject {
    host_id: DataConId,
    descriptor: Arc<ObjectDescriptor>,
    fields: Vec<PlannedField>,
}

enum PlannedField {
    /// Index into the flattened object list.
    Object(usize),
    Scalar([u8; 16]),
    /// Index into the flattened byte-array list.
    Bytes(usize),
    /// Index into the flattened handle list — a borrowed reference, resolved
    /// to its live tagged word at write time (after any collection
    /// [`Self::resolve`]'s caller runs to make room, mirroring how byte
    /// payloads are allocated after sizing rather than during it).
    Handle(usize),
}

struct PlannedBytes {
    data: Vec<u8>,
}

/// A plan flattened against the machine's descriptors: every constructor
/// resolved, every field count checked, every object given its offset.
pub(super) struct FlattenedAnswer {
    objects: Vec<PlannedObject>,
    byte_arrays: Vec<PlannedBytes>,
    /// Borrowed handles referenced by [`AnswerPlan::Handle`] leaves, in the
    /// order [`Self::write`] expects their resolved tagged words.
    handles: Vec<PreparedHandle>,
    /// The external `Bytes` wrapper descriptor every byte array is written
    /// with, from the program the answer is built for.
    bytes_descriptor: Arc<ObjectDescriptor>,
    /// Index into `objects` of the plan's root, recorded once
    /// [`Self::resolve`] has refused a non-`Constructor` root: `write`
    /// returns this object's word rather than assuming the last object
    /// pushed is the root.
    root: usize,
}

/// The nursery's own allocation granularity: every generated allocation is a
/// whole number of words, so the exact-start scan can walk objects back to
/// back. The builder lays objects out the same way.
const WORD: usize = std::mem::size_of::<u64>();

fn align_up(offset: usize) -> usize {
    offset.div_ceil(WORD) * WORD
}

impl FlattenedAnswer {
    /// Resolve and size `plan`. Nothing here touches the heap.
    pub(super) fn resolve<'a>(
        plan: &AnswerPlan,
        resolve: &impl Fn(DataConId) -> Option<&'a Arc<ObjectDescriptor>>,
        bytes_descriptor: &Arc<ObjectDescriptor>,
    ) -> Result<Self, AnswerBuildError> {
        if !matches!(plan, AnswerPlan::Constructor { .. }) {
            return Err(AnswerBuildError::UnboxedRoot);
        }
        let mut flattened = Self {
            objects: Vec::new(),
            byte_arrays: Vec::new(),
            handles: Vec::new(),
            bytes_descriptor: Arc::clone(bytes_descriptor),
            root: 0,
        };
        flattened.visit(plan, resolve, 0)?;
        // The root was just refused above unless `plan` is `Constructor`, and
        // a `Constructor`'s own `visit` call pushes its `PlannedObject` last
        // (its fields are visited first, then itself), so the last entry is
        // always the root's.
        flattened.root = flattened
            .objects
            .len()
            .checked_sub(1)
            .ok_or(AnswerBuildError::UnboxedRoot)?;
        Ok(flattened)
    }

    /// Borrowed handles are resolved after each possible collection.
    pub(super) fn handles(&self) -> impl Iterator<Item = PreparedHandle> + '_ {
        self.handles.iter().copied()
    }

    fn visit<'a>(
        &mut self,
        plan: &AnswerPlan,
        resolve: &impl Fn(DataConId) -> Option<&'a Arc<ObjectDescriptor>>,
        depth: usize,
    ) -> Result<PlannedField, AnswerBuildError> {
        if depth > MAX_ANSWER_DEPTH {
            return Err(AnswerBuildError::TooDeep(MAX_ANSWER_DEPTH));
        }
        match plan {
            AnswerPlan::Scalar { bits, .. } => Ok(PlannedField::Scalar(*bits)),
            AnswerPlan::Bytes(data) => {
                self.byte_arrays.push(PlannedBytes { data: data.clone() });
                Ok(PlannedField::Bytes(self.byte_arrays.len() - 1))
            }
            AnswerPlan::Json(_) => Err(AnswerBuildError::UnresolvedJson),
            AnswerPlan::Handle(handle) => {
                self.handles.push(*handle);
                Ok(PlannedField::Handle(self.handles.len() - 1))
            }
            AnswerPlan::Constructor { host_id, fields } => {
                let descriptor =
                    resolve(*host_id).ok_or(AnswerBuildError::UnknownConstructor(*host_id))?;
                let expected = descriptor.payload().logical_to_stored().len();
                if fields.len() != expected {
                    return Err(AnswerBuildError::FieldCount {
                        host_id: *host_id,
                        expected,
                        actual: fields.len(),
                    });
                }
                if descriptor.allocation_alignment() as usize > WORD {
                    return Err(AnswerBuildError::Alignment {
                        host_id: *host_id,
                        required: descriptor.allocation_alignment(),
                    });
                }
                let mut planned = Vec::with_capacity(fields.len());
                for field in fields {
                    planned.push(self.visit(field, resolve, depth + 1)?);
                }
                self.objects.push(PlannedObject {
                    host_id: *host_id,
                    descriptor: Arc::clone(descriptor),
                    fields: planned,
                });
                Ok(PlannedField::Object(self.objects.len() - 1))
            }
        }
    }

    pub(super) fn root_count(&self) -> usize {
        self.byte_arrays.len() + self.objects.len()
    }

    pub(super) fn byte_count(&self) -> usize {
        self.byte_arrays.len()
    }

    pub(super) fn byte_data(&self, index: usize) -> &[u8] {
        &self.byte_arrays[index].data
    }

    pub(super) fn byte_extent(&self) -> usize {
        align_up(self.bytes_descriptor.allocation_extent() as usize)
    }

    /// Initialize one byte wrapper. The caller guarantees that no collection
    /// occurs before the returned tagged word reaches a registered root slot.
    pub(super) unsafe fn write_byte(
        &self,
        pointer: *mut u8,
        payload: *mut u8,
    ) -> Result<usize, AnswerBuildError> {
        marshal_descriptor_object(
            pointer,
            self.bytes_descriptor.allocation_extent() as usize,
            &self.bytes_descriptor,
            &[DescriptorValue::Address(payload.cast_const())],
        )
        .map_err(AnswerBuildError::Wrapper)?;
        Ok(pointer as usize | usize::from(self.bytes_descriptor.tag()))
    }

    pub(super) fn object_count(&self) -> usize {
        self.objects.len()
    }

    pub(super) fn object_extent(&self, index: usize) -> usize {
        align_up(self.objects[index].descriptor.allocation_extent() as usize)
    }

    /// Initialize one constructor from collector-updated child roots and
    /// borrowed handle words resolved after the most recent collection.
    pub(super) unsafe fn write_object(
        &self,
        index: usize,
        pointer: *mut u8,
        roots: &[u64],
        handle_words: &[usize],
    ) -> Result<usize, AnswerBuildError> {
        let object = &self.objects[index];
        let mut values = Vec::with_capacity(object.fields.len());
        for field in &object.fields {
            values.push(match field {
                PlannedField::Scalar(bits) => DescriptorValue::Bits(*bits),
                PlannedField::Object(index) => {
                    DescriptorValue::Managed(roots[self.byte_arrays.len() + *index] as *mut u8)
                }
                PlannedField::Bytes(index) => DescriptorValue::Managed(roots[*index] as *mut u8),
                PlannedField::Handle(index) => {
                    DescriptorValue::Managed(handle_words[*index] as *mut u8)
                }
            });
        }
        marshal_descriptor_object(
            pointer,
            object.descriptor.allocation_extent() as usize,
            &object.descriptor,
            &values,
        )
        .map_err(|error| AnswerBuildError::Field {
            host_id: object.host_id,
            index: match &error {
                DescriptorMarshalError::Representation { index, .. }
                | DescriptorMarshalError::ManagedReference { index } => *index,
                _ => 0,
            },
            error,
        })?;
        Ok(pointer as usize | usize::from(object.descriptor.tag()))
    }

    pub(super) fn root_slot(&self) -> usize {
        self.byte_arrays.len() + self.root
    }
}
