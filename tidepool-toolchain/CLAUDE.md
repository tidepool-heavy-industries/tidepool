# tidepool-toolchain — locate, validate, fingerprint, and cache the toolchain

**Charter.** Belongs: locating the `tidepool-extract` binary and the Haskell
stdlib it pairs with (`toolchain.rs`), the extract/stdlib deploy handshake
(also `toolchain.rs`), on-disk path resolution (`paths.rs`), the
compiled-artifact cache (`cache.rs`), the ONE policy-bearing compile front
door (`artifacts.rs`), and the structured extract-diagnostics contract
(`diag.rs`, `timing.rs`). Sits between `tidepool-extract-cmd` (the endpoint
and invocation boundary this crate executes through) and `tidepool-runtime` (the
high-level compile/run API and session substrate, which depends on this
crate and re-exports what its own downstream callers still reach through
`tidepool_runtime::` paths — `paths`, `toolchain`, `cache`, `artifacts`,
`diag`, and `timing` are thin module shims there). Does NOT belong: the
session substrate, turn supervision, or anything that dispatches over
`RuntimeError`/`SessionError` — those types live in `tidepool-runtime` and
must not be visible here (this crate sits below it). `failclass.rs`'s
`classify_compile` (a pure `CompileError` decision tree) lives here for that
reason; `classify`/`classify_session` stay in `tidepool-runtime`, calling
back into `classify_compile`.

## Compile cache — two content-addressed layers in `cache.rs`

`cache.rs` holds two keyed memos, both under `paths::compile_cache_dir()`
(`$TIDEPOOL_COMPILE_CACHE_DIR` if set, else the ordinary cache dir — sharable
across test processes since it is content-addressed, not path-addressed):
the original 2-artifact eval layer (`CacheKey` / `cache_load` / `cache_store`,
keyed on source + target + include fingerprint, optionally salted per
session), and an N-artifact invocation layer (`InvocationKey` /
`artifacts_load` / `artifacts_store`) that keys a COMPLETE
`tidepool-extract` invocation — built from the caller's own `ExtractCmd`, so
the key always describes the invocation that actually runs — and stores every
artifact the caller reads (`meta.cbor`, per-target `.cbor`, the asks
sidecar), not just Core. The invocation key is namespaced so it can never
collide with an eval key even though both live under the same `<key>.*`
filenames. `compile_turns` (`tidepool-harness`'s fixed-source boot/answerer/
render compiles, identical across ~200 test processes) is the invocation
layer's consumer; see the doc comments on `invocation_key` for the
allowlisted-argv keying discipline (default-deny — an unrecognized flag makes
the invocation uncacheable rather than silently unkeyed). Every key also
frames the identity reported by the already-bound compiler endpoint, so the
producer named by the key is necessarily the producer that executes. See
`fingerprint_dir_relative` for why include-dir fingerprints are
path-independent (module identity comes from the path relative to the search
root, since Cast/Tick/Type erasure strips source spans before Core reaches
Rust — see root CLAUDE.md's Key Decisions Reference).

Both layers consume one dependency-source manifest covering `.hs`,
`.hs-boot`, `.lhs`, and `.lhs-boot`. The eval layer additionally frames each
include root's absolute path; the invocation layer frames only root-relative
module paths so identical relocated trees share a key. CPP directives make
either layer explicitly uncacheable: `#include` can name files outside every
GHC import root, so an import-directory walk cannot honestly enumerate those
side inputs.

The key builder deliberately lives here and not in `tidepool-extract-cmd`:
that crate owns binding and execution, while this crate owns cache policy.
The key is computed from `ExtractCmd::argv()` plus the opaque
`CompilerIdentity` returned by binding, so it stays invocation-shaped without
reimplementing wrapper, PATH, Nix, daemon, worker, or GHC identity policy.
This crate sits one layer above `tidepool-extract-cmd` because it also needs
Cranelift-adjacent dependencies (`tidepool-codegen`, for
`register_var_names`/`register_poisoned_externals`).

Session-scope compiles (`--inject-val`/`--session-root`,
which read per-session mutable directories) are excluded from the invocation
layer by construction — those flags are not on the allowlist, so such an
invocation keys to `None` and always compiles cold — except for the one
`stable_val` caller path, which additionally accepts `--session-root` (like
`--output-dir`, dropped) and one matching `--inject-val` (dropped, and
separately content-fingerprinted by its `.hi` iface).
