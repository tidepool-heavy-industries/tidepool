//! JIT debugging tools.
//!
//! Provides reusable infrastructure for debugging JIT-compiled code:
//! - **LambdaRegistry**: maps code pointers back to lambda names
//! - **heap_describe**: human-readable description of heap objects
//! - **heap_validate**: structural integrity checks for heap objects
//! - **TracingClosureCaller**: wraps closure calls with logging
//!
//! Diagnostic tracing is routed through the `log` crate with per-subsystem
//! targets and controlled by `RUST_LOG` (standard) or the legacy
//! `TIDEPOOL_TRACE` env var (back-compat). See [`init_logging`].
//!
//! Targets:
//! - `tidepool::calls` (trace) — each closure call (name, arg, result)
//! - `tidepool::scope` (trace) — emit-time scope/env bookkeeping
//! - `tidepool::heap`  (trace) — heap-object validation before use
//! - `tidepool::effects` (debug) — effect dispatch at the JIT↔Rust boundary
//! - `tidepool::fp` (debug) — runtime cache binary-fingerprint memo
//!
//! Legacy mapping (honored by [`init_logging`] for back-compat):
//! - `TIDEPOOL_TRACE=calls` → `tidepool::calls=trace`
//! - `TIDEPOOL_TRACE=scope` → `tidepool::calls=trace,tidepool::scope=trace`
//! - `TIDEPOOL_TRACE=heap`  → calls+scope+heap at trace (preserves the old
//!   `heap >= scope >= calls` ordering)
//! - `TIDEPOOL_TRACE_EFFECTS=1` → `tidepool::effects=debug`
//! - `TIDEPOOL_FP_DEBUG=1` → `tidepool::fp=debug`

use crate::layout;
use std::cell::RefCell;
use std::collections::HashMap;
use tidepool_heap::layout as heap_layout;

// ── Lambda Registry ──────────────────────────────────────────

thread_local! {
    static LAMBDA_REGISTRY: RefCell<Option<LambdaRegistry>> = const { RefCell::new(None) };
}

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

    /// Look up a lambda name by an address within its body.
    /// Finds the entry point <= addr that is closest to addr.
    pub fn lookup_by_address(&self, addr: usize) -> Option<&str> {
        let mut best: Option<(usize, &str)> = None;
        for (&ptr, name) in &self.entries {
            if ptr <= addr {
                if let Some((best_ptr, _)) = best {
                    if ptr > best_ptr {
                        best = Some((ptr, name.as_str()));
                    }
                } else {
                    best = Some((ptr, name.as_str()));
                }
            }
        }
        best.map(|(_, name)| name)
    }

    /// Number of registered lambdas.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Install a registry as the thread-local singleton. Returns the old one if any.
pub fn set_lambda_registry(registry: LambdaRegistry) -> Option<LambdaRegistry> {
    LAMBDA_REGISTRY.with(|cell| cell.borrow_mut().replace(registry))
}

/// Clear the thread-local registry.
pub fn clear_lambda_registry() -> Option<LambdaRegistry> {
    LAMBDA_REGISTRY.with(|cell| cell.borrow_mut().take())
}

/// Look up a code pointer in the thread-local registry.
pub fn lookup_lambda(code_ptr: usize) -> Option<String> {
    LAMBDA_REGISTRY.with(|cell| {
        cell.borrow()
            .as_ref()
            .and_then(|r| r.lookup(code_ptr))
            .map(|s| s.to_string())
    })
}

/// Look up a lambda name by an address within its body in the thread-local registry.
pub fn lookup_lambda_by_address(addr: usize) -> Option<String> {
    LAMBDA_REGISTRY.with(|cell| {
        cell.borrow()
            .as_ref()
            .and_then(|r| r.lookup_by_address(addr))
            .map(|s| s.to_string())
    })
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
    // SAFETY: Caller guarantees ptr points to a valid heap object or readable memory.
    if ptr.is_null() {
        return "NULL".to_string();
    }

    let tag_byte = *ptr.add(heap_layout::OFFSET_TAG);
    let size = std::ptr::read_unaligned(ptr.add(heap_layout::OFFSET_SIZE) as *const u16);

    match heap_layout::HeapTag::from_byte(tag_byte) {
        Some(heap_layout::HeapTag::Lit) => {
            let lit_tag = *ptr.add(layout::LIT_TAG_OFFSET as usize);
            let value = *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *const i64);
            let tag_name = heap_layout::LitTag::from_byte(lit_tag)
                .map(|t| t.to_string())
                .unwrap_or_else(|| format!("?{}", lit_tag));
            format!("Lit({}, {})", tag_name, value)
        }
        Some(heap_layout::HeapTag::Con) => {
            let con_tag = *(ptr.add(layout::CON_TAG_OFFSET as usize) as *const u64);
            let num_fields = *(ptr.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *const u16);
            format!("Con(tag={}, {} fields, size={})", con_tag, num_fields, size)
        }
        Some(heap_layout::HeapTag::Closure) => {
            let code_ptr = *(ptr.add(layout::CLOSURE_CODE_PTR_OFFSET as usize) as *const usize);
            let num_captured =
                *(ptr.add(layout::CLOSURE_NUM_CAPTURED_OFFSET as usize) as *const u16);
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
        Some(heap_layout::HeapTag::Thunk) => {
            let state = *ptr.add(layout::THUNK_STATE_OFFSET as usize);
            format!("Thunk(state={}, size={})", state, size)
        }
        None => {
            format!("INVALID(tag={}, size={}, ptr={:?})", tag_byte, size, ptr)
        }
    }
}

