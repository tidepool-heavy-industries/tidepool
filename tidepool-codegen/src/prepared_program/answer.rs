//! Host-built answers for parked prepared continuations.
//!
//! The session validates a bridge `Value` against a parked frame's site
//! evidence and lowers it to an [`AnswerPlan`]: constructors named by their
//! bridge `DataConId` and scalars already target-encoded. The machine resolves
//! every constructor through its interner (`by_host`), sizes the whole tree,
//! makes room once, writes the objects bottom-up into the reserved nursery
//! span without any allocating call, and only then advances the allocation
//! cursor and publishes the root as a realm-owned handle. A failure at any
//! point leaves the cursor where it was: nothing built becomes reachable.
//!
//! A byte-backed leaf (`ByteArray#`: the backing of `Text` and of a
//! multi-limb `Integer`/`Natural`) is one external payload plus its managed
//! wrapper object. The payloads are allocated in the machine ledger before any
//! object is written and revoked again if anything later fails, so a refused
//! answer leaves the ledger as it found it. Handle delivery is a later slice.

use std::sync::Arc;

use tidepool_heap::execution_descriptor::ObjectDescriptor;
use tidepool_heap::external_storage::ExternalStorageValidationError;
use tidepool_repr::execution_schema::RuntimeRep;
use tidepool_repr::DataConId;

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
}

/// Why a plan could not be built. Every variant leaves the heap untouched.
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
    /// Byte offset of this object from the start of the reserved span.
    offset: usize,
}

enum PlannedField {
    /// Index into the flattened object list.
    Object(usize),
    Scalar([u8; 16]),
    /// Index into the flattened byte-array list.
    Bytes(usize),
}

/// One `ByteArray#` wrapper of a flattened plan: the wrapper object lives in
/// the reserved span at `offset`; its payload is allocated in the ledger
/// separately and handed to [`FlattenedAnswer::write`].
struct PlannedBytes {
    data: Vec<u8>,
    offset: usize,
}

/// A plan flattened against the machine's descriptors: every constructor
/// resolved, every field count checked, every object given its offset.
pub(super) struct FlattenedAnswer {
    objects: Vec<PlannedObject>,
    byte_arrays: Vec<PlannedBytes>,
    /// The external `Bytes` wrapper descriptor every byte array is written
    /// with, from the program the answer is built for.
    bytes_descriptor: Arc<ObjectDescriptor>,
    /// Total bytes the build writes, alignment padding included.
    pub(super) extent: usize,
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
        let mut flattened = Self {
            objects: Vec::new(),
            byte_arrays: Vec::new(),
            bytes_descriptor: Arc::clone(bytes_descriptor),
            extent: 0,
        };
        flattened.visit(plan, resolve, 0)?;
        Ok(flattened)
    }

    /// The byte arrays the build needs payloads for, in the order
    /// [`Self::write`] expects them.
    pub(super) fn byte_arrays(&self) -> impl Iterator<Item = &[u8]> + '_ {
        self.byte_arrays.iter().map(|bytes| bytes.data.as_slice())
    }

    /// Lay one object of `descriptor` out at the next word-aligned offset.
    fn place(&mut self, descriptor: &ObjectDescriptor) -> Result<usize, AnswerBuildError> {
        let offset = align_up(self.extent);
        self.extent = offset
            .checked_add(align_up(descriptor.allocation_extent() as usize))
            .ok_or(AnswerBuildError::TooLarge(usize::MAX))?;
        Ok(offset)
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
                let descriptor = Arc::clone(&self.bytes_descriptor);
                let offset = self.place(&descriptor)?;
                self.byte_arrays.push(PlannedBytes {
                    data: data.clone(),
                    offset,
                });
                Ok(PlannedField::Bytes(self.byte_arrays.len() - 1))
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
                let offset = self.place(descriptor)?;
                self.objects.push(PlannedObject {
                    host_id: *host_id,
                    descriptor: Arc::clone(descriptor),
                    fields: planned,
                    offset,
                });
                Ok(PlannedField::Object(self.objects.len() - 1))
            }
        }
    }

    /// Write every object into `span` (at least `self.extent` writable bytes
    /// that nothing else reaches), byte-array wrappers first, then
    /// constructors children before parents, and return the tagged root
    /// reference. `payloads` are the published ledger identities for
    /// [`Self::byte_arrays`], in order, already holding their bytes. A failure
    /// leaves `span` unpublished garbage; the caller revokes the payloads.
    ///
    /// # Safety
    /// `span` must name `self.extent` writable bytes inside the live nursery,
    /// beyond the allocation cursor, so no collection or generated code can
    /// observe them until the caller advances the cursor.
    pub(super) unsafe fn write(
        &self,
        span: *mut u8,
        payloads: &[*mut u8],
    ) -> Result<usize, AnswerBuildError> {
        debug_assert_eq!(payloads.len(), self.byte_arrays.len());
        let tagged = |pointer: *mut u8, descriptor: &ObjectDescriptor| {
            // Reference words carry the descriptor's tag, as generated code
            // tags every reference it constructs.
            pointer as usize | usize::from(descriptor.tag())
        };
        let mut wrappers = Vec::with_capacity(self.byte_arrays.len());
        for (bytes, payload) in self.byte_arrays.iter().zip(payloads) {
            let pointer = span.add(bytes.offset);
            marshal_descriptor_object(
                pointer,
                self.bytes_descriptor.allocation_extent() as usize,
                &self.bytes_descriptor,
                &[DescriptorValue::Address(payload.cast_const())],
            )
            .map_err(AnswerBuildError::Wrapper)?;
            wrappers.push(tagged(pointer, &self.bytes_descriptor));
        }
        let mut words = Vec::with_capacity(self.objects.len());
        for object in &self.objects {
            let pointer = span.add(object.offset);
            let mut values = Vec::with_capacity(object.fields.len());
            for field in &object.fields {
                values.push(match field {
                    PlannedField::Scalar(bits) => DescriptorValue::Bits(*bits),
                    PlannedField::Object(index) => {
                        DescriptorValue::Managed(words[*index] as *mut u8)
                    }
                    PlannedField::Bytes(index) => {
                        DescriptorValue::Managed(wrappers[*index] as *mut u8)
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
            words.push(tagged(pointer, &object.descriptor));
        }
        words.last().copied().ok_or(AnswerBuildError::TooLarge(0))
    }
}
