//! JIT debugging tools.
//!
//! Provides reusable infrastructure for debugging JIT-compiled code:
//! - **LambdaRegistry**: maps code pointers back to lambda names
//! - **heap_describe**: human-readable description of heap objects
//! - **heap_validate**: structural integrity checks for heap objects
//! - **TracingClosureCaller**: wraps closure calls with logging
//!
//! Tracing is controlled by the `TIDEPOOL_TRACE` env var:
//! - `TIDEPOOL_TRACE=calls` — log each closure call (name, arg, result)
//! - `TIDEPOOL_TRACE=heap` — also validate heap objects before use

use core_heap::layout;
use std::collections::HashMap;
use std::sync::Mutex;

// ── Lambda Registry ──────────────────────────────────────────

static LAMBDA_REGISTRY: Mutex<Option<LambdaRegistry>> = Mutex::new(None);

/// Maps JIT code pointers to human-readable lambda names.
///
/// Populated during compilation, queried during execution to identify
/// which closure is being called when debugging crashes.
#[derive(Default)]
pub struct LambdaRegistry {
    /// code_ptr → lambda name
    entries: HashMap<usize, String>,
}

impl LambdaRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a lambda's code pointer and name.
    pub fn register(&mut self, code_ptr: usize, name: String) {
        self.entries.insert(code_ptr, name);
    }

    /// Look up a lambda name by code pointer.
    pub fn lookup(&self, code_ptr: usize) -> Option<&str> {
        self.entries.get(&code_ptr).map(|s| s.as_str())
    }

    /// Number of registered lambdas.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Install a registry as the global singleton. Returns the old one if any.
pub fn set_lambda_registry(registry: LambdaRegistry) -> Option<LambdaRegistry> {
    let mut guard = LAMBDA_REGISTRY.lock().unwrap();
    guard.replace(registry)
}

/// Clear the global registry.
pub fn clear_lambda_registry() -> Option<LambdaRegistry> {
    LAMBDA_REGISTRY.lock().unwrap().take()
}

/// Look up a code pointer in the global registry.
pub fn lookup_lambda(code_ptr: usize) -> Option<String> {
    let guard = LAMBDA_REGISTRY.lock().unwrap();
    guard
        .as_ref()
        .and_then(|r| r.lookup(code_ptr))
        .map(|s| s.to_string())
}

// ── Heap Object Inspection ───────────────────────────────────

/// Describes a heap object in human-readable form.
///
/// Returns a string like:
/// - `Lit(Int#, 42)`
/// - `Con(tag=12345, 2 fields)`
/// - `Closure(code=0x..., 3 captures) [repl_lambda_5]`
/// - `INVALID(tag=255, ptr=0x...)`
///
/// # Safety
///
/// `ptr` must point to a valid heap object, or at least readable memory.
pub unsafe fn heap_describe(ptr: *const u8) -> String {
    if ptr.is_null() {
        return "NULL".to_string();
    }

    let tag_byte = *ptr.add(layout::OFFSET_TAG);
    let size = std::ptr::read_unaligned(ptr.add(layout::OFFSET_SIZE) as *const u16);

    match layout::HeapTag::from_byte(tag_byte) {
        Some(layout::HeapTag::Lit) => {
            let lit_tag = *ptr.add(layout::LIT_TAG_OFFSET);
            let value = *(ptr.add(layout::LIT_VALUE_OFFSET) as *const i64);
            let tag_name = layout::LitTag::from_byte(lit_tag)
                .map(|t| t.to_string())
                .unwrap_or_else(|| format!("?{}", lit_tag));
            format!("Lit({}, {})", tag_name, value)
        }
        Some(layout::HeapTag::Con) => {
            let con_tag = *(ptr.add(layout::CON_TAG_OFFSET) as *const u64);
            let num_fields = *(ptr.add(layout::CON_NUM_FIELDS_OFFSET) as *const u16);
            format!("Con(tag={}, {} fields, size={})", con_tag, num_fields, size)
        }
        Some(layout::HeapTag::Closure) => {
            let code_ptr = *(ptr.add(layout::CLOSURE_CODE_PTR_OFFSET) as *const usize);
            let num_captured = *(ptr.add(layout::CLOSURE_NUM_CAPTURED_OFFSET) as *const u16);
            let name = lookup_lambda(code_ptr);
            let name_str = name
                .as_deref()
                .map(|n| format!(" [{}]", n))
                .unwrap_or_default();
            format!(
                "Closure(code=0x{:x}, {} captures, size={}){}",
                code_ptr, num_captured, size, name_str
            )
        }
        Some(layout::HeapTag::Thunk) => {
            let state = *ptr.add(layout::THUNK_STATE_OFFSET);
            format!("Thunk(state={}, size={})", state, size)
        }
        None => {
            format!("INVALID(tag={}, size={}, ptr={:?})", tag_byte, size, ptr)
        }
    }
}

// ── Heap Object Validation ───────────────────────────────────

/// Validation errors for heap objects.
#[derive(Debug)]
pub enum HeapError {
    NullPointer,
    InvalidTag(u8),
    ZeroSize,
    /// Closure has null code pointer
    NullCodePtr,
    /// Size field doesn't match expected size for the object type
    SizeMismatch { expected_min: u16, actual: u16 },
    /// A field pointer is null
    NullField { index: usize },
    /// A field pointer has an invalid heap tag
    InvalidFieldTag { index: usize, tag: u8 },
}