// ── Heap Object Validation ───────────────────────────────────

/// Validation errors for heap objects.
#[derive(Debug, thiserror::Error)]
pub enum HeapError {
    #[error("null pointer")]
    NullPointer,
    #[error("invalid heap tag: {0}")]
    InvalidTag(u8),
    #[error("zero size")]
    ZeroSize,
    /// Closure has null code pointer
    #[error("null code pointer in closure")]
    NullCodePtr,
    /// Size field doesn't match expected size for the object type
    #[error("size mismatch: expected >= {expected_min}, got {actual}")]
    SizeMismatch { expected_min: u16, actual: u16 },
    /// A field pointer is null
    #[error("null pointer in field {index}")]
    NullField { index: usize },
    /// A field pointer has an invalid heap tag
    #[error("field {index} has invalid tag: {tag}")]
    InvalidFieldTag { index: usize, tag: u8 },
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
    // SAFETY: Caller guarantees ptr points to readable memory of at least `size` bytes.
    if ptr.is_null() {
        return Err(HeapError::NullPointer);
    }

    let tag_byte = *ptr.add(heap_layout::OFFSET_TAG);
    let size = std::ptr::read_unaligned(ptr.add(heap_layout::OFFSET_SIZE) as *const u16);

    if size == 0 {
        return Err(HeapError::ZeroSize);
    }

    match heap_layout::HeapTag::from_byte(tag_byte) {
        None => return Err(HeapError::InvalidTag(tag_byte)),
        Some(heap_layout::HeapTag::Closure) => {
            let code_ptr = *(ptr.add(layout::CLOSURE_CODE_PTR_OFFSET as usize) as *const usize);
            if code_ptr == 0 {
                return Err(HeapError::NullCodePtr);
            }
            let num_captured =
                *(ptr.add(layout::CLOSURE_NUM_CAPTURED_OFFSET as usize) as *const u16);
            let expected_min =
                (layout::CLOSURE_CAPTURED_OFFSET as usize + 8 * num_captured as usize) as u16;
            if size < expected_min {
                return Err(HeapError::SizeMismatch {
                    expected_min,
                    actual: size,
                });
            }
        }
        Some(heap_layout::HeapTag::Con) => {
            let num_fields = *(ptr.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *const u16);
            let expected_min =
                (layout::CON_FIELDS_OFFSET as usize + 8 * num_fields as usize) as u16;
            if size < expected_min {
                return Err(HeapError::SizeMismatch {
                    expected_min,
                    actual: size,
                });
            }
        }
        Some(heap_layout::HeapTag::Lit) => {
            if size < layout::LIT_TOTAL_SIZE as u16 {
                return Err(HeapError::SizeMismatch {
                    expected_min: layout::LIT_TOTAL_SIZE as u16,
                    actual: size,
                });
            }
        }
        Some(heap_layout::HeapTag::Thunk) => {
            // Thunks are at least header + state + code_ptr
            if size < layout::THUNK_MIN_SIZE as u16 {
                return Err(HeapError::SizeMismatch {
                    expected_min: layout::THUNK_MIN_SIZE as u16,
                    actual: size,
                });
            }
        }
    }

    Ok(())
}

/// A closure caller that validates both closure and argument before each call.
pub struct TracingClosureCaller {
    pub vmctx: *mut crate::context::VMContext,
}

impl TracingClosureCaller {
    /// # Safety
    /// Caller must ensure callee and arg are valid heap object pointers.
    pub unsafe fn call(&self, callee: *mut u8, arg: *mut u8) -> Result<*mut u8, String> {
        // SAFETY: callee and arg must point to valid HeapObjects.
        // Validation is gated on the `tidepool::heap` log target.
        if log::log_enabled!(target: "tidepool::heap", log::Level::Trace) {
            heap_validate(callee).map_err(|e| format!("Closure validation failed: {}", e))?;
            heap_validate(arg).map_err(|e| format!("Arg validation failed: {}", e))?;
        }

        let tag_byte = *callee.add(heap_layout::OFFSET_TAG);
        if tag_byte != layout::TAG_CLOSURE {
            return Err(format!("Not a closure: tag={}", tag_byte));
        }

        let code_ptr = *(callee.add(layout::CLOSURE_CODE_PTR_OFFSET as usize) as *const usize);
        let num_captured =
            *(callee.add(layout::CLOSURE_NUM_CAPTURED_OFFSET as usize) as *const u16);
        let name = lookup_lambda(code_ptr);

        log::trace!(
            target: "tidepool::calls",
            "CALL {} callee={:?} arg={:?} ({} captures)",
            name.as_deref().unwrap_or("unknown"),
            callee,
            arg,
            num_captured
        );

        // Call the closure
        let func: unsafe extern "C" fn(
            *mut crate::context::VMContext,
            *mut u8,
            *mut u8,
        ) -> *mut u8 = std::mem::transmute(code_ptr);
        let result = func(self.vmctx, callee, arg);

        log::trace!(
            target: "tidepool::calls",
            "RET  {} result={:?}",
            name.as_deref().unwrap_or("unknown"),
            result
        );

        if !result.is_null() && log::log_enabled!(target: "tidepool::heap", log::Level::Trace) {
            heap_validate(result).map_err(|e| format!("Result validation failed: {}", e))?;
        }

        Ok(result)
    }
}

