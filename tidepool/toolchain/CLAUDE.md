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

## Compile cache

`artifacts::compile_invocation` is the single compile front door. Immutable
single-target and multi-target requests share one recipe and named artifact
bundle in `cache.rs`. Session salts and injected session interfaces bypass the
Rust artifact cache. The resident compiler daemon has a separate,
dependency-validated module memo that can reuse immutable support across those
requests. Matched measurement tests are the evidence for costs and savings.

The recipe binds source bytes, the generated module filename, ordered targets
and absolute import roots, and the bound compiler's producer identity. Unknown
options are uncacheable. A recipe lookup does not scan entire import trees.
Instead, the worker emits versioned `dependencies.json` with SHA-256 source
evidence, selected home modules and absent higher-priority import candidates.
Package dependencies belong to the producer identity. Incomplete evidence (including
untracked preprocessing or request-time execution) cannot produce a hit.
Cache safety and test-selection completeness are separate fields.

Before publication and on each lookup, validate consumed source bytes and
negative resolution witnesses. The generated request path is normalized to a
logical source marker; authored dependencies retain absolute path identity.
Metadata, prepared programs, typed-site sidecars, and dependency evidence live
in one checksummed bundle, published by atomic rename. A malformed bundle or
incompatible evidence is a miss. The recipe namespace deliberately invalidates
the former eval and invocation cache layouts; neither is read or written.

`source_root_manifest` and `source_roots_identity` still own whole-source-revision
identities for workspace capture and reload. Those identities describe a source
snapshot; they do not determine which files a compiled program consumed.
