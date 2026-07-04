//! Runtime error raising: `RuntimeError`/`RuntimeErrorKind`, the eager and
//! lazy "poison" pointer machinery, diagnostics, and the App/case-dispatch
//! guards (`debug_app_check`, `runtime_case_trap`).

use crate::context::VMContext;
use crate::layout;
use crate::machine_state::{current_machine, machine_state};
use std::cell::{Cell, RefCell};
use tidepool_heap::layout as heap_layout;

use super::force::heap_force;
use super::gc::{register_rust_root, rust_roots_mark, truncate_rust_roots};

/// Addresses below this are considered invalid (null page guard).
pub(crate) const MIN_VALID_ADDR: u64 = 0x1000;

/// Runtime errors raised by JIT code via host functions.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuntimeError {
    #[error("division by zero")]
    DivisionByZero,
    #[error("arithmetic overflow")]
    Overflow,
    #[error("Haskell error called")]
    UserError,
    #[error("Haskell undefined forced")]
    Undefined,
    #[error("case trap: scrutinee constructor not among case alternatives (tag mismatch; diagnostics on server stderr)")]
    CaseTrap,
    #[error("bad pointer in JIT runtime (diagnostics on server stderr)")]
    BadPointer,
    #[error("forced type metadata (should be dead code)")]
    TypeMetadata,
    #[error("unresolved variable VarId({0:#x}){name} — a compiler bug, not a user error; report it (TIDEPOOL_VARID_AUDIT={0:x} on the extract names it too)", name = .1.as_deref().map(|n| format!(" = {n}")).unwrap_or_default())]
    UnresolvedVar(u64, Option<String>),
    #[error("application of null function pointer")]
    NullFunPtr,
    #[error("application of non-closure (tag={0})")]
    BadFunPtrTag(u8),
    #[error("heap overflow (nursery exhausted after GC)")]
    HeapOverflow,
    #[error("stack overflow — likely unbounded/non-tail recursion, or a very long list strict-forced at a bind (`x <- e` deep-forces its result; >~15k elements overflows — process inside one expression or bind an aggregate instead)")]
    StackOverflow,
    #[error("blackhole detected (infinite loop: thunk forced itself)")]
    BlackHole,
    #[error("thunk has invalid evaluation state: {0}")]
    BadThunkState(u8),
    #[error("Haskell error: {0}")]
    UserErrorMsg(String),
    /// External cancellation requested via a `CancelHandle`.
    /// Observed at the next GC safepoint (heap check).
    #[error("execution cancelled by external request")]
    Cancelled,
}

/// The `kind` discriminant JIT code passes to [`runtime_error`] /
/// [`runtime_error_with_msg`]. The numeric values are a FROZEN ABI: they are
/// emitted as `iconst` args by the JIT (derived from the Haskell error-sentinel
/// `VarId`, `extract_error_kind`), so the discriminants must stay byte-identical.
///
/// Centralising them here kills the previously hand-parallel `match kind` arms
/// (one for the diagnostic name, one for the `RuntimeError`) that could — and
/// did — disagree: the old `_` name was `"Unknown"` while the old `_` error
/// variant was `UserError`. Now both derive from one decode, so an unknown
/// discriminant is `UserError` consistently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum RuntimeErrorKind {
    DivisionByZero = 0,
    Overflow = 1,
    UserError = 2,
    Undefined = 3,
    TypeMetadata = 4,
}

impl RuntimeErrorKind {
    /// Decode the raw ABI discriminant. Unknown values map to `UserError`
    /// (matching the historical `_ =>` fallback).
    pub fn from_u64(kind: u64) -> Self {
        match kind {
            0 => Self::DivisionByZero,
            1 => Self::Overflow,
            3 => Self::Undefined,
            4 => Self::TypeMetadata,
            // includes 2 (UserError) and any out-of-range discriminant
            _ => Self::UserError,
        }
    }

    /// Short diagnostic name (used in the `[JIT] runtime_error` breadcrumb).
    pub fn name(self) -> &'static str {
        match self {
            Self::DivisionByZero => "DivisionByZero",
            Self::Overflow => "Overflow",
            Self::UserError => "UserError",
            Self::Undefined => "Undefined",
            Self::TypeMetadata => "TypeMetadata",
        }
    }

    /// The `RuntimeError` this kind raises (message-less variant).
    pub fn into_error(self) -> RuntimeError {
        match self {
            Self::DivisionByZero => RuntimeError::DivisionByZero,
            Self::Overflow => RuntimeError::Overflow,
            Self::UserError => RuntimeError::UserError,
            Self::Undefined => RuntimeError::Undefined,
            Self::TypeMetadata => RuntimeError::TypeMetadata,
        }
    }
}

thread_local! {
    static EXEC_CONTEXT: RefCell<String> = const { RefCell::new(String::new()) };
    pub(crate) static SIGNAL_SAFE_CTX: Cell<[u8; 128]> = const { Cell::new([0u8; 128]) };
    pub(crate) static SIGNAL_SAFE_CTX_LEN: Cell<usize> = const { Cell::new(0) };
}

/// Unconditionally overwrite this thread's current-machine pending cause —
/// no-op if no machine is installed. Mirrors writers (e.g.
/// `unresolved_var_trap`, the case/app traps) that replace any earlier cause
/// rather than preserving it; see `MachineState::set_runtime_error_overwrite`
/// and contrast with the first-write-wins [`set_first_cause`].
pub(crate) fn overwrite_runtime_error(cause: RuntimeError) {
    if let Some(ms) = unsafe { current_machine() } {
        ms.set_runtime_error_overwrite(cause);
    }
}

/// Set the current execution context for JIT code.
/// This is used to provide more info when a signal (SIGSEGV/SIGILL) occurs.
pub fn set_exec_context(ctx: &str) {
    EXEC_CONTEXT.with(|c| {
        let mut s = c.borrow_mut();
        s.clear();
        s.push_str(ctx);
    });
    SIGNAL_SAFE_CTX.with(|c| {
        let mut buf = [0u8; 128];
        let len = ctx.len().min(128);
        buf[..len].copy_from_slice(&ctx.as_bytes()[..len]);
        c.set(buf);
    });
    SIGNAL_SAFE_CTX_LEN.with(|c| c.set(ctx.len().min(128)));
}