/// Validate a heap object and all its pointer fields (one level deep).
///
/// # Safety
///
/// All pointers must be readable.
pub unsafe fn heap_validate_deep(ptr: *const u8) -> Result<(), HeapError> {
    // SAFETY: Caller guarantees ptr and all reachable field pointers point to readable memory.
    heap_validate(ptr)?;

    let tag_byte = *ptr.add(heap_layout::OFFSET_TAG);
    match heap_layout::HeapTag::from_byte(tag_byte) {
        Some(heap_layout::HeapTag::Con) => {
            let num_fields = *(ptr.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *const u16);
            for i in 0..num_fields as usize {
                let field =
                    *(ptr.add(layout::CON_FIELDS_OFFSET as usize + 8 * i) as *const *const u8);
                if field.is_null() {
                    continue;
                }
                let field_tag = *field.add(heap_layout::OFFSET_TAG);
                if heap_layout::HeapTag::from_byte(field_tag).is_none() {
                    return Err(HeapError::InvalidFieldTag {
                        index: i,
                        tag: field_tag,
                    });
                }
            }
        }
        Some(heap_layout::HeapTag::Closure) => {
            let num_captured =
                *(ptr.add(layout::CLOSURE_NUM_CAPTURED_OFFSET as usize) as *const u16);
            for i in 0..num_captured as usize {
                let cap = *(ptr.add(layout::CLOSURE_CAPTURED_OFFSET as usize + 8 * i)
                    as *const *const u8);
                if cap.is_null() {
                    continue;
                }
                let cap_tag = *cap.add(heap_layout::OFFSET_TAG);
                if heap_layout::HeapTag::from_byte(cap_tag).is_none() {
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

// ── Logging Init ─────────────────────────────────────────────

/// Initialize diagnostic logging for the JIT subsystems.
///
/// Routes the per-subsystem `tidepool::*` log targets to stderr via
/// `env_logger`. Idempotent and safe to call multiple times (uses `try_init`),
/// so tests, library entry points, and the MCP server binary may all call it.
///
/// Resolution order for the filter:
/// 1. `RUST_LOG` (standard `env_logger` syntax) — primary mechanism.
/// 2. Legacy `TIDEPOOL_TRACE` / `TIDEPOOL_TRACE_EFFECTS` / `TIDEPOOL_FP_DEBUG`
///    env vars, mapped to the equivalent `tidepool::*` targets for back-compat.
///
/// If `RUST_LOG` is set it takes precedence (and the legacy vars are appended
/// after it, so they still add their targets unless `RUST_LOG` already names
/// them). If neither is set, nothing is logged.
pub fn init_logging() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let mut directives: Vec<String> = Vec::new();

        // RUST_LOG first (highest precedence in env_logger's last-wins parser
        // is actually first-match-wins per target, so put explicit user
        // directives ahead of legacy-derived ones).
        if let Ok(rust_log) = std::env::var("RUST_LOG") {
            if !rust_log.is_empty() {
                directives.push(rust_log);
            }
        }

        // Legacy TIDEPOOL_TRACE — preserve old `heap >= scope >= calls`
        // ordering: a higher level enables all lower targets.
        match std::env::var("TIDEPOOL_TRACE").as_deref() {
            Ok("calls") => {
                directives.push("tidepool::calls=trace".into());
            }
            Ok("scope") => {
                directives.push("tidepool::calls=trace".into());
                directives.push("tidepool::scope=trace".into());
            }
            Ok("heap") => {
                directives.push("tidepool::calls=trace".into());
                directives.push("tidepool::scope=trace".into());
                directives.push("tidepool::heap=trace".into());
            }
            _ => {}
        }

        if std::env::var("TIDEPOOL_TRACE_EFFECTS").is_ok() {
            directives.push("tidepool::effects=debug".into());
        }
        if std::env::var("TIDEPOOL_FP_DEBUG").is_ok() {
            directives.push("tidepool::fp=debug".into());
        }

        let filter = directives.join(",");

        let mut builder = env_logger::Builder::new();
        builder.target(env_logger::Target::Stderr);
        if filter.is_empty() {
            // Nothing requested: env_logger defaults to `error`, which is fine —
            // our diagnostic targets are trace/debug and stay silent.
            builder.parse_filters("");
        } else {
            builder.parse_filters(&filter);
        }
        // try_init: don't panic if a logger is already installed (e.g. a test
        // harness or another crate set one first).
        let _ = builder.try_init();
    });
}
