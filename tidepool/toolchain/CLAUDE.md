# tidepool-toolchain — locate, validate, fingerprint, and cache the toolchain

**Charter.** Belongs: locating the `tidepool-extract` binary and the Haskell
stdlib it pairs with (`toolchain.rs`), the extract/stdlib deploy handshake
(also `toolchain.rs`), on-disk path resolution (`paths.rs`), the
source and dependency evidence (`cache.rs`), the ONE policy-bearing compile front
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

## Compilation and reuse

`artifacts::compile_invocation` is the single policy-bearing compile front door.
It submits each request to the configured compiler and acquires its complete named
physical output through `InvocationOutputLayout`. Single-target and multi-target
layouts derive required metadata, prepared programs, typed-site sidecars, native
module products and paired package provenance from the ordered request targets.
Duplicate targets or unsafe generated names refuse before worker submission.
The existing inventory operation bounds physical reads and downstream decoding;
missing members and inaccessible members retain distinct failures. Cancellation
is a refusal and does not authorize worker submission.

The resident compiler owns dependency-validated module reuse. Native module
candidates, deployment catalogs and retained entries keep their own certified
reuse boundaries. There is no separate Rust invocation-output memo: a repeated
request enters the compiler even when an unusual zero-product result could have
been reused without it. Matched measurements own any performance claim.

Runtime, exact, authored and retained-entry policies carry a borrowed caller-owned
compiler settlement recipient. It retains independent close evidence through
operation unwind; each known-unsubmitted retry forwards another affine observation
to the same recipient. Actual execution transfers selected sealed input files or owned lookup directories to
the endpoint transaction owner. Cell parsing, declaration joins and original interface
inventory require the same settlement recipient. Path-based join inputs must belong
to the supplied owned directory; its custody follows uncertain close evidence. Keep `CompilerCloseUnconfirmed` and its completed
action typed, and never replay it. Declared build actions use their file-only
isolated invocation and do not acquire owned request transport.

The worker emits versioned `dependencies.json` with consumed SHA-256 source
evidence, selected home modules and absent higher-priority import candidates.
Cache safety and test-selection completeness are separate fields. Source and
negative resolution witnesses are validated by their semantic consumer before
reuse or publication; package selection requires its owning compiler proof.

Completed compiler output uses `CompletedSourceEvidence` to retain validated
consumed bytes, import owners and negative resolution witnesses even when
compile-time execution makes replay ineligible. Exact receipts, checked cells
and canonical/native product certification carry that proof without changing
its eligibility flags. Cache publication, source replay recipes and sealed
compile-input reuse keep their stricter eligibility gates. Build actions also
require the declared input closure; completed runtime output does not establish
a hermetic build input proof.
Prepared-fixture and completed-corpus source admission share the caller's
`InventoryOperation`. The private parsed observation owner charges the existing
length-times-64 decode allowance before serde, then lends structural facts to
source-closure checks and consumes them through `CompletedSourceEvidence`.
Parsed facts alone do not authenticate consumed bytes or negative witnesses.
Build-action admission authenticates one completed physical-input/dependency
observation, then checks captured invocation text against its generated-source
witness. These adjacent checks do not issue a later filesystem observation;
exact receipts, later certification and standalone corpus validation retain
their independent authentication boundaries.
Standalone corpus validation creates one operation at its action boundary rather
than resetting a budget for each validation.

The native Suite corpus integration test uses completed source observations
and the same declared-source-tree guard for its run-owned output. This checks
observed Haskell bytes and import choices without converting compile-time
execution into a replay recipe or a hermetic Buck fixture.

`source_root_manifest` and `source_roots_identity` still own whole-source-revision
identities for workspace capture and reload. Those identities describe a source
snapshot; they do not determine which files a compiled program consumed.

Host authentication and private materialization use `host_work` checkpoints in
the owning compiler scope, including between bounded reads and owner-lock polls.
Interruption remains a refusal through optional reuse lookups; it cannot admit
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

Offer application consumes the prepared offer and final placed request, capturing
its complete distinct nonzero Val-owner selection before execution. The applied
owner keeps that immutable request and required selection through certification;
program item derivatives retain the same original selection. Optional candidate
search limits cannot truncate a complete selection. Retained original-entry
certification obtains an explicit empty selection only from its validated format,
which excludes resident injection. Preparation refusals retain request evidence
without claiming compiler submission, stderr or a worker outcome.

The physical cell request captures produced value interfaces once. Its private
inventory may contain later outputs; item views select current and preceding type
outputs by ordinal, while runtime value authority comes only from completed prior
items. Complete new-product native offers are issued from that physical custody
before item type filtering, then carried unchanged into the item selections.
Final value capture adds source/template authority to the same owned bytes
and interface certificate. Sparse generation numbers do not determine visibility.

A bound segment retains its physical compiler selection. Its cursor owns the
admitted prefix and executable demand; item admission stages support and value
results before advancing both. Compiler receipt, binder, table and target checks
belong to `PreparedCellProgram`. Contextual sealing consumes those observations
after physical compiler close and preserves independent host cancellation.