/// Get the current execution context.
pub fn get_exec_context() -> String {
    EXEC_CONTEXT.with(|c| c.borrow().clone())
}

/// Push a diagnostic message to the thread-local buffer.
pub fn push_diagnostic(msg: String) {
    if let Some(ms) = unsafe { current_machine() } {
        ms.push_diagnostic(msg);
    }
}

/// Drain all accumulated diagnostics.
pub fn drain_diagnostics() -> Vec<String> {
    unsafe { current_machine() }
        .map(|ms| ms.drain_diagnostics())
        .unwrap_or_default()
}

/// varId → human name, registered from meta.cbor's `var_names` at load time
/// so runtime unresolved-variable errors can NAME the symbol (friction #12).
/// Process-global and append-only: ids are content-addressed (stableVarId /
/// disambiguated local hashes), so cross-session collisions mean identical
/// names anyway.
static VAR_NAMES: std::sync::OnceLock<std::sync::RwLock<std::collections::HashMap<u64, String>>> =
    std::sync::OnceLock::new();

/// Register varId → name pairs (from `MetaWarnings::var_names`).
pub fn register_var_names(pairs: &[(u64, String)]) {
    if pairs.is_empty() {
        return;
    }
    let map = VAR_NAMES.get_or_init(Default::default);
    let mut w = map
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for (id, name) in pairs {
        w.insert(*id, name.clone());
    }
}

fn lookup_var_name(id: u64) -> Option<String> {
    let map = VAR_NAMES.get()?;
    let r = map
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    r.get(&id).cloned()
}

#[cfg(test)]
mod var_name_tests {
    use super::*;

    #[test]
    fn unresolved_error_names_registered_symbol() {
        register_var_names(&[(0xfe00_0000_0000_1234, "GHC.Internal.List.cycle".into())]);
        let e = RuntimeError::UnresolvedVar(
            0xfe00_0000_0000_1234,
            lookup_var_name(0xfe00_0000_0000_1234),
        );
        let msg = e.to_string();
        assert!(msg.contains("= GHC.Internal.List.cycle"), "{msg}");
        // unregistered ids still render (hex only, no name segment)
        let bare = RuntimeError::UnresolvedVar(0xfe99, lookup_var_name(0xfe99));
        assert!(bare.to_string().contains("VarId(0xfe99)"), "{bare}");
    }
}

/// Called by JIT code when an unresolved external variable is forced.
/// Returns null to allow execution to continue (will likely segfault later).
/// In debug mode (TIDEPOOL_TRACE), logs and returns null.
pub extern "C" fn unresolved_var_trap(var_id: u64) -> *mut u8 {
    let tag_char = (var_id >> 56) as u8 as char;
    let key = var_id & ((1u64 << 56) - 1);
    let name = lookup_var_name(var_id);
    let named = name
        .as_deref()
        .map(|n| format!(" = {n}"))
        .unwrap_or_default();
    let msg = format!(
        "[JIT] Forced unresolved external variable: VarId({:#x}){named} [tag='{}', key={}] — \
         a compiler bug, not a user error (the extract should have failed loudly); \
         report it",
        var_id, tag_char, key
    );
    eprintln!("{}", msg);
    push_diagnostic(msg);
    overwrite_runtime_error(RuntimeError::UnresolvedVar(var_id, name));
    error_poison_ptr()
}

/// Called by JIT code for runtime errors (divZeroError, overflowError).
/// Sets a thread-local error flag and returns a "poison" Lit(Int#, 0) object
/// instead of null. This prevents JIT code from segfaulting on the return value.
/// The effect machine checks the error flag after JIT returns and converts
/// to Yield::Error.
/// kind: 0 = divZeroError, 1 = overflowError, 2 = UserError, 3 = Undefined
pub extern "C" fn runtime_error(kind: u64) -> *mut u8 {
    let rk = RuntimeErrorKind::from_u64(kind);
    let msg = format!("[JIT] runtime_error called: kind={} ({})", kind, rk.name());
    eprintln!("{}", msg);
    push_diagnostic(msg);
    let err = rk.into_error();
    set_first_cause(err);
    // Return a poison object instead of null. This is a valid Lit(Int#, 0)
    // heap object, so JIT code won't segfault when reading its tag byte.
    // The effect machine will detect the error flag and return Yield::Error
    // before this poison value reaches user code.
    error_poison_ptr()
}

/// Called by JIT code for runtime errors with a specific message.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn runtime_error_with_msg(kind: u64, msg_ptr: *const u8, msg_len: u64) -> *mut u8 {
    let msg = if !msg_ptr.is_null() && msg_len > 0 {
        // SAFETY: msg_ptr is non-null and points to msg_len bytes of valid memory
        // from a JIT-allocated LitString or leaked message buffer.
        let slice = unsafe { std::slice::from_raw_parts(msg_ptr, msg_len as usize) };
        String::from_utf8_lossy(slice).to_string()
    } else {
        String::new()
    };
    let rk = RuntimeErrorKind::from_u64(kind);
    let diag = format!(
        "[JIT] runtime_error called: kind={} ({}) msg={:?}",
        kind,
        rk.name(),
        msg
    );
    eprintln!("{}", diag);
    push_diagnostic(diag);
    let err = match rk {
        RuntimeErrorKind::UserError if !msg.is_empty() => RuntimeError::UserErrorMsg(msg),
        other => other.into_error(),
    };
    set_first_cause(err);
    error_poison_ptr()
}

/// Record `cause` as the run's pending first cause for the current thread,
/// unless an earlier cause is already recorded — first write wins, because the
/// earliest record is the one closest to the fault. Public so effect
/// dispatchers can record a cooperative cancellation (a `PauseGate` abort
/// observed at an effect checkpoint) as the same first cause a flag-fired
/// cancel records; [`surface_error`] then surfaces `RuntimeError::Cancelled`
/// regardless of which cancellation channel fired.
pub fn set_first_cause(cause: RuntimeError) {
    if let Some(ms) = unsafe { current_machine() } {
        ms.set_first_cause(cause);
    }
}

/// Returns true if a runtime error has been set for the current thread.
pub fn has_runtime_error() -> bool {
    unsafe { current_machine() }.is_some_and(|ms| ms.has_runtime_error())
}

