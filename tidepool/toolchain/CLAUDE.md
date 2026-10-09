# tidepool-toolchain — locate, validate, fingerprint, and cache the toolchain

**Charter.** Belongs: locating the `tidepool-extract` binary and the Haskell
stdlib it pairs with (`toolchain.rs`), the extract/stdlib deploy handshake
(also `toolchain.rs`), on-disk path resolution (`paths.rs`), the
compiled-artifact cache (`cache.rs`), the ONE policy-bearing compile front
door (`artifacts.rs`), and the structured extract-diagnostics contract
(`diag.rs`, `timing.rs`). Sits between `tidepool-extract-cmd` (the endpoint
and invocation boundary this crate executes through) and `tidepool-runtime` (the
high-level compile/run API and session substrate). Consumers of path,
toolchain, cache and diagnostic modules import `tidepool_toolchain` directly.
The runtime keeps those module imports crate-private and exposes selected
compile APIs, `CompileError` and `PreparedArtifact` at its root. Does NOT belong:
the session substrate, turn supervision, or anything that dispatches over
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

Completed compiler output uses `CompletedSourceEvidence` to retain validated
consumed bytes, import owners and negative resolution witnesses even when
compile-time execution makes replay ineligible. Exact receipts, checked cells
and canonical/native product certification carry that proof without changing
its eligibility flags. Cache publication, source replay recipes and sealed
compile-input reuse keep their stricter eligibility gates. Build actions also
require the declared input closure; completed runtime output does not establish
a hermetic build input proof.
The native Suite corpus integration test uses completed source observations
and the same declared-source-tree guard for its run-owned output. This checks
observed Haskell bytes and import choices without converting compile-time
execution into a replay recipe or a hermetic Buck fixture.

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

Host authentication and private materialization use `host_work` checkpoints in
the owning compiler scope, including between bounded reads and owner-lock polls.
Interruption remains a refusal through optional cache lookups; it cannot admit
physical work as a miss. A partial validation stage stays operation-local and
is discarded on refusal. Once an immutable result is fully validated, a late
stop cannot revoke its retained owner; cancelled consumers still lack publication
permission. Checkpoints are cooperative and do not preempt a syscall or decode.

## Segment original facts

`ExactProgramSegmentAdmission` owns the physical request and consumed-source
witnesses. Its `TPCERT10` segment-original packet is read, decoded and certified
once; native, canonical and recovery custody stays shared. Segment-item packets
carry target global witnesses and package additions, with independent checked
entry, selected type visibility, package demand and executable closure admission.
Available module or site censuses cannot choose an item's execution context.
The initial exact request and that item's selected closure retain that authority.
Ordinary compilation uses the separate closed ordinary packet variant.

Candidate publication occurs once after every item of that segment seals and
rechecks mutable source observations at publication. A failed or cancelled
segment publishes no original suggestions. Whole-program and value publication
keep their existing atomic owners. A fresh admission operation authenticates
physical artifacts afresh; an existing shared owner needs no repeated immutable
byte observation or proof memo.

## Immutable build fixtures

`tidepool_prepared_fixture` in `build/haskell/prepared_fixture.bzl` invokes
`tidepool-toolchain`'s `prepared-fixture` binary through `build_prepared_fixture`.
One action compiles one module and its complete ordered target set against a
pinned compiler deployment/package closure and explicit source directory trees.
The CLI binds the configured frontend directly, clears inherited compiler and
package selection, and uses private scratch as its current directory. Admission
checks complete source evidence against the declared trees before export.
The GHC compilation owner replaces package-database flags with the libdir's
pinned global database before loading packages. User databases and ambient
`GHC_PACKAGE_PATH` entries cannot add packages to direct, resident or corpus
compilation. Native oracle actions select that same global database explicitly.

Build actions never read or publish the runtime memo, module-candidate store,
deployment catalog or runtime build-products directory. Production readers
validate every requested prepared program, typed-site sidecar, metadata and
product certificate before any portable output is exported. The directory
contains `meta.cbor`, `<target>.prepared.cbor` and `<target>.asks.json` for every
target. `PreparedFixtureInfo` exposes that directory and the target artifacts.
Test runners supply it as a runtime resource. Source-bound certificates remain
at the original compilation paths and are discarded with action scratch.
The fixture CLI uses `BUCK_SCRATCH_PATH` for action scratch when supplied,
resolving relative paths from the launch directory before changing directories.
Standalone invocation defaults to that launch directory.
Failed fixture CLI actions retain their scratch and name it in stderr. The
existing failure-artifact owner preserves the raw compiler report, stderr,
status and request; the CLI prints every typed diagnostic and source span.
These files are diagnostic observations and cannot hydrate fixture authority.

## Deployment catalog production

