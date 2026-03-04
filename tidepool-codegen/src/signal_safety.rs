//! JIT signal safety via sigsetjmp/siglongjmp.
//!
//! JIT-compiled code can crash with SIGILL (case trap) or SIGSEGV
//! (bad memory access). This module provides `with_signal_protection` which
//! wraps JIT calls so that these signals return a clean error instead of
//! killing the process.
//!
//! The actual sigsetjmp call lives in C (`csrc/sigsetjmp_wrapper.c`) because
//! sigsetjmp is a "returns_twice" function. LLVM requires the `returns_twice`
//! attribute on the caller for correct codegen, but Rust doesn't expose this
//! attribute. Calling sigsetjmp directly from Rust can cause the optimizer to
//! break the second-return path, especially on aarch64.

#[cfg(unix)]
mod inner {
    use std::cell::Cell;
    use std::ptr::{self, null_mut};

    // sigjmp_buf sizes vary by platform:
    //   - Linux x86_64 (glibc): __jmp_buf_tag[1] = 200 bytes
    //   - macOS x86_64: 37 ints + signal mask ≈ 296 bytes
    //   - macOS aarch64: int[49] = 196 bytes
    // Use 512 bytes to cover all platforms with headroom.
    #[repr(C, align(16))]
    pub struct SigJmpBuf {
        _buf: [u8; 512],
    }

    extern "C" {
        fn siglongjmp(env: *mut SigJmpBuf, val: libc::c_int) -> !;

        /// C wrapper: calls sigsetjmp, then callback(userdata) if it returns 0.
        /// Returns 0 on normal completion, or the signal number on siglongjmp.
        fn tidepool_sigsetjmp_call(
            buf: *mut SigJmpBuf,
            callback: unsafe extern "C" fn(*mut libc::c_void),
            userdata: *mut libc::c_void,
        ) -> libc::c_int;
    }

    /// Signal number that caused the jump.
    #[derive(Debug, Clone, Copy)]
    pub struct SignalError(pub i32);