pub extern "C" fn runtime_oom() -> *mut u8 {
    // First-write-wins: the external-cancellation path (see `gc_trigger`)
    // records `RuntimeError::Cancelled` and then forces `runtime_oom` to fire;
    // `set_first_cause` keeps the more specific cancellation cause instead of
    // overwriting it with `HeapOverflow`.
    set_first_cause(RuntimeError::HeapOverflow);
    error_poison_ptr()
}

/// Called by JIT code when a BlackHole is encountered (thunk forcing itself).
pub extern "C" fn runtime_blackhole_trap(_vmctx: *mut VMContext) -> *mut u8 {
    let msg = "[JIT] BlackHole detected: infinite loop (thunk forcing itself)".to_string();
    eprintln!("{}", msg);
    push_diagnostic(msg);
    overwrite_runtime_error(RuntimeError::BlackHole);
    error_poison_ptr()
}

/// Called by JIT code when a Thunk has an invalid state.
pub extern "C" fn runtime_bad_thunk_state_trap(_vmctx: *mut VMContext, state: u8) -> *mut u8 {
    let msg = format!("[JIT] Invalid thunk state: {}", state);
    eprintln!("{}", msg);
    push_diagnostic(msg);
    overwrite_runtime_error(RuntimeError::BadThunkState(state));
    error_poison_ptr()
}

/// Size of the poison buffer.
///
/// The JIT's `emit_alloc_fast_path` slow-fail edge calls `runtime_oom`, takes
/// the returned pointer as if it were a freshly-allocated heap object, and
/// then unconditionally writes the full header + payload into it (tag byte,
/// size halfword, Con/Closure/Thunk fields, capture slots, …). If the poison
/// is smaller than the attempted allocation, those post-OOM stores spill past
/// the poison into adjacent heap — we've observed glibc "corrupted size vs.
/// prev_size" aborts as a direct consequence.
///
/// The JIT never clamps allocation size at emit time. The effective upper
/// bound is `CON_FIELDS_OFFSET + MAX_FIELDS * 8` (i.e. the largest Con the
/// read-side `heap_bridge` is willing to decode; see `MAX_FIELDS = 1024`
/// there). Closures and thunks are bounded by the same field/capture count
/// in practice. We size the poison to comfortably absorb that worst case so
/// any OOM path can complete its field writes harmlessly.
///
/// 16 KiB: `24 + 8 * 1024 = 8216` bytes for a max-arity Con, doubled for
/// headroom. Stays well under the `u16` header `size` encoding limit.
pub(crate) const POISON_BUF_SIZE: usize = 16 * 1024;

/// Compile-time guard: the poison buffer must be large enough to absorb a
/// post-OOM write of a worst-case Con at the read-side decoder's
/// `MAX_FIELDS` ceiling. If `MAX_FIELDS` is bumped without updating
/// `POISON_BUF_SIZE`, this assertion fails to compile rather than
/// regressing into the runtime heap-corruption symptom that PR #272
/// originally diagnosed (glibc "corrupted size vs. prev_size" aborts on
/// OOM paths writing past the old 24-byte poison). The matching runtime
/// regression test lives in the module's `tests` block under
/// `poison_buf_absorbs_max_con_write`.
const _: () = {
    let worst_case_con = layout::CON_FIELDS_OFFSET as usize + crate::heap_bridge::MAX_FIELDS * 8;
    assert!(
        POISON_BUF_SIZE >= worst_case_con,
        "POISON_BUF_SIZE must absorb worst-case Con write \
         (CON_FIELDS_OFFSET + MAX_FIELDS * 8); bump POISON_BUF_SIZE \
         when MAX_FIELDS grows",
    );
};

/// Return a pointer to a pre-allocated "poison" Closure heap object.
/// When JIT code tries to call this as a function, it returns itself,
/// preventing cascading crashes. The runtime error flag is already set,
/// so the effect machine will catch it before the poison reaches user code.
///
/// The backing allocation is oversized (`POISON_BUF_SIZE`) so that OOM
/// paths which treat the poison as freshly-allocated scratch (via
/// `runtime_oom`) can complete their field writes without corrupting
/// adjacent heap. See `POISON_BUF_SIZE` for rationale.
pub fn error_poison_ptr() -> *mut u8 {
    use std::sync::OnceLock;
    // Layout: Closure with code_ptr pointing to `poison_trampoline`,
    // num_captured = 0. When called, returns the poison closure itself.
    static POISON: OnceLock<usize> = OnceLock::new();
    let addr = *POISON.get_or_init(|| {
        // Backing buffer is oversized to absorb post-OOM scratch writes
        // from the JIT (see POISON_BUF_SIZE docs). The Closure header
        // describes only the logical 24-byte Closure layout — the tail
        // bytes are zero-initialized padding that the JIT may clobber
        // after a `runtime_oom` return.
        let logical_size = 24u16;
        let layout = std::alloc::Layout::from_size_align(POISON_BUF_SIZE, 8)
            .unwrap_or_else(|_| std::process::abort());
        // SAFETY: alloc_zeroed returns a valid, zeroed allocation of the requested size.
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        // SAFETY: ptr is a fresh allocation of POISON_BUF_SIZE bytes
        // (>= 24). Writing the closure header, code pointer, and capture
        // count at known offsets within the first 24 bytes.
        unsafe {
            tidepool_heap::layout::write_header(
                ptr,
                tidepool_heap::layout::TAG_CLOSURE,
                logical_size,
            );
            // code_ptr = poison_trampoline
            *(ptr.add(tidepool_heap::layout::CLOSURE_CODE_PTR_OFFSET) as *mut usize) =
                poison_trampoline as *const () as usize;
            // num_captured = 0
            *(ptr.add(tidepool_heap::layout::CLOSURE_NUM_CAPTURED_OFFSET) as *mut u16) = 0;
        }
        ptr as usize
    });
    addr as *mut u8
}

/// Trampoline for the poison closure. Returns the poison closure itself,
/// so any chain of function applications on an error result just keeps
/// returning the poison without crashing.
// SAFETY: Only called via JIT code applying the poison closure. Returns the
// static poison pointer — no memory writes, no side effects beyond the return.
unsafe extern "C" fn poison_trampoline(
    _vmctx: *mut VMContext,
    _closure: *mut u8,
    _arg: *mut u8,
) -> *mut u8 {
    error_poison_ptr()
}