impl std::fmt::Display for HeapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HeapError::NullPointer => write!(f, "null pointer"),
            HeapError::InvalidTag(t) => write!(f, "invalid heap tag: {}", t),
            HeapError::ZeroSize => write!(f, "zero size"),
            HeapError::NullCodePtr => write!(f, "null code pointer in closure"),
            HeapError::SizeMismatch { expected_min, actual } => {
                write!(f, "size mismatch: expected >= {}, got {}", expected_min, actual)
            }
            HeapError::NullField { index } => write!(f, "null pointer in field {}", index),
            HeapError::InvalidFieldTag { index, tag } => {
                write!(f, "field {} has invalid tag: {}", index, tag)
            }
        }
    }
}

/// Validate a heap object's structural integrity.
///
/// Checks:
/// - Non-null pointer
/// - Valid tag byte
/// - Non-zero size
/// - Size consistent with field count
/// - Closure code_ptr is non-null
///
/// Does NOT follow field pointers (use `heap_validate_deep` for that).
///
/// # Safety
///
/// `ptr` must point to readable memory of at least `size` bytes.
pub unsafe fn heap_validate(ptr: *const u8) -> Result<(), HeapError> {
    if ptr.is_null() {
        return Err(HeapError::NullPointer);
    }

    let tag_byte = *ptr.add(layout::OFFSET_TAG);
    let size = std::ptr::read_unaligned(ptr.add(layout::OFFSET_SIZE) as *const u16);

    if size == 0 {
        return Err(HeapError::ZeroSize);
    }

    match layout::HeapTag::from_byte(tag_byte) {
        None => return Err(HeapError::InvalidTag(tag_byte)),
        Some(layout::HeapTag::Closure) => {
            let code_ptr = *(ptr.add(layout::CLOSURE_CODE_PTR_OFFSET) as *const usize);
            if code_ptr == 0 {
                return Err(HeapError::NullCodePtr);
            }
            let num_captured = *(ptr.add(layout::CLOSURE_NUM_CAPTURED_OFFSET) as *const u16);
            let expected_min = (24 + 8 * num_captured as usize) as u16;
            if size < expected_min {
                return Err(HeapError::SizeMismatch {
                    expected_min,
                    actual: size,
                });
            }
        }
        Some(layout::HeapTag::Con) => {
            let num_fields = *(ptr.add(layout::CON_NUM_FIELDS_OFFSET) as *const u16);
            let expected_min = (24 + 8 * num_fields as usize) as u16;
            if size < expected_min {
                return Err(HeapError::SizeMismatch {
                    expected_min,
                    actual: size,
                });
            }
        }
        Some(layout::HeapTag::Lit) => {
            if size < layout::LIT_SIZE as u16 {
                return Err(HeapError::SizeMismatch {
                    expected_min: layout::LIT_SIZE as u16,
                    actual: size,
                });
            }
        }
        Some(layout::HeapTag::Thunk) => {
            // Thunks are at least header + state + code_ptr
            if size < 24 {
                return Err(HeapError::SizeMismatch {
                    expected_min: 24,
                    actual: size,
                });
            }
        }
    }

    Ok(())
}

/// Validate a heap object and all its pointer fields (one level deep).
///
/// # Safety
///
/// All pointers must be readable.
pub unsafe fn heap_validate_deep(ptr: *const u8) -> Result<(), HeapError> {
    heap_validate(ptr)?;

    let tag_byte = *ptr.add(layout::OFFSET_TAG);
    match layout::HeapTag::from_byte(tag_byte) {
        Some(layout::HeapTag::Con) => {
            let num_fields = *(ptr.add(layout::CON_NUM_FIELDS_OFFSET) as *const u16);
            for i in 0..num_fields as usize {
                let field =
                    *(ptr.add(layout::CON_FIELDS_OFFSET + 8 * i) as *const *const u8);
                if field.is_null() {
                    return Err(HeapError::NullField { index: i });
                }
                let field_tag = *field.add(layout::OFFSET_TAG);
                if layout::HeapTag::from_byte(field_tag).is_none() {
                    return Err(HeapError::InvalidFieldTag {
                        index: i,
                        tag: field_tag,
                    });
                }
            }
        }
        Some(layout::HeapTag::Closure) => {
            let num_captured = *(ptr.add(layout::CLOSURE_NUM_CAPTURED_OFFSET) as *const u16);
            for i in 0..num_captured as usize {
                let cap =
                    *(ptr.add(layout::CLOSURE_CAPTURED_OFFSET + 8 * i) as *const *const u8);
                if cap.is_null() {
                    return Err(HeapError::NullField { index: i });
                }
                let cap_tag = *cap.add(layout::OFFSET_TAG);
                if layout::HeapTag::from_byte(cap_tag).is_none() {
                    return Err(HeapError::InvalidFieldTag {
                        index: i,
                        tag: cap_tag,
                    });
                }
            }
        }
        _ => {}
    }

    Ok(())
}

// ── Trace Level ──────────────────────────────────────────────

/// Trace level, controlled by `TIDEPOOL_TRACE` env var.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TraceLevel {
    Off,
    Calls,
    Heap,
}

/// Read the trace level from the environment. Cached after first call.
pub fn trace_level() -> TraceLevel {
    use std::sync::OnceLock;
    static LEVEL: OnceLock<TraceLevel> = OnceLock::new();
    *LEVEL.get_or_init(|| match std::env::var("TIDEPOOL_TRACE").as_deref() {
        Ok("calls") => TraceLevel::Calls,
        Ok("heap") => TraceLevel::Heap,
        _ => TraceLevel::Off,
    })
}