Candidate publication starts once after every item of the segment seals and
rechecks mutable source observations. Cancellation refuses program handoff and
stops remaining writes; completed atomic candidate records remain suggestions.
Whole-program and value publication keep their existing atomic owners. A fresh admission operation authenticates
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
Fixture requests select typed portable output. The worker closes reachable home
bindings through its existing prepared recovery and rejects residual home-unit
imports before writing programs. Ordinary extraction retains native-original
boundaries; portable output cannot carry resident, candidate or exact inputs.
The GHC compilation owner replaces package-database flags with the libdir's
pinned global database before loading packages. User databases and ambient
`GHC_PACKAGE_PATH` entries cannot add packages to direct, resident or corpus
compilation. Native oracle actions select that same global database explicitly.

Build actions never read or publish the runtime module-candidate store,
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
acquired package is retained explicitly by its workspace/session and cell admission.
New acquisition validates the catalog, every artifact, canonical companion and
source observation. Borrowing uses the captured original bytes and single decoded
products until release or reload; it checks current source/import witnesses,
package interfaces and exact declaration compatibility without reauthenticating
the static acquired image. Private compiler paths are materialized once by that
owner, and certified originals retain their custody after a run is released.
Deployment records share a dependency proof only after every distinct physical
evidence file authenticates its exact SHA-256 and length. Each fresh source
validation stage walks shared proofs once, including negative import witnesses.
Read-only bundle permissions do not replace new-acquisition checks.
Actual Nix registration, NAR, retention and final bundle qualification remain the
qualification owner’s independent checks.

Artifact identity compares durable certified bytes. A fresh native source
admission witness and acquired input custody belong to the issuing view, not the
shared semantic vertex. Independent admission retains its own witness; a join
keeps the receiving view's witness while combining compatible custody. Sparse
view overrides include explicit absent witnesses when needed to mask another
parent's proof. Detached selection retains those exact overrides. Tests must
cross source-witness differences with acquired custody, rather than treating
outer artifact equality as equality of issuing authority.

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

## Native handoff acceptance

Native availability, executable demand and retained graph custody are distinct.
A new complete product receives its dependency closure from the inventory owner
using the compiler request's exact issued availability. Carry that evidence into
issuance before validation; do not attach it afterward or reconstruct it from
all retained artifacts. Temporary private issuance must restore the caller's
namespace and executable roots while retaining the exact dependency bindings.
Adding issued offer custody preserves the receiving namespace exactly. Historical
canonical interfaces may coexist in separate authenticated scopes; uniqueness
applies to selected compiler roles, not every retained payload. Compiler input
joins carry each surviving role's issuer binding rather than resolving equal
payload IDs against the receiving inventory.

Resolve carried selections through the owner's preferred-input path before
deriving fallbacks from current executable roots. Classify a supplied proof using
its own carrier and group graph against the selected canonical binding for that
request. Broader
custody cannot supply missing groups or veto an exact proof. Ambient discovery
belongs only to the absence of a supplied or selected proof.

`CompilerSourceSupport` carries issued roles, graph custody and immutable native
offers together through context publication and retained values. Its fields and
constructor are private; domain operations carry selected issuer bindings rather
than accepting independently reconstructed projections and views.
`OriginalCompilerInputs` is a separate baseline-dependent private contribution:
authenticate its own scoped role associations at issuance, then close its type
dependencies against the receiving context and protected requirements at request
construction. A missing baseline role cannot be supplied by private custody.
Activation reuses carried offers unchanged; fresh complete issuance belongs to
checked candidates or certified physical outputs. Narrowing type
or execution selection does not reissue body availability. Requests consume
carried offers; they do not census broader context custody. Context snapshot v3
records exact offer selections and Empty/Partial/Full mode. Reopening authenticates
those references and preserves the recorded mode, even when a partial graph can
now qualify as complete. Recorded Full validates its complete exact carrier and
canonical edge in its issuing graph, including proofs with no compiler namespace.
Current request compatibility remains a separate check after restoration.
Older snapshots require regeneration.

Compiler transport combines authenticated body availability while retaining each
offer's exact issued graph. Equal payload IDs and ordinals can belong to distinct
issuance scopes; transport neither selects nor collapses those bindings. Full
availability requires an individual offer's complete proof. Combining issued
subsets cannot create Full. The inventory owns execution selection through exact
root and carrier choices, and refuses any remaining demand ambiguity.
Target admission uses exact offered graph candidates before explicit execution
root qualification. Retained custody cannot fill an empty or partial request
offer, including dependency-only originals absent from its compiler namespace.
Fresh product bodies and explicit issued Group roots retain their own authority;
absence of a request offer is not permission to recover one from custody.

Changes here need a history crossing offer, dependent-product admission, private
handoff and subsequent compilation. Carry independently issued overlapping
closures through transport, source selection and target admission. Cover equal
payloads with distinct carrier or group scopes, repeated proofs, reversed joins,
Full with no demand, and divergent carried availability and selected demand.
Check exact scoped execution and refusal of unqualified ambiguous demand,
unavailable dependencies and conflicting selected products or interfaces. Run
the owning toolchain target and a real actor/compiler consumer on the join.

Fixture imports read exact binder identities from authenticated original-group
definitions. Ordinals and naming patterns do not issue binder identity.

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