/// Return a pre-allocated "lazy poison" Closure for a given error kind.
/// Unlike `error_poison_ptr()`, this does NOT set the error flag at creation
/// time. The error is only triggered when the closure is actually called
/// (via `poison_trampoline_lazy`). This is critical for typeclass dictionaries
/// where error methods exist as fields but may never be invoked.
///
/// kind: 0=DivisionByZero, 1=Overflow, 2=UserError, 3=Undefined, 4=TypeMetadata
pub fn error_poison_ptr_lazy(kind: u64) -> *mut u8 {
    use std::sync::OnceLock;
    static LAZY_POISONS: OnceLock<[usize; 5]> = OnceLock::new();
    let ptrs = LAZY_POISONS.get_or_init(|| {
        let mut arr = [0usize; 5];
        for k in 0..5u64 {
            // Closure: header(8) + code_ptr(8) + num_captured(2+pad=8) + captured[0](8) = 32
            let size = 32usize;
            let lo = std::alloc::Layout::from_size_align(size, 8)
                .unwrap_or_else(|_| std::process::abort());
            // SAFETY: alloc_zeroed returns a valid, zeroed allocation of the requested size.
            let ptr = unsafe { std::alloc::alloc_zeroed(lo) };
            if ptr.is_null() {
                std::alloc::handle_alloc_error(lo);
            }
            // SAFETY: ptr is a fresh 32-byte allocation. Writing closure header, code pointer,
            // capture count, and captured error kind at known offsets.
            unsafe {
                tidepool_heap::layout::write_header(
                    ptr,
                    tidepool_heap::layout::TAG_CLOSURE,
                    size as u16,
                );
                *(ptr.add(tidepool_heap::layout::CLOSURE_CODE_PTR_OFFSET) as *mut usize) =
                    poison_trampoline_lazy as *const () as usize;
                *(ptr.add(tidepool_heap::layout::CLOSURE_NUM_CAPTURED_OFFSET) as *mut u16) = 1;
                *(ptr.add(tidepool_heap::layout::CLOSURE_CAPTURED_OFFSET) as *mut u64) = k;
            }
            arr[k as usize] = ptr as usize;
        }
        arr
    });
    ptrs[kind.min(4) as usize] as *mut u8
}

/// Raise a runtime error whose message is materialized from a live heap value.
/// Called by JIT Raise sites whose message wasn't statically extractable
/// (floated bindings, thunk-subtree captures, dynamically built messages).
/// Handles String literals, Text constructors, and String cons-lists; falls
/// back to a message-less error on any other shape.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn runtime_error_dynamic(vmctx: *mut VMContext, kind: u64, arg: *mut u8) -> *mut u8 {
    // SAFETY: vmctx and arg come from JIT code; arg may be null.
    let msg = unsafe { materialize_message(vmctx, arg) };
    match msg {
        Some(bytes) if !bytes.is_empty() => {
            // runtime_error_with_msg copies the bytes into an owned String.
            runtime_error_with_msg(kind, bytes.as_ptr(), bytes.len() as u64)
        }
        _ => runtime_error(kind),
    }
}

