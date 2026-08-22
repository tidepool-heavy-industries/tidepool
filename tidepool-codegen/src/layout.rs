//! Canonical VMContext and HeapObject layout constants for codegen.
//!
//! These constants define the frozen layout of the VMContext struct and
//! the various heap object types as `i32`/`i64` values suitable for
//! Cranelift IR emission. The heap-object *tag* discriminants (`TAG_*`,
//! `THUNK_*`, and `LIT_TAG_*`) AND the payload field offsets/sizes
//! (`CLOSURE_*`/`CON_*`/`LIT_*`/`THUNK_*`/`HEAP_HEADER_SIZE`) are NOT
//! redefined here — they are DERIVED from (or re-exported from)
//! `tidepool_heap::layout`, which is the single source of truth for the
//! numeric ABI. This makes drift between the heap runtime and codegen a
//! compile error rather than a silent divergence. All derivations are
//! compile-time `as` casts / re-exports — zero runtime cost.

/// The literal-value discriminant, re-exported from the heap crate so codegen
/// and the runtime share one definition. `SsaVal::Raw` carries a `LitTag`
/// (not a bare `i64`), so an unboxed value's tag can only be a real literal
/// kind. Cast to `i64` with `as i64` at Cranelift `iconst`/`icmp_imm` sites.
pub use tidepool_heap::layout::LitTag;

// --- VMContext field offsets (i32 for Cranelift) ---

pub const VMCTX_ALLOC_PTR_OFFSET: i32 = 0;
pub const VMCTX_ALLOC_LIMIT_OFFSET: i32 = 8;
pub const VMCTX_GC_TRIGGER_OFFSET: i32 = 16;
pub const VMCTX_TAIL_CALLEE_OFFSET: i32 = 24;
pub const VMCTX_TAIL_ARG_OFFSET: i32 = 32;

// --- Heap object tags (u8), derived from tidepool_heap::layout::HeapTag ---

pub const TAG_CLOSURE: u8 = tidepool_heap::layout::HeapTag::Closure as u8;
pub const TAG_THUNK: u8 = tidepool_heap::layout::HeapTag::Thunk as u8;
pub const TAG_CON: u8 = tidepool_heap::layout::HeapTag::Con as u8;
pub const TAG_LIT: u8 = tidepool_heap::layout::HeapTag::Lit as u8;
pub const TAG_FORWARDED: u8 = tidepool_heap::layout::TAG_FORWARDED;

// --- Thunk state tags (u8), derived from ThunkStateTag ---

pub const THUNK_UNEVALUATED: u8 = tidepool_heap::layout::ThunkStateTag::Unevaluated as u8;
pub const THUNK_BLACKHOLE: u8 = tidepool_heap::layout::ThunkStateTag::BlackHole as u8;
pub const THUNK_EVALUATED: u8 = tidepool_heap::layout::ThunkStateTag::Evaluated as u8;

// --- HeapObject layout constants (i32/u64 for Cranelift and Rust), derived
// from tidepool_heap::layout (usize) so an offset/size can never drift from
// the heap runtime that actually owns the ABI. ---

pub const HEAP_HEADER_SIZE: u64 = tidepool_heap::layout::HEADER_SIZE as u64;

// Closure layout
pub const CLOSURE_CODE_PTR_OFFSET: i32 = tidepool_heap::layout::CLOSURE_CODE_PTR_OFFSET as i32;
pub const CLOSURE_NUM_CAPTURED_OFFSET: i32 =
    tidepool_heap::layout::CLOSURE_NUM_CAPTURED_OFFSET as i32;
pub const CLOSURE_CAPTURED_OFFSET: i32 = tidepool_heap::layout::CLOSURE_CAPTURED_OFFSET as i32;

// Con layout
pub const CON_TAG_OFFSET: i32 = tidepool_heap::layout::CON_TAG_OFFSET as i32;
pub const CON_NUM_FIELDS_OFFSET: i32 = tidepool_heap::layout::CON_NUM_FIELDS_OFFSET as i32;
pub const CON_FIELDS_OFFSET: i32 = tidepool_heap::layout::CON_FIELDS_OFFSET as i32;

// Lit layout
pub const LIT_TAG_OFFSET: i32 = tidepool_heap::layout::LIT_TAG_OFFSET as i32;
pub const LIT_VALUE_OFFSET: i32 = tidepool_heap::layout::LIT_VALUE_OFFSET as i32;
pub const LIT_TOTAL_SIZE: u64 = tidepool_heap::layout::LIT_SIZE as u64;

// Lit tags, re-exported as `LitTag` values from tidepool_heap::layout (the
// single ABI source). `SsaVal::Raw` stores one directly; at Cranelift
// `iconst`/`icmp_imm` sites write `LIT_TAG_INT as i64`.
pub const LIT_TAG_INT: LitTag = LitTag::Int;
pub const LIT_TAG_WORD: LitTag = LitTag::Word;
pub const LIT_TAG_CHAR: LitTag = LitTag::Char;
pub const LIT_TAG_FLOAT: LitTag = LitTag::Float;
pub const LIT_TAG_DOUBLE: LitTag = LitTag::Double;
pub const LIT_TAG_STRING: LitTag = LitTag::String;
pub const LIT_TAG_ADDR: LitTag = LitTag::Addr;
pub const LIT_TAG_BYTEARRAY: LitTag = LitTag::ByteArray;
pub const LIT_TAG_SMALLARRAY: LitTag = LitTag::SmallArray;
pub const LIT_TAG_ARRAY: LitTag = LitTag::Array;

// Thunk layout
pub const THUNK_STATE_OFFSET: i32 = tidepool_heap::layout::THUNK_STATE_OFFSET as i32;
pub const THUNK_CODE_PTR_OFFSET: i32 = tidepool_heap::layout::THUNK_CODE_PTR_OFFSET as i32;
pub const THUNK_CAPTURED_OFFSET: i32 = tidepool_heap::layout::THUNK_CAPTURED_OFFSET as i32;
pub const THUNK_MIN_SIZE: u64 = tidepool_heap::layout::THUNK_MIN_SIZE as u64;
pub const THUNK_INDIRECTION_OFFSET: i32 = tidepool_heap::layout::THUNK_INDIRECTION_OFFSET as i32;
