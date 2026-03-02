/* sigsetjmp/siglongjmp wrapper for Rust FFI.
 *
 * sigsetjmp is a "returns_twice" function — it returns once normally (0)
 * and again when siglongjmp jumps back to it (with the signal number).
 * LLVM requires the `returns_twice` attribute on the caller for correct
 * codegen, but Rust doesn't expose this attribute. Calling sigsetjmp
 * directly from Rust can cause the optimizer to break the second-return
 * path, especially on aarch64.
 *
 * Solution: call sigsetjmp from C where the compiler handles it correctly,
 * and export a thin wrapper to Rust.
 */

#include <setjmp.h>
#include <signal.h>
#include <stdint.h>

/* Run a callback with sigsetjmp/siglongjmp protection.
 *
 * Returns 0 if the callback completed normally.
 * Returns the signal number (> 0) if siglongjmp was called from a handler.
 *
 * jmp_buf_out: filled with the sigjmp_buf pointer so the signal handler
 *              can call siglongjmp on it.
 */
int tidepool_sigsetjmp_call(
    sigjmp_buf *buf_out,
    void (*callback)(void *),
    void *userdata
) {
    int val = sigsetjmp(*buf_out, 1);
    if (val != 0) {
        return val;
    }
    callback(userdata);
    return 0;
}