    impl std::fmt::Display for SignalError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            let name = match self.0 {
                libc::SIGILL => "SIGILL (illegal instruction — likely exhausted case branch)",
                libc::SIGSEGV => "SIGSEGV (segmentation fault — likely invalid memory access)",
                libc::SIGBUS => "SIGBUS (bus error)",
                libc::SIGTRAP => "SIGTRAP (trap — likely Cranelift trap instruction)",
                _ => return write!(f, "JIT signal: signal {} (unknown)", self.0),
            };
            write!(f, "JIT signal: {}", name)
        }
    }

    // Thread-local jump buffer pointer. Synchronous signals (SIGILL, SIGSEGV,
    // SIGBUS) are delivered to the faulting thread, so per-thread storage is
    // correct. The `const` initializer avoids any lazy-init allocation, making
    // the thread-local read async-signal-safe in practice.
    thread_local! {
        static JMP_BUF: Cell<*mut SigJmpBuf> = const { Cell::new(ptr::null_mut()) };
        static CLOSURE_PTR: Cell<*mut libc::c_void> = const { Cell::new(ptr::null_mut()) };
    }

    /// Trampoline called from C after sigsetjmp returns 0.
    /// Casts userdata back to a `Box<dyn FnOnce()>` and calls it.
    /// Panics are caught to prevent unwinding across the C FFI boundary (which is UB).
    unsafe extern "C" fn trampoline(userdata: *mut libc::c_void) {
        // Clear the thread-local pointer, as we're about to consume the Box.
        CLOSURE_PTR.with(|cell| cell.set(null_mut()));

        let closure: Box<Box<dyn FnOnce()>> = Box::from_raw(userdata as *mut Box<dyn FnOnce()>);
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            (*closure)();
        }))
        .is_err()
        {
            // Panic crossed into the trampoline. We can't propagate it across C,
            // so abort. The caller (with_signal_protection) already wraps JIT calls
            // in catch_unwind at a higher level, so this should never fire.
            std::process::abort();
        }
    }

    /// Wrap a JIT call with signal protection.
    ///
    /// If SIGILL/SIGSEGV/SIGBUS fires during `f()`, returns `Err(SignalError)`
    /// instead of crashing the process.
    ///
    /// # Safety
    ///
    /// The closure `f` must not hold Rust objects with Drop impls that would be
    /// skipped by siglongjmp. Raw pointers and references are fine.
    pub unsafe fn with_signal_protection<F, R>(f: F) -> Result<R, SignalError>
    where
        F: FnOnce() -> R,
    {
        // We need to pass the closure through C's void* callback interface.
        // Use an UnsafeCell to get the return value out of the type-erased closure.
        let result_cell = std::cell::UnsafeCell::new(None::<R>);
        let result_ptr = &result_cell as *const std::cell::UnsafeCell<Option<R>>;

        let wrapper: Box<dyn FnOnce()> = Box::new(move || {
            let r = f();
            // SAFETY: we're the only writer, and the reader waits until after we return.
            unsafe { *(*result_ptr).get() = Some(r) };
        });

        let mut buf: SigJmpBuf = std::mem::zeroed();

        // Store the jump buffer so the signal handler can find it.
        JMP_BUF.with(|cell| cell.set(&mut buf as *mut SigJmpBuf));

        // Double-box: outer Box for the fat pointer, inner Box<dyn FnOnce()>.
        let boxed: Box<Box<dyn FnOnce()>> = Box::new(wrapper);
        let userdata = Box::into_raw(boxed) as *mut libc::c_void;

        // Store so we can recover on signal.
        CLOSURE_PTR.with(|cell| cell.set(userdata));

        let val = tidepool_sigsetjmp_call(&mut buf, trampoline, userdata);

        JMP_BUF.with(|cell| cell.set(null_mut()));
        let leaked_ptr = CLOSURE_PTR.with(|cell| cell.replace(null_mut()));

        if val != 0 {
            // Signal was caught. The trampoline never ran (or was interrupted),
            // so the boxed closure was not consumed. Drop it to prevent leak.
            if !leaked_ptr.is_null() {
                // SAFETY: userdata was created by Box::into_raw above and was not
                // consumed by the trampoline (signal interrupted it).
                drop(Box::from_raw(leaked_ptr as *mut Box<dyn FnOnce()>));
            }
            return Err(SignalError(val));
        }

        // Closure completed normally.
        Ok(result_cell.into_inner().unwrap())
    }

    extern "C" fn handler(sig: libc::c_int, _info: *mut libc::siginfo_t, _ctx: *mut libc::c_void) {
        // Synchronous signals (SIGILL, SIGSEGV, SIGBUS) are delivered to the
        // faulting thread, so the thread-local read returns this thread's buf.
        let buf = JMP_BUF.with(|cell| cell.get());
        if !buf.is_null() {
            // In JIT context — jump back to sigsetjmp
            unsafe {
                siglongjmp(buf, sig);
            }
        }
        // Not in JIT context — restore default handler and re-raise
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }

    /// Install signal handlers for SIGILL, SIGSEGV, SIGBUS on an alternate stack.
    ///
    /// Safe to call multiple times. Uses `sigaltstack` so the handler works even
    /// on stack overflow.
    pub fn install() {
        use std::alloc::{alloc, Layout};

        const ALT_STACK_SIZE: usize = 64 * 1024;

        // sigaltstack is per-thread, so each calling thread needs its own.
        // Use a thread-local to allocate once per thread and leak (signal
        // stacks must outlive the handler).
        thread_local! {
            static ALT_STACK_INSTALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        }
        ALT_STACK_INSTALLED.with(|installed| {
            if !installed.get() {
                unsafe {
                    let layout = Layout::from_size_align(ALT_STACK_SIZE, 16).unwrap();
                    let alt_stack_ptr = alloc(layout);
                    if alt_stack_ptr.is_null() {
                        return;
                    }

                    let stack = libc::stack_t {
                        ss_sp: alt_stack_ptr as *mut libc::c_void,
                        ss_flags: 0,
                        ss_size: ALT_STACK_SIZE,
                    };
                    libc::sigaltstack(&stack, ptr::null_mut());
                }
                installed.set(true);
            }
        });

        // Always (re)install signal handlers. Other code (Rust panic runtime,
        // test harness) may overwrite them, so we reinstall on every call.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
            sa.sa_sigaction = handler as *const () as usize;
            libc::sigemptyset(&mut sa.sa_mask);

            libc::sigaction(libc::SIGILL, &sa, ptr::null_mut());
            libc::sigaction(libc::SIGSEGV, &sa, ptr::null_mut());
            libc::sigaction(libc::SIGBUS, &sa, ptr::null_mut());
            libc::sigaction(libc::SIGTRAP, &sa, ptr::null_mut());
        }
    }
}

#[cfg(not(unix))]
mod inner {
    #[derive(Debug, Clone, Copy)]
    pub struct SignalError(pub i32);

    impl std::fmt::Display for SignalError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "JIT signal: {}", self.0)
        }
    }

    pub unsafe fn with_signal_protection<F, R>(f: F) -> Result<R, SignalError>
    where
        F: FnOnce() -> R,
    {
        Ok(f())
    }

    pub fn install() {}
}

pub use inner::*;