/// Best-effort conversion of a heap value into UTF-8 message bytes.
/// Forces thunks as needed; all locals held across forces are registered as
/// GC roots (forcing can collect, and host frames are invisible to the
/// frame walker).
///
/// # Safety
/// `vmctx` must be valid; `arg` must be null or a valid heap object.
unsafe fn materialize_message(vmctx: *mut VMContext, arg: *mut u8) -> Option<Vec<u8>> {
    const MAX_MSG_BYTES: usize = 4096;
    if vmctx.is_null() || arg.is_null() {
        return None;
    }

    let mark = rust_roots_mark();
    let mut cur: *mut u8 = arg;
    let mut tmp: *mut u8 = std::ptr::null_mut();
    register_rust_root(&mut cur as *mut *mut u8);
    register_rust_root(&mut tmp as *mut *mut u8);

    // Reads an Int/Char payload, looking through an I#/C# box.
    // Does not force; callers force into `tmp` first.
    let read_small_int = |p: *mut u8| -> Option<i64> {
        match heap_layout::read_tag(p) {
            t if t == tidepool_heap::layout::TAG_LIT => {
                Some(*(p.add(tidepool_heap::layout::LIT_VALUE_OFFSET) as *const i64))
            }
            t if t == tidepool_heap::layout::TAG_CON => {
                let nf = *(p.add(tidepool_heap::layout::CON_NUM_FIELDS_OFFSET) as *const u16);
                if nf == 1 {
                    let f0 = *(p.add(tidepool_heap::layout::CON_FIELDS_OFFSET) as *const *mut u8);
                    if !f0.is_null() && heap_layout::read_tag(f0) == tidepool_heap::layout::TAG_LIT
                    {
                        return Some(
                            *(f0.add(tidepool_heap::layout::LIT_VALUE_OFFSET) as *const i64),
                        );
                    }
                }
                None
            }
            _ => None,
        }
    };

    let read_lit_string = |p: *mut u8| -> Option<Vec<u8>> {
        if heap_layout::read_tag(p) != tidepool_heap::layout::TAG_LIT {
            return None;
        }
        // LitString and ByteArray# share the [len: u64][bytes...] payload
        // layout; Text's first field is a ByteArray#.
        let lit_tag = *p.add(tidepool_heap::layout::LIT_TAG_OFFSET);
        if lit_tag != 5 && lit_tag != crate::layout::LIT_TAG_BYTEARRAY as u8 {
            // 5 = LIT_TAG_STRING
            return None;
        }
        let raw = *(p.add(tidepool_heap::layout::LIT_VALUE_OFFSET) as *const *const u8);
        if raw.is_null() {
            return None;
        }
        let len = (*(raw as *const u64) as usize).min(MAX_MSG_BYTES);
        Some(std::slice::from_raw_parts(raw.add(8), len).to_vec())
    };

    let result = (|| -> Option<Vec<u8>> {
        if is_lazy_poison(cur) {
            return None;
        }
        cur = heap_force(vmctx, cur);
        if has_runtime_error() {
            return None;
        }

        // Bare string literal.
        if let Some(bytes) = read_lit_string(cur) {
            return Some(bytes);
        }

        if heap_layout::read_tag(cur) != tidepool_heap::layout::TAG_CON {
            return None;
        }
        let nf = *(cur.add(tidepool_heap::layout::CON_NUM_FIELDS_OFFSET) as *const u16);

        // Text bytes offset len — field 0 holds the byte buffer, possibly
        // behind single-field box constructors (ByteArray ba#).
        if nf == 3 {
            tmp = *(cur.add(tidepool_heap::layout::CON_FIELDS_OFFSET) as *const *mut u8);
            if tmp.is_null() {
                return None;
            }
            tmp = heap_force(vmctx, tmp);
            while heap_layout::read_tag(tmp) == tidepool_heap::layout::TAG_CON
                && *(tmp.add(tidepool_heap::layout::CON_NUM_FIELDS_OFFSET) as *const u16) == 1
            {
                let inner = *(tmp.add(tidepool_heap::layout::CON_FIELDS_OFFSET) as *const *mut u8);
                if inner.is_null() {
                    return None;
                }
                tmp = heap_force(vmctx, inner);
            }
            let bytes = read_lit_string(tmp)?;
            // Re-read offset/len AFTER the force above (cur may have moved).
            let f1 = *(cur.add(tidepool_heap::layout::CON_FIELDS_OFFSET + 8) as *const *mut u8);
            let f2 = *(cur.add(tidepool_heap::layout::CON_FIELDS_OFFSET + 16) as *const *mut u8);
            let off = read_small_int(f1)? as usize;
            let len = read_small_int(f2)? as usize;
            if off <= bytes.len() {
                let end = (off + len).min(bytes.len());
                return Some(bytes[off..end].to_vec());
            }
            return None;
        }

        // String cons-list of Chars.
        let mut out: Vec<u8> = Vec::new();
        loop {
            let tag = heap_layout::read_tag(cur);
            if tag != tidepool_heap::layout::TAG_CON {
                break;
            }
            let nf = *(cur.add(tidepool_heap::layout::CON_NUM_FIELDS_OFFSET) as *const u16);
            if nf != 2 {
                break; // nil (0 fields) or not a list shape
            }
            tmp = *(cur.add(tidepool_heap::layout::CON_FIELDS_OFFSET) as *const *mut u8);
            if tmp.is_null() {
                break;
            }
            tmp = heap_force(vmctx, tmp);
            if has_runtime_error() {
                break;
            }
            let Some(c) = read_small_int(tmp) else { break };
            let Some(ch) = char::from_u32(c as u32) else {
                break;
            };
            let mut buf = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            if out.len() >= MAX_MSG_BYTES {
                break;
            }
            // Re-read the tail AFTER forcing the head (cur may have moved).
            let next = *(cur.add(tidepool_heap::layout::CON_FIELDS_OFFSET + 8) as *const *mut u8);
            if next.is_null() {
                break;
            }
            cur = heap_force(vmctx, next);
            if has_runtime_error() {
                break;
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    })();

    truncate_rust_roots(mark);
    result
}

/// Check whether a heap object is a lazy poison closure (⊥ with a deferred
/// error). Used by the heap bridge: converting ⊥ to a `Value` is a genuine
/// demand, so the bridge invokes the trampoline to raise the deferred error
/// (with its captured message) instead of misreading the closure as data.
/// Deliberately NOT consulted by `heap_force`: dictionaries carry poison in
/// never-selected method slots, and effect plumbing forces bound values it
/// must not observe.
///
/// # Safety
/// `ptr` must be a valid heap object pointer.
pub unsafe fn is_lazy_poison(ptr: *const u8) -> bool {
    if heap_layout::read_tag(ptr as *mut u8) != tidepool_heap::layout::TAG_CLOSURE {
        return false;
    }
    let code_ptr = *(ptr.add(tidepool_heap::layout::CLOSURE_CODE_PTR_OFFSET) as *const usize);
    code_ptr == poison_trampoline_lazy as *const () as usize
        || code_ptr == poison_trampoline_lazy_msg as *const () as usize
}

/// Invoke a lazy poison closure's trampoline, setting the runtime error flag
/// (including any captured message) and returning the eager poison pointer.
///
/// # Safety
/// `ptr` must satisfy `is_lazy_poison`.
pub unsafe fn raise_lazy_poison(vmctx: *mut VMContext, ptr: *mut u8) -> *mut u8 {
    let code_ptr = *(ptr.add(tidepool_heap::layout::CLOSURE_CODE_PTR_OFFSET) as *const usize);
    let f: unsafe extern "C" fn(*mut VMContext, *mut u8, *mut u8) -> *mut u8 =
        std::mem::transmute(code_ptr);
    f(vmctx, ptr, std::ptr::null_mut())
}

/// Trampoline for lazy poison closures. Reads the error kind from captured[0]
/// and raises — setting the error flag only now, when the closure is actually
/// invoked. The argument is the error's message expression whenever the
/// sentinel was applied (including first-class uses like the point-free
/// `error . unpack` shadow, where the poison closure receives the already
/// computed String at call time): materialize it into the message.
// SAFETY: closure points to a lazy poison closure allocated by error_poison_ptr_lazy
// with captured[0] = error kind. arg may be null or a valid heap object.
unsafe extern "C" fn poison_trampoline_lazy(
    vmctx: *mut VMContext,
    closure: *mut u8,
    arg: *mut u8,
) -> *mut u8 {
    let kind = *(closure.add(tidepool_heap::layout::CLOSURE_CAPTURED_OFFSET) as *const u64);

    if let Some(bytes) = materialize_message(vmctx, arg) {
        if !bytes.is_empty() {
            return runtime_error_with_msg(kind, bytes.as_ptr(), bytes.len() as u64);
        }
    }

    // Non-string argument (e.g. the CallStack dict in a partial application
    // like `error cs`): swallow it and return self, so the eventual
    // application to the actual message raises with that message. A poison
    // that is forced as a value (never applied) reaches the bridge, which
    // raises message-less via raise_lazy_poison(null).
    if !arg.is_null() {
        return closure;
    }

    runtime_error(kind)
}

/// Create a pre-allocated "lazy poison" Closure for a given error kind and message.
pub fn error_poison_ptr_lazy_msg(kind: u64, msg: &[u8]) -> *mut u8 {
    // Leak the message bytes so they live forever
    let msg_bytes = msg.to_vec().into_boxed_slice();
    let msg_ptr = msg_bytes.as_ptr();
    let msg_len = msg_bytes.len();
    std::mem::forget(msg_bytes);

    // Allocate closure with 3 captures: kind, msg_ptr, msg_len
    // Closure: header(8) + code_ptr(8) + num_captured(2+pad=8) + 3*8 = 48
    let size = tidepool_heap::layout::CLOSURE_CAPTURED_OFFSET + 3 * 8;
    let layout = std::alloc::Layout::from_size_align(size, 8).expect("constant size/align");
    // SAFETY: alloc_zeroed returns a valid, zeroed allocation of the requested size.
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
    if ptr.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    // SAFETY: ptr is a fresh allocation. Writing closure header, code pointer,
    // capture count, and 3 captures (kind, msg_ptr, msg_len) at known offsets.
    // msg_ptr is a leaked allocation that lives forever.
    unsafe {
        tidepool_heap::layout::write_header(ptr, tidepool_heap::layout::TAG_CLOSURE, size as u16);
        *(ptr.add(tidepool_heap::layout::CLOSURE_CODE_PTR_OFFSET) as *mut usize) =
            poison_trampoline_lazy_msg as *const () as usize;
        *(ptr.add(tidepool_heap::layout::CLOSURE_NUM_CAPTURED_OFFSET) as *mut u16) = 3;
        *(ptr.add(tidepool_heap::layout::CLOSURE_CAPTURED_OFFSET) as *mut u64) = kind;
        *(ptr.add(tidepool_heap::layout::CLOSURE_CAPTURED_OFFSET + 8) as *mut usize) =
            msg_ptr as usize;
        *(ptr.add(tidepool_heap::layout::CLOSURE_CAPTURED_OFFSET + 16) as *mut u64) =
            msg_len as u64;
    }
    ptr
}

// SAFETY: closure points to a lazy poison closure with 3 captures (kind, msg_ptr, msg_len)
// allocated by error_poison_ptr_lazy_msg. The msg_ptr was leaked and remains valid.
unsafe extern "C" fn poison_trampoline_lazy_msg(
    _vmctx: *mut VMContext,
    closure: *mut u8,
    _arg: *mut u8,
) -> *mut u8 {
    let kind = *(closure.add(tidepool_heap::layout::CLOSURE_CAPTURED_OFFSET) as *const u64);
    let msg_ptr =
        *(closure.add(tidepool_heap::layout::CLOSURE_CAPTURED_OFFSET + 8) as *const *const u8);
    let msg_len = *(closure.add(tidepool_heap::layout::CLOSURE_CAPTURED_OFFSET + 16) as *const u64);
    runtime_error_with_msg(kind, msg_ptr, msg_len)
}

/// Check and take any pending runtime error from JIT code.
///
/// Uses `try_borrow_mut` defensively: this runs on the signal/teardown path
/// (`runtime_error_or_signal` and `RegistryGuard::drop`), and a signal can fire
/// while JIT host code (e.g. `debug_app_check` setting `StackOverflow`) still
/// holds a `borrow_mut` on the cell. A plain `borrow_mut` would then panic —
/// and panicking inside `Drop`/unwind double-panics → `abort()`. If the cell is
/// momentarily borrowed, there is no error we can safely take here; return None
/// and let the caller fall back (e.g. `Signal(sig)`).
pub fn take_runtime_error() -> Option<RuntimeError> {
    unsafe { current_machine() }.and_then(|ms| ms.take_runtime_error())
}

/// The single first-cause resolver at the JIT boundary.
///
/// A pending [`RuntimeError`] is the run's *first cause*: it was recorded by
/// the host fn or safepoint closest to the fault (`Cancelled` at a cancel
/// safepoint or effect checkpoint, `HeapOverflow` in [`runtime_oom`], …).
/// What a boundary observes afterwards is often only a downstream *symptom* —
/// a signal, a failed (or poison-fed) heap-bridge conversion, a handler
/// error. If a first cause is pending, take it (clearing the cell) and
/// surface it; otherwise pass the symptomatic outcome through unchanged.
/// Every boundary that chooses between the recorded cause and an observed
/// symptom resolves through here.
pub fn surface_error<T, E: From<RuntimeError>>(symptom: Result<T, E>) -> Result<T, E> {
    match take_runtime_error() {
        Some(cause) => Err(E::from(cause)),
        None => symptom,
    }
}

/// Check pointer validity; if bad, set runtime error and return true.
pub(crate) fn check_ptr_invalid(ptr: *const u8, fn_name: &str) -> bool {
    if (ptr as i64) < MIN_VALID_ADDR as i64 {
        let msg = format!("[BUG] {}: bad pointer {:#x}", fn_name, ptr as u64);
        eprintln!("{}", msg);
        push_diagnostic(msg);
        overwrite_runtime_error(RuntimeError::BadPointer);
        true
    } else {
        false
    }
}

/// Return the list of host function symbols for JIT registration.
///
/// Usage: `CodegenPipeline::new(&host_fn_symbols())`
/// Debug: called before every App call_indirect to validate the function pointer.
/// Prints the heap tag and code_ptr. Aborts on non-closure.
///
/// # Safety
///
/// `fun_ptr` must point to a valid HeapObject if not null.
/// Maximum call depth before raising StackOverflow. This catches infinite
/// recursion (e.g. `[0..]` in non-fusing context) with a clean error
/// instead of SIGSEGV from stack overflow.
const MAX_CALL_DEPTH: u32 = 20_000;

/// Returns 0 if the call is safe to proceed, or a poison pointer if the call
/// should be short-circuited (runtime error already set or call depth exceeded).
///
/// # Safety
/// `vmctx` must be non-null with `machine_state` installed; `fun_ptr` must
/// point to a valid HeapObject or be null.
pub unsafe extern "C" fn debug_app_check(vmctx: *mut VMContext, fun_ptr: *const u8) -> *mut u8 {
    // If a runtime error is already pending, don't abort on tag mismatches —
    // we're in error-propagation mode and the effect machine will handle it.
    let has_error = has_runtime_error();

    // SAFETY: caller contract above.
    let ms = unsafe { machine_state(vmctx) };

    // Check call depth to catch runaway recursion before stack overflow.
    if !has_error {
        let depth = ms.incr_call_depth();
        if depth > MAX_CALL_DEPTH {
            overwrite_runtime_error(RuntimeError::StackOverflow);
            return error_poison_ptr();
        }
    }
    if fun_ptr.is_null() {
        if has_error {
            return error_poison_ptr(); // Error already flagged, just continue
        }
        let msg = "[JIT] App: fun_ptr is NULL — unresolved binding".to_string();
        eprintln!("{}", msg);
        push_diagnostic(msg);
        overwrite_runtime_error(RuntimeError::NullFunPtr);
        return error_poison_ptr();
    }
    // SAFETY: fun_ptr was checked non-null above; reading the tag byte at offset 0
    // of a heap object is valid for any object allocated by the JIT nursery.
    let tag = unsafe { *fun_ptr };
    if tag != tidepool_heap::layout::TAG_CLOSURE {
        use std::io::Write;
        let mut stderr = std::io::stderr().lock();
        if has_error {
            return error_poison_ptr(); // Error already flagged, tag mismatch is expected (poison object)
        }
        let tag_name = match tag {
            0 => "Closure",
            1 => "Thunk",
            2 => "Con",
            3 => "Lit",
            _ => "UNKNOWN",
        };
        let msg = format!(
            "[JIT] App: fun_ptr={:p} has tag {} ({}) — expected Closure!",
            fun_ptr, tag, tag_name
        );
        let _ = writeln!(stderr, "{}", msg);
        push_diagnostic(msg);
        if tag == tidepool_heap::layout::TAG_CON {
            // SAFETY: tag == TAG_CON confirms this is a Con heap object;
            // reading con_tag at offset 8 and num_fields at offset 16 is valid.
            let con_tag = unsafe { *(fun_ptr.add(layout::CON_TAG_OFFSET as usize) as *const u64) };
            let num_fields =
                unsafe { *(fun_ptr.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *const u16) };
            let msg2 = format!("[JIT]   Con tag={}, num_fields={}", con_tag, num_fields);
            let _ = writeln!(stderr, "{}", msg2);
            push_diagnostic(msg2);
        }
        let _ = stderr.flush();
        overwrite_runtime_error(RuntimeError::BadFunPtrTag(tag));
        return error_poison_ptr();
    }
    std::ptr::null_mut() // 0 = ok, proceed with the call
}

/// Debug: called instead of `trap user2` when TIDEPOOL_DEBUG_CASE is set.
/// Prints diagnostic info about the scrutinee that failed case matching.
/// `scrut_ptr` is the heap pointer to the scrutinee.
/// `num_alts` is the number of data alt tags expected.
/// `alt_tags` is a pointer to an array of expected tag u64 values.
pub extern "C" fn runtime_case_trap(
    scrut_ptr: i64,
    num_alts: i64,
    alt_tags: i64,
    fn_name_ptr: i64,
    fn_name_len: i64,
) -> *mut u8 {
    // Identify the enclosing compiled function (emit threads its name in).
    if fn_name_ptr != 0 && fn_name_len > 0 && fn_name_len < 4096 {
        // SAFETY: emit leaks a 'static str and passes its exact ptr/len.
        let name = unsafe {
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                fn_name_ptr as *const u8,
                fn_name_len as usize,
            ))
        };
        eprintln!("[CASE TRAP] in compiled fn: {}", name);
    }
    // If a runtime error is already pending (e.g. DivisionByZero), the poison
    // value cascaded into a case expression. Return poison again instead of
    // aborting — the error flag will be detected when with_signal_protection
    // returns.
    if has_runtime_error() {
        return error_poison_ptr();
    }

    let ptr = scrut_ptr as *const u8;

    // Check if the scrutinee is a lazy poison closure. If so, trigger it to set the error flag.
    if !ptr.is_null()
        // SAFETY: ptr is non-null (checked above). Reading the tag byte at offset 0.
        && unsafe { tidepool_heap::layout::read_tag(ptr) } == tidepool_heap::layout::TAG_CLOSURE
    {
        // SAFETY: ptr is a Closure (tag confirmed above). Reading code_ptr at the known offset.
        let code_ptr =
            unsafe { *(ptr.add(tidepool_heap::layout::CLOSURE_CODE_PTR_OFFSET) as *const usize) };
        if code_ptr == poison_trampoline_lazy as *const () as usize
            || code_ptr == poison_trampoline_lazy_msg as *const () as usize
        {
            // SAFETY: code_ptr is the poison trampoline function pointer. Calling it
            // with null vmctx and arg triggers the lazy error flag without side effects
            // beyond setting the current machine's runtime-error cell.
            unsafe {
                let func: unsafe extern "C" fn(*mut VMContext, *mut u8, *mut u8) -> *mut u8 =
                    std::mem::transmute(code_ptr);
                func(std::ptr::null_mut(), ptr as *mut u8, std::ptr::null_mut());
            }
            return error_poison_ptr();
        }
    }

    use std::io::Write;
    if check_ptr_invalid(scrut_ptr as *const u8, "runtime_case_trap") {
        return error_poison_ptr();
    }
    // SAFETY: ptr passed the null/low-address guard above. Reading the tag byte at offset 0.
    let tag_byte = unsafe { *ptr };
    let tag_name = match tag_byte {
        0 => "Closure",
        1 => "Thunk",
        2 => "Con",
        3 => "Lit",
        0xFF => "Forwarded(GC bug!)",
        _ => "UNKNOWN",
    };

    // Read expected alt tags
    // SAFETY: alt_tags points to a JIT data section array of num_alts u64 tag values.
    let expected: Vec<u64> = if num_alts > 0 && alt_tags != 0 {
        (0..num_alts as usize)
            .map(|i| unsafe { *((alt_tags as *const u64).add(i)) })
            .collect()
    } else {
        vec![]
    };

    // Dump raw bytes for any object type
    // SAFETY: ptr points to a heap object. Reading 32 bytes for diagnostic dump.
    // Heap objects are always at least this size (minimum header is 8 bytes + fields).
    let raw_bytes: Vec<u8> = (0..32).map(|i| unsafe { *ptr.add(i) }).collect();
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "[CASE TRAP] raw bytes: {:02x?}", raw_bytes);

    if tag_byte == layout::TAG_CON {
        // SAFETY: tag_byte == TAG_CON confirms Con; reading con_tag and num_fields at known offsets.
        let con_tag = unsafe { *(ptr.add(layout::CON_TAG_OFFSET as usize) as *const u64) };
        let num_fields =
            unsafe { *(ptr.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *const u16) };
        let _ = writeln!(
            stderr,
            "[CASE TRAP] Con: con_tag={:#x}, num_fields={}, expected_tags={:?}",
            con_tag, num_fields, expected
        );
    } else if tag_byte == layout::TAG_LIT {
        // SAFETY: tag_byte == TAG_LIT confirms Lit; reading lit_tag and value at known offsets.
        let lit_tag = unsafe { *(ptr.add(layout::LIT_TAG_OFFSET as usize) as *const u64) };
        let value = unsafe { *(ptr.add(layout::LIT_VALUE_OFFSET as usize) as *const u64) };
        let _ = writeln!(
            stderr,
            "[CASE TRAP] Lit: lit_tag={:#x}, value={:#x}, expected_tags={:?}",
            lit_tag, value, expected
        );
    } else if tag_byte == layout::TAG_CLOSURE {
        // SAFETY: tag_byte == TAG_CLOSURE confirms Closure; reading code_ptr and num_captured at known offsets.
        let code_ptr =
            unsafe { *(ptr.add(layout::CLOSURE_CODE_PTR_OFFSET as usize) as *const u64) };
        let num_captured =
            unsafe { *(ptr.add(layout::CLOSURE_NUM_CAPTURED_OFFSET as usize) as *const u16) };
        let _ = writeln!(
            stderr,
            "[CASE TRAP] Closure: code_ptr={:#x}, num_captured={}, expected_tags={:?}",
            code_ptr, num_captured, expected
        );
    } else {
        let _ = writeln!(
            stderr,
            "[CASE TRAP] tag_byte={} ({}), expected_tags={:?}",
            tag_byte, tag_name, expected
        );
    }
    let _ = stderr.flush();
    drop(stderr);
    overwrite_runtime_error(RuntimeError::CaseTrap);
    error_poison_ptr()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diagnostics() {
        crate::machine_state::test_support::with_test_machine(|| {
            let _ = drain_diagnostics();
            push_diagnostic("test1".to_string());
            push_diagnostic("test2".to_string());
            let d = drain_diagnostics();
            assert_eq!(d, vec!["test1".to_string(), "test2".to_string()]);
            let d2 = drain_diagnostics();
            assert!(d2.is_empty());
        });
    }

    /// Regression test for the poison-buffer undersize bug.
    ///
    /// Prior to the fix, `runtime_oom` returned a 24-byte poison buffer that
    /// the JIT's slow-fail alloc path then treated as freshly-allocated
    /// scratch. For any Con with `>= 1` field (size `>= 32`) the post-OOM
    /// field write spilled past the 24-byte allocation into adjacent heap,
    /// manifesting as glibc "corrupted size vs. prev_size" aborts.
    ///
    /// The fix enlarges the poison buffer to absorb the maximum Con/Closure
    /// footprint the JIT can emit. This test simulates the JIT's write
    /// sequence directly: allocate a worst-case Con (24 + 1024*8 = 8216
    /// bytes) into the poison and verify no OOB writes occur.
    ///
    /// Under Miri / ASan this would fail before the fix; under glibc the
    /// corruption is non-deterministic, but the write itself is unsound
    /// and the buffer-size assertion below guards against regression.
    #[test]
    fn poison_buf_absorbs_max_con_write() {
        crate::machine_state::test_support::with_test_machine(|| {
            // The read-side decoder cap; the compile-time assertion above
            // guarantees POISON_BUF_SIZE absorbs this. The runtime check here
            // additionally exercises the full write sequence to surface any
            // overflow under Miri / ASan, not just the size relationship.
            use crate::heap_bridge::MAX_FIELDS;
            let worst_case_con = layout::CON_FIELDS_OFFSET as usize + MAX_FIELDS * 8;
            assert!(
                POISON_BUF_SIZE >= worst_case_con,
                "poison buffer ({} B) must cover worst-case Con footprint ({} B)",
                POISON_BUF_SIZE,
                worst_case_con,
            );

            // Simulate the JIT's post-OOM write sequence exactly as
            // `emit_alloc_fast_path` + the Con emitter do: tag at 0, size
            // halfword at 1, CON_TAG at 8, num_fields at 16, fields from 24.
            let ptr = runtime_oom();
            assert!(!ptr.is_null());

            // SAFETY: `ptr` is the poison buffer (POISON_BUF_SIZE >= worst_case_con).
            // Writing a TAG_CON header and MAX_FIELDS u64 field slots into it
            // stays entirely within the allocation after the fix.
            // JIT stores use `MemFlags::trusted()` which permits unaligned
            // access; mirror that with `write_unaligned` so the test also works
            // on targets where a naked deref would trap on misalignment (the
            // size halfword lands at offset 1).
            unsafe {
                ptr.write(layout::TAG_CON);
                (ptr.add(1) as *mut u16).write_unaligned(worst_case_con as u16);
                (ptr.add(layout::CON_TAG_OFFSET as usize) as *mut u64).write_unaligned(7);
                (ptr.add(layout::CON_NUM_FIELDS_OFFSET as usize) as *mut u16)
                    .write_unaligned(MAX_FIELDS as u16);
                for i in 0..MAX_FIELDS {
                    let off = layout::CON_FIELDS_OFFSET as usize + 8 * i;
                    (ptr.add(off) as *mut u64).write_unaligned(0xDEAD_BEEF_0000_0000 | (i as u64));
                }
                // Read back a sentinel to ensure the writes landed (and weren't
                // silently dropped) — also defeats the optimizer.
                let last_off = layout::CON_FIELDS_OFFSET as usize + 8 * (MAX_FIELDS - 1);
                assert_eq!(
                    (ptr.add(last_off) as *const u64).read_unaligned(),
                    0xDEAD_BEEF_0000_0000 | (MAX_FIELDS as u64 - 1),
                );
            }

            // `runtime_oom` sets `RuntimeError::HeapOverflow` — clear it so
            // we don't leak state to other tests sharing this thread.
            let err = take_runtime_error().expect("runtime_oom must flag an error");
            assert!(matches!(err, RuntimeError::HeapOverflow));
        });
    }
}
