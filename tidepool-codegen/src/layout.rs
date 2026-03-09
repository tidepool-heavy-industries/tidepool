//! Canonical VMContext and HeapObject layout constants for codegen.
//!
//! These constants define the frozen layout of the VMContext struct and
//! the various heap object types as `i32`/`i64` values suitable for
//! Cranelift IR emission. `tidepool_heap::layout` defines the same
//! layout using native Rust types for runtime use — the two modules
//! must stay in sync.

// --- VMContext field offsets (i32 for Cranelift) ---

/// Offset of alloc_ptr within VMContext.
pub const VMCTX_ALLOC_PTR_OFFSET: i32 = 0;
/// Offset of alloc_limit within VMContext.
pub const VMCTX_ALLOC_LIMIT_OFFSET: i32 = 8;
/// Offset of gc_trigger within VMContext.
pub const VMCTX_GC_TRIGGER_OFFSET: i32 = 16;
/// Offset of tail_callee within VMContext.
pub const VMCTX_TAIL_CALLEE_OFFSET: i32 = 24;
/// Offset of tail_arg within VMContext.
pub const VMCTX_TAIL_ARG_OFFSET: i32 = 32;

// --- Heap object tags (u8) ---

pub const TAG_CLOSURE: u8 = 0;
pub const TAG_THUNK: u8 = 1;
pub const TAG_CON: u8 = 2;
pub const TAG_LIT: u8 = 3;
pub const TAG_FORWARDED: u8 = 0xFF;

// --- Thunk state tags (u8) ---

pub const THUNK_UNEVALUATED: u8 = 0;
pub const THUNK_BLACKHOLE: u8 = 1;
pub const THUNK_EVALUATED: u8 = 2;

// --- HeapObject layout constants (i32/u64 for Cranelift and Rust) ---

pub const HEAP_HEADER_SIZE: u64 = 8;

// Closure layout
pub const CLOSURE_CODE_PTR_OFFSET: i32 = 8;
pub const CLOSURE_NUM_CAPTURED_OFFSET: i32 = 16;
pub const CLOSURE_CAPTURED_OFFSET: i32 = 24;

// Con layout
pub const CON_TAG_OFFSET: i32 = 8;
pub const CON_NUM_FIELDS_OFFSET: i32 = 16;
pub const CON_FIELDS_OFFSET: i32 = 24;

// Lit layout
pub const LIT_TAG_OFFSET: i32 = 8;
pub const LIT_VALUE_OFFSET: i32 = 16;
pub const LIT_TOTAL_SIZE: u64 = 24;

// Lit tags (i64 for builder.ins().iconst)
pub const LIT_TAG_INT: i64 = 0;
pub const LIT_TAG_WORD: i64 = 1;
pub const LIT_TAG_CHAR: i64 = 2;
pub const LIT_TAG_FLOAT: i64 = 3;
pub const LIT_TAG_DOUBLE: i64 = 4;
pub const LIT_TAG_STRING: i64 = 5;
pub const LIT_TAG_ADDR: i64 = 6;
pub const LIT_TAG_BYTEARRAY: i64 = 7;
pub const LIT_TAG_SMALLARRAY: i64 = 8;
pub const LIT_TAG_ARRAY: i64 = 9;

// Thunk layout
pub const THUNK_STATE_OFFSET: i32 = 8;
pub const THUNK_CODE_PTR_OFFSET: i32 = 16;
pub const THUNK_CAPTURED_OFFSET: i32 = 24;
pub const THUNK_MIN_SIZE: u64 = 24;
pub const THUNK_INDIRECTION_OFFSET: i32 = 16;
