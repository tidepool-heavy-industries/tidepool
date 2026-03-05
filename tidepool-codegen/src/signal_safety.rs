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
    use std::ptr::{self, addr_of, addr_of_mut, null_mut};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Pre-computed crash log path, populated once at `install()` time.
    /// Stored as a fixed-size null-terminated byte buffer for async-signal-safety.
    static mut CRASH_LOG_PATH: [u8; 512] = [0u8; 512];
    static CRASH_LOG_PATH_LEN: AtomicUsize = AtomicUsize::new(0);
    static mut CRASH_DIR_PATH: [u8; 512] = [0u8; 512];
    static CRASH_DIR_PATH_LEN: AtomicUsize = AtomicUsize::new(0);

    /// Write a crash dump using only async-signal-safe syscalls.
    /// No allocations, no locks, no std::fs — just raw libc open/write/close.
    unsafe fn write_crash_dump(sig: libc::c_int, info: *mut libc::siginfo_t) {
        let path_len = CRASH_LOG_PATH_LEN.load(Ordering::Relaxed);
        if path_len == 0 {
            return;
        }

        let fd = libc::open(
            addr_of!(CRASH_LOG_PATH) as *const libc::c_char,
            libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
            0o644,
        );
        if fd < 0 {
            return;
        }

        // Write signal info
        let sig_name: &[u8] = match sig {
            libc::SIGILL => b"SIGILL",
            libc::SIGSEGV => b"SIGSEGV",
            libc::SIGBUS => b"SIGBUS",
            libc::SIGTRAP => b"SIGTRAP",
            _ => b"UNKNOWN",
        };

        let mut buf = [0u8; 256];
        let mut pos = 0;

        // "[tidepool-crash] sig="
        let prefix = b"[tidepool-crash] sig=";
        buf[pos..pos + prefix.len()].copy_from_slice(prefix);
        pos += prefix.len();

        buf[pos..pos + sig_name.len()].copy_from_slice(sig_name);
        pos += sig_name.len();

        // " addr="
        let addr_prefix = b" addr=0x";
        buf[pos..pos + addr_prefix.len()].copy_from_slice(addr_prefix);
        pos += addr_prefix.len();

        // Faulting address as hex
        let si_addr = if !info.is_null() {
            (*info).si_addr() as usize
        } else {
            0
        };
        // Write hex digits
        let hex_digits = b"0123456789abcdef";
        let mut hex_buf = [b'0'; 16];
        let mut val = si_addr;
        for i in (0..16).rev() {
            hex_buf[i] = hex_digits[val & 0xf];
            val >>= 4;
        }
        buf[pos..pos + 16].copy_from_slice(&hex_buf);
        pos += 16;

        // " jmpbuf="
        let jmp_prefix = b" jmpbuf=";
        buf[pos..pos + jmp_prefix.len()].copy_from_slice(jmp_prefix);
        pos += jmp_prefix.len();

        let jmp_set = JMP_BUF.with(|cell| !cell.get().is_null());
        if jmp_set {
            buf[pos..pos + 3].copy_from_slice(b"set");
            pos += 3;
        } else {
            buf[pos..pos + 4].copy_from_slice(b"null");
            pos += 4;
        }

        // " ts="
        let ts_prefix = b" ts=";
        buf[pos..pos + ts_prefix.len()].copy_from_slice(ts_prefix);
        pos += ts_prefix.len();

        // Unix timestamp as decimal
        let mut ts = libc::time(ptr::null_mut()) as u64;
        let mut ts_buf = [0u8; 20];
        let mut ts_len = 0;
        if ts == 0 {
            ts_buf[0] = b'0';
            ts_len = 1;
        } else {
            while ts > 0 {
                ts_buf[ts_len] = b'0' + (ts % 10) as u8;
                ts /= 10;
                ts_len += 1;
            }
            ts_buf[..ts_len].reverse();
        }
        buf[pos..pos + ts_len].copy_from_slice(&ts_buf[..ts_len]);
        pos += ts_len;

        buf[pos] = b'\n';
        pos += 1;

        libc::write(fd, buf.as_ptr() as *const libc::c_void, pos);
        libc::close(fd);
    }

    /// Write a simple crash message (for panics in trampoline).
    unsafe fn write_crash_dump_msg(msg: &[u8]) {
        let path_len = CRASH_LOG_PATH_LEN.load(Ordering::Relaxed);
        if path_len == 0 {
            return;
        }

        let fd = libc::open(
            addr_of!(CRASH_LOG_PATH) as *const libc::c_char,
            libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
            0o644,
        );
        if fd < 0 {
            return;
        }

        let prefix = b"[tidepool-crash] ";
        libc::write(fd, prefix.as_ptr() as *const libc::c_void, prefix.len());
        libc::write(fd, msg.as_ptr() as *const libc::c_void, msg.len());
        let nl = b"\n";
        libc::write(fd, nl.as_ptr() as *const libc::c_void, 1);
        libc::close(fd);
    }

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
    }

    /// Trampoline called from C after sigsetjmp returns 0.
    /// Casts userdata back to a `Box<dyn FnOnce()>` and calls it.
    /// Panics are caught to prevent unwinding across the C FFI boundary (which is UB).
    unsafe extern "C" fn trampoline(userdata: *mut libc::c_void) {
        let closure: Box<Box<dyn FnOnce()>> = Box::from_raw(userdata as *mut Box<dyn FnOnce()>);
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            (*closure)();
        }))
        .is_err()
        {
            // Panic crossed into the trampoline. We can't propagate it across C,
            // so abort. The caller (with_signal_protection) already wraps JIT calls
            // in catch_unwind at a higher level, so this should never fire.
            write_crash_dump_msg(b"panic in JIT trampoline");
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

        let val = tidepool_sigsetjmp_call(&mut buf, trampoline, userdata);

        JMP_BUF.with(|cell| cell.set(null_mut()));

        if val != 0 {
            // Signal was caught. Drop the closure that the trampoline never consumed.
            drop(Box::from_raw(userdata as *mut Box<dyn FnOnce()>));
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
        // Not in JIT context — log crash dump, terminate just this thread.
        // Raw syscall SYS_exit (NOT exit_group) terminates only the calling
        // thread at the kernel level. It's async-signal-safe (raw syscall)
        // and avoids pthread_exit's TLS destructor/deadlock issues.
        // The detached eval thread's channel sender drops, which the
        // MCP server handles as "Eval thread crashed".
        unsafe {
            write_crash_dump(sig, _info);
            #[cfg(target_os = "linux")]
            libc::syscall(libc::SYS_exit, 0);
            #[cfg(not(target_os = "linux"))]
            libc::pthread_exit(std::ptr::null_mut());
        }
    }

    /// Install signal handlers for SIGILL, SIGSEGV, SIGBUS on an alternate stack.
    ///
    /// Safe to call multiple times. Uses `sigaltstack` so the handler works even
    /// on stack overflow.
    pub fn install() {
        use std::alloc::{alloc, Layout};
        use std::sync::Once;

        const ALT_STACK_SIZE: usize = 64 * 1024;

        // Pre-compute crash log path once (safe, non-signal context).
        static PATHS_INIT: Once = Once::new();
        PATHS_INIT.call_once(|| {
            if let Ok(cwd) = std::env::current_dir() {
                let cwd_bytes = cwd.as_os_str().as_encoded_bytes();
                let log_suffix = b"/.tidepool/crash.log\0";
                let dir_suffix = b"/.tidepool\0";

                if cwd_bytes.len() + log_suffix.len() < 512 {
                    unsafe {
                        let log_ptr = addr_of_mut!(CRASH_LOG_PATH) as *mut u8;
                        ptr::copy_nonoverlapping(cwd_bytes.as_ptr(), log_ptr, cwd_bytes.len());
                        ptr::copy_nonoverlapping(
                            log_suffix.as_ptr(),
                            log_ptr.add(cwd_bytes.len()),
                            log_suffix.len(),
                        );
                        CRASH_LOG_PATH_LEN
                            .store(cwd_bytes.len() + log_suffix.len() - 1, Ordering::Relaxed);

                        let dir_ptr = addr_of_mut!(CRASH_DIR_PATH) as *mut u8;
                        ptr::copy_nonoverlapping(cwd_bytes.as_ptr(), dir_ptr, cwd_bytes.len());
                        ptr::copy_nonoverlapping(
                            dir_suffix.as_ptr(),
                            dir_ptr.add(cwd_bytes.len()),
                            dir_suffix.len(),
                        );
                        CRASH_DIR_PATH_LEN
                            .store(cwd_bytes.len() + dir_suffix.len() - 1, Ordering::Relaxed);

                        // Ensure .tidepool/ directory exists (safe, non-signal context).
                        libc::mkdir(
                            addr_of!(CRASH_DIR_PATH) as *const libc::c_char,
                            0o755,
                        );
                    }
                }
            }
        });

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
