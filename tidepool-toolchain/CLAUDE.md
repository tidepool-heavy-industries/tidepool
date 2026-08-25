# tidepool-toolchain — locate, validate, fingerprint, and cache the toolchain

**Charter.** Belongs: locating the `tidepool-extract` binary and the Haskell
stdlib it pairs with (`toolchain.rs`), the extract/stdlib deploy handshake
(also `toolchain.rs`), on-disk path resolution (`paths.rs`), the
compiled-artifact cache (`cache.rs`), the ONE policy-bearing compile front
door (`artifacts.rs`), and the structured extract-diagnostics contract
(`diag.rs`, `timing.rs`). Sits between `tidepool-extract-cmd` (the zero-dep
invocation builder this crate spawns through) and `tidepool-runtime` (the
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
the invocation uncacheable rather than silently unkeyed) and on
`fingerprint_dir_relative` for why include-dir fingerprints are
path-independent (module identity comes from the path relative to the search
root, since Cast/Tick/Type erasure strips source spans before Core reaches
Rust — see root CLAUDE.md's Key Decisions Reference).

The key builder deliberately lives here and not in `tidepool-extract-cmd`
(the crate that actually builds/spawns `ExtractCmd`): that crate is a
zero-dependency std-only leaf so `tidepool-macro` can depend on it without
dragging Cranelift and blake3 into every crate that expands
`haskell_eval!`/`haskell_inline!`. The key is nonetheless computed FROM
`ExtractCmd::argv()`, so the builder stays invocation-shaped and can move
down a crate later without a redesign if `tidepool-extract-cmd` ever grows
deps. This crate itself sits one layer above `tidepool-extract-cmd` for the
same reason `tidepool-runtime` used to: it needs Cranelift-adjacent deps
(`tidepool-codegen`, for `binary_content_hash`'s memo and
`register_var_names`/`register_poisoned_externals`) that the invocation
builder must stay free of.

Session-scope compiles (`--session-bind`/`--inject-val`/`--session-root`,
which read per-session mutable directories) are excluded from the invocation
layer by construction — those flags are not on the allowlist, so such an
invocation keys to `None` and always compiles cold — except for the one
`stable_val` caller path, which additionally accepts `--session-root` (like
`--output-dir`, dropped) and one matching `--inject-val` (dropped, and
separately content-fingerprinted by its `.hi` iface).