Production entries reserve an exclusive `<entry>.preparing` container beneath
the caller's durable output parent before compiler execution. Raw outputs,
stdout, stderr and status remain there after uncertain submission or sealing failure.
Sealing validates the complete original through the shared loader, syncs it,
and renames that same container to the ready path without copying artifacts.
An unfinished identity with uncertain or completed submission refuses
recompilation; another preparation requires a fresh identity. A typed proven
zero-submission refusal may release only its own unused reservation, syncing
the parent. A retry confirms the absent name before reserving it again.
Runtime source owners retain the UUID parent. Build-action
owners control their output-tree cleanup, so a fresh Buck action tree has fresh
preparation custody rather than recovering a removed original.

`build_deployment_module_package` uses that same build-action compiler policy,
with authenticated catalog export instead of portable fixture export. Its caller
supplies the snapshot’s `TidepoolCatalog.hs` probe, ordered targets, retained
snapshot root, private current-directory scratch and absent output directory. Complete worker evidence and the canonical
module-product owner admit the closed source cohort before catalog export.
Preparing those catalog records grants no runtime candidate publication.

`tidepool-module-package build` receives `--source`, repeated `--target`,
`--source-root` and `--output-root`. The build action supplies the configured
frontend, worker, compiler deployment manifest and GHC libdir. The CLI removes
inherited resident/cache selection and contains temporary files in its own
scratch. Schema 4 records `source_selection` with the canonical retained snapshot,
ordered `StableEffects`, `Stdlib`, `Actors`, and `Jev` roles, and the complete
Haskell source manifest including the probe. These roles resolve to `effects`,
`lib`, `actors`, and `jev/core`; each directory must exist, without source aliases.
The worker include list must equal these roots in order, and every catalog source
and home dependency must belong to their union. Source selection is checked again
after compilation before exporting compiler-issued evidence unchanged. Product
references are relative to the opened catalog's canonical parent, so the complete container can move without rewriting its bytes. Source
paths cannot move or alias other paths. The source guard remains separate from
qualification's actual Nix registration, NAR and GC-root checks. Earlier catalogs
are rejected and must be regenerated through the matched producer.

`configured_module_source_selection` shares catalog schema, compiler authority,
source manifest and alias validation without hydrating native products. The
configured package owner retains only its current exact catalog path and complete
compiler authority selection. Initial loading validates every proof and the
complete cohort; reuse reauthenticates catalog, native artifacts, canonical
companions and source observations, sharing the admitted decoded products.
Deployment records share a dependency proof only after each physical evidence
file authenticates the exact SHA-256 and length. Each fresh catalog or candidate
validation stage walks that shared proof and generated input once, including
negative import witnesses; a later stage observes the filesystem again.
Candidate acquisition still validates its current source/import witnesses and
exact context. Read-only bundle permissions do not replace content checks.
Actual Nix registration, NAR, retention and final bundle qualification remain the
qualification owner’s independent checks.

`tidepool-module-package inspect` uses the same declared snapshot, probe, targets,
direct configured endpoint and build-action source guards as `build`. It writes
`catalog-inventory.json` only after actual product certification, with canonical
interfaces and optional Core, native owners, native group counts, and each
worker module’s `ProductAvailability`. It publishes no catalog or candidates.
`request.json`, `invocation.json`, and `outcome.json` preserve source and producer
identity, the ordered roots, and refusal stage. Both CLI scratch and the recursive
compiler transaction under `output-root/raw` are retained, including stdout,
stderr, build products and dependency evidence. Raw worker flags in a refusal
report remain explicitly unadmitted; missing canonical counts are not zeroes.
Inspection has no internal wall timeout; its execution owner supplies one.

## Execution failure diagnostics

The isolated libtest runner's `--output-dir` owns one fresh diagnostic root per
case and explicitly sets `TIDEPOOL_TEST_DIAGNOSTIC_SCOPE=1`. Only that mode keeps
original compiler scratch under the case root after successful compilation,
through later native execution and case settlement. Requests are recorded before
compiler execution, so timeout or hard termination preserves pending inputs.
Ordinary production and build-action scratch retain their normal cleanup.

`CompilerDiagnosticCapture` records bounded hashes and consumed-source copies
before consumers execute; raw compiler products stay at their original paths.
The hash walk covers at most 128 MiB, 4096 entries and depth 16; the existing
source diagnostic owner separately bounds copied sources to 128 MiB. Reports
mark omissions. Original outputs are the workload's compiler outputs rather
than duplicate success copies, and are retained intact until the consuming case
settles. Diagnostics neither authorize imports nor relocate certificates.

Only the runner removes successful case evidence, after process/service cleanup
and any hosted/owned-compiler cleanup reports are confirmed. Failure, timeout,
interruption and unknown cleanup retain the case directory and its report.
