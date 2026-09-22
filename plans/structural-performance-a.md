# Structural performance A: implementation ledger

The user authorized a broad structural sweep after the first STG performance
wave, including adjacent low-risk simplifications and Rust compile/test costs.
Baseline: `57aa159cf`. Historical timings in `stg-specialization-followup.md`
are not a matched post-JSON baseline. Report scopes separately; do not sum
category savings with defining-module savings.

## Accepted core

1. Eliminate full GHC interfaces for leaf targets only after proving no
   downstream compilation, metadata query, recovery, or TH consumer needs them.
2. Keep immutable Tidepool support in the existing content-addressed source,
   artifact, export, and validity owners. The attempted package manifest and
   staging layer did not remain the supported boundary and has been retired;
   mutable workspace, effect-row shims, and session generations remain outside
   immutable source ownership.
3. Compile expression display alongside the expression. Pass runtime budgets and
   presented keys as values; preserve observation publication before display
   failure, execute-once behavior, lexical aliases, and partial commits.
4. Mount host Text/JSON through typed managed construction and payload-independent
   binder interfaces instead of embedding payloads in generated Haskell source.
5. Compact temporary root storage and active-root enumeration. Explicitly
   distinguish consuming tree construction from reusable DAG nodes; preserve
   nested forcing, cancellation, and failure cleanup.
6. Share indexed static-region ownership across collection and observation,
   preserving exact object-start/tag validation, rollback, and retirement.

One-cell execution bundles are a bounded design investigation, not an automatic
implementation commitment. Cross-machine native templates and persistent native
caching require a separate decision. Do not prune escaping callables from local
call-site evidence.

## Adjacent parcels

- Remove test-only AnswerPlan/build_answer and unused native fault-recovery C
  machinery; retain production error and response-depth contracts.
- Finish opt-in interface wall/CPU/RTS attribution and restore the interrupted
  measurement harness. Missing attribution is explicit, not zero.
- Unify embedded prepared-fixture inventory and migration checks. Embedded artifacts must be registered and regenerated from their producers,
  not header edits; corpus and embedded validation run together.
- Remove deep artifact/program copies at consuming ownership boundaries.
- Audit unused mutable-turn evidence/sidecars before removing their production.
- Remove JSON guard placeholder trees, bigint intermediate values, repeated
  observation bitmap/region copies, and eager observation-root reservation.
- Audit linear constructor lookup in structural response validation.
- Measure repeated actor-spec preparation before considering immutable artifact
  reuse. Any later design must use fresh actor instances and exact
  source/import/effect/toolchain identity; never share a mutable live handle.
- Prepare fixed root/spec test artifacts once with compiler dependency evidence,
  preserving fresh machines and separate crash/signal/global-state processes.
- Narrow affected Cargo checks by dependency kind and target kind. Dev consumers
  still compile where affected; they must not propagate as production edges.
- Restrict suite prebuilds to selected integration targets; reuse the existing
  daemon owner for default checks; avoid compiler startup for Rust-only checks.
- Remove test-support dependency edges where possible and isolate shipped asset
  changes from central crate code. Moving helpers without shrinking the Cargo
  graph is not a completed build-time optimization.

## Delivery and evidence

### Notebook cost profile: 2026-09-22 UTC

A quiet run of the frozen baseline (prebuilt binaries, one resident daemon,
other build jobs paused) measured two recurring one-statement cells at 29.943s
and 29.856s, and two six-statement cells at 66.766s and 67.339s. This is a
baseline attribution, not a before/after speedup. The measurement test passed.
Raw compiler/client logs and machine-readable phase totals are retained under
`target/structural-performance-a/compile-profile/`.

Mean attribution for the recurring one-statement cells:

| Work | Seconds | Share of cell wall time |
| --- | ---: | ---: |
| GHC desugaring and Core optimization | 23.675 | 79.2% |
| Prepared STG production | 2.319 | 7.8% |
| Parsing/typechecking | 1.965 | 6.6% |
| Prepared projection and recovery | 0.727 | 2.4% |
| Prepared tidy | 0.411 | 1.4% |
| Native compilation | 0.122 | 0.4% |
| Module interfaces | 0.114 | 0.4% |
| Remaining setup, transport, execution, and unattributed time | 0.567 | 1.9% |
| Total | 29.900 | 100% |

These are disjoint phase totals; request totals and module breakdowns are
alternative views and must not be added to the table. Per-phase millisecond
rounding limits precision. The first-cell phase block also contains bootstrap
and must not be compared directly with its cell-only wall counter. The external
`time` process does not own the sibling daemon, so its CPU/RSS statistics do not
measure compiler CPU/RSS. Full lowering allocation/GC attribution remains open.

Each one-statement cell issues five compiler requests: cell preflight, expression,
display-page construction, display metadata, and display alias publication.
The four executable requests each take roughly 7–7.6s. Six-statement cells
issue ten requests. Lookup alone takes 41ms, including one compiler request.

The compiler log repeatedly reports `Tidepool.Effects.Core` as non-reusable due
to `untracked-compile-time-execution`, cascading into dependent-module misses.
The generated module inherits `QuasiQuotes` from the authored-cell dialect even
though its generated static definitions need no quasiquotation. A representative
warm request spends 3.307s in Effects.Core, versus 37ms in its Expr module;
Contract, Actors.Unfold, and Command take 372ms, 363ms, and 296ms respectively.
These module numbers are subsets of the phase totals above.

The historical priority was therefore (1) remove unnecessary quasiquotation
from generated support only, retaining conservative validation and authored
language behavior, then measure actual reuse; (2) fuse display preparation to
remove whole compiler requests while preserving fresh lexical identities and
failure publication; and (3) reassess immutable-support packaging against the
remaining measured cost. The first two changes removed the measured need for a
separate package boundary, and the later staging implementation was retired.
Native lookup/interface micro-optimizations were not the primary notebook
latency work. Reuse and request removal overlap: measure the combined result
rather than adding their projected savings.

The generated-support correction landed in `b5f094270`. Against the same frozen
baseline workload, two current recurring one-statement cells measured 5.017s
and 5.021s (29.900s baseline mean to 5.019s current mean, 83.2% lower wall
time). Two six-statement cells measured 11.055s and 11.118s (67.053s to
11.087s, 83.5% lower). One-statement lowering fell from 23.675s to 2.864s;
prepared STG from 2.319s to 0.305s; and typechecking from 1.965s to 0.354s.
Native compilation remained approximately 0.12s, as expected. This is a
matched structural speedup for the measured notebook workload, not a general
throughput claim.

The generated-support run completed every cell measurement but its final
standalone `lookup map` probe failed after checked compilation. Investigation
found that metadata-only interface elision had left lookup's generated row
sentinel on an HPT lookup path; the exact checked `Id` must travel with the
other immediate inspection probes. The cell results precede and do not depend
on that probe, but the measurement test is not considered passing until the
lookup regression is repaired and rerun.

After generated Core became reusable, `Tidepool.Worktree` was the remaining
repeated `untracked-compile-time-execution` miss and invalidated six actor
modules. Its blanket `QuasiQuotes` extension is unused; `Tidepool.Event` has
the same latent issue. Both are being narrowed and will be measured only after
the facade's embedded source bundle is rebuilt, since editing the repository
source alone does not change an already-built test binary.

The complete static-source correction landed in `c00b7797a`: Worktree, Event,
and the generated library-isolation probe omit quasiquotation, while authored
cells and declaration templates retain it. After rebuilding the embedded
source bundle, the full matched measurement passed, including lookup:

| Workload | Frozen baseline | Current | Wall-time reduction |
| --- | ---: | ---: | ---: |
| First measured one-statement cell | 30.205s | 5.180s | 82.9% |
| Recurring one-statement cell, mean of two | 29.900s | 1.504s | 95.0% |
| Recurring six-statement cell, mean of two | 67.053s | 3.044s | 95.5% |
| Lookup | 41ms | 32ms | 22.0% |

In the final recurring one-statement cells, mean GHC typechecking was 45ms,
lowering 12ms, prepared STG 4ms, and native compilation 121ms. The remaining
compiler-side cost is primarily prepared projection/recovery (mean 971ms,
with one 1.119s projection outlier) and the four executable requests still
made by each cell. This changes the next priority: display fusion removes
whole requests and their projection/recovery work. Building a separate stable
support package no longer attacks the measured dominant phase and stays held
until the post-fusion profile demonstrates otherwise.

The final compiler log has no recurring Worktree or Effects.Core untracked
execution miss. Each appears during cold dependency population; Worktree has
one additional `executable-body-not-prepared` miss when first demanded, which
is the intended separation between stable dependency facts and on-demand
prepared bodies. Lookup's row-sentinel fix is `cfc7df9b0`; the final lookup
probe passed.

Display fusion landed in `a69ef8216` and the same matched measurement passed.
The page, strict metadata tuple, and fresh `cellDisplay` identity now compile
as one three-binder request; failure coverage proves that metadata failure
keeps the captured value but publishes no new alias. Results:

| Workload | Before display fusion | Fused display | Additional reduction |
| --- | ---: | ---: | ---: |
| First measured one-statement cell | 5.180s | 4.609s | 11.0% |
| Recurring one-statement cell, mean of two | 1.504s | 0.705s | 53.1% |
| Recurring six-statement cell, mean of two | 3.044s | 2.826s | 7.2% |
| Lookup | 32ms | 38ms | noise-scale change |

The recurring one-statement cell now makes three compiler requests instead of
five. The six-statement cell makes eight instead of ten because only its final
expression has a display; six separately compiled execution items remain. The
fused final machine has 14,287 native functions, 4,963,201 native-code bytes,
and 13 installed programs, versus 14,617 functions, 5,033,170 bytes, and 14
programs before fusion. This is 330 fewer functions (2.3%), about 70 KB less
native code (1.4%), and one fewer live program for the measured sequence.

Against the original frozen baseline, recurring one-statement latency is now
97.6% lower (29.900s to 0.705s) and recurring six-statement latency is 95.8%
lower (67.053s to 2.826s). Do not add these end-to-end reductions to the
intermediate generated-support or display percentages; they are nested views
of the same workload. Raw evidence is retained under
`target/structural-performance-a/compile-profile-display/`.

The final integrated revision repeated the same matched workload after host
authority, constructor cleanup, and dependency pruning. It passed at 4.607s
first cell, 0.689s and 0.684s recurring one-statement cells (0.687s mean),
2.780s and 2.760s recurring six-statement cells (2.770s mean), and 33ms lookup,
with the same 3/8/1 compiler-request counts. These small improvements over the
display-fusion run are noise-scale confirmation that the closing work did not
regress the hot path, not an additional speedup claim. Final state was 14,299
native functions, 4,970,777 native-code bytes, and 13 programs. Evidence is
under `target/structural-performance-a/compile-profile-final/`.

The remaining warm request shape is now explicit. Ordinary executable requests
still spend about 240--270ms in the compiler endpoint, dominated by prepared
projection/recovery rather than GHC lowering. The fused display program also
adds roughly 100ms of native compilation and about 187 KB of generated code per
cell. One six-statement run had a 927ms request outlier, and its linear request
count now dominates its wall time. These measurements make reusable display
execution and a checked-cell execution bundle concrete next-wave questions.

Use a Sol integration lead and at most three children concurrently: Luna for
bounded cleanup/fixtures/scripts, Terra for substantive implementation, and
independent Sol review for consequential compiler and GC changes. Use
isolated file ownership and concrete commits. Independently review consequential
compiler and GC changes. Establish shared contracts before parallel consumers.
No broad batteries in children; root owns integration corpus checks.

For each parcel record: commit, obsolete path removed, focused behavior checks,
changed-target compilation, remaining gaps, and measured or structural savings.
Preserve failure cases when deleting tests. Format changed languages and run
git diff --check. Run fixtures-check once at translation/schema integration and
also check all registered embedded prepared fixtures. Do not routinely run
the hours-long just verify.

Matched workloads: cold/bootstrap, warm one/six-statement cells, child activation,
unchanged lookup, large host values/JSON, long-lived installation/retirement,
and clean/incremental Rust build, link, selected-test and fixture preparation.
Record requests/stages, CPU/allocation/GC, native functions/bytes, live versus
cumulative memory, root/probe counts, fixture preparations, and physical versus
apparent disk use. Prebuild before runtime timing; do not clear shared caches.

## Next Astra review: live questions

Carry these forward with the final measurements and resolved bug list; prune
questions that this implementation closes before handoff.

- After display-request fusion, what fraction of warm cell latency remains in
  prepared projection/recovery, native compilation, execution, and transport?
  Is there another structural reuse boundary, or would further work optimize
  hundreds of milliseconds with disproportionate compiler risk?
- Does the fully fused callable display ABI earn its extra fresh-alias/mount
  machinery after the safe multi-bind fusion is measured? Current evidence says
  it could remove one roughly 260ms compiler request, about 100ms of native
  compilation, and roughly 187 KB of generated code from each displayed cell;
  validate a design that preserves dynamic keys/budgets and partial commits.
- Does a stable support package still remove meaningful work now that recurring
  typechecking/lowering/prepared-STG total about 60ms per one-statement cell?
  Require a post-fusion profile before reviving it.
- Are every host Text/JSON payload, actor reply, request update, and generated
  effect response on the one visitor/incremental-builder boundary? Which eager
  snapshots remain real consumers of `to_value`?
- Can immutable actor-spec compilation be reused under exact source/import/
  effect/toolchain identity while every actor still receives fresh machine and
  live-handle state? Is repeated spec preparation still visible after compiler
  reuse?
- Native state reaches roughly 14,299 functions and 5.0MB cumulative code after
  the measured sequence. Which retained functions/programs are still live and
  demanded, and which exist only through broad exports, adapters, display
  requests, or retired session generations?
- Static lookup is now logarithmic and exact, but the measured run still makes
  hundreds of thousands of collector lookups. Is traversal itself the next GC
  cost, or does wall/CPU attribution show it is already cheap enough?
- Which compiler/runtime tests still compile fixed sources independently, test
  removed mechanisms, or pull heavy support through dev-dependency edges after
  the fixture and Cargo-graph changes?
- Are support-program extraction, static-region indexing beyond the current
  catalog, or literal reclamation justified by final live/cumulative memory and
  code-size evidence? Keep them deferred without that evidence.
- What failure cases remain weakly covered: nested forcing during display,
  cancellation between construction and publication, source drift at
  publication, retirement during quiescent transition, and reuse after partial
  failure?
- Is the designed single-cell executable bundle now worthwhile, or do fused
  display requests and support reuse make its complexity unnecessary? The
  six-statement cell still makes eight requests and takes 2.770s while a
  one-statement cell takes 0.687s, so request batching is the largest measured
  multi-statement latency opportunity.
- Can the JSON/Text authority values already resolved during prepared
  projection be threaded to the host-binding sidecar without widening the
  request pipeline? The completed gate adds no work to ordinary binds, but a
  typed host-carrier request currently repeats its candidate authority lookup.
- Revisit the resident-Haskell compute candidates recorded in
  `stg-specialization-followup.md`: first confirm whether `Tidepool.Patch`'s
  production `planUpdate` path is still used, then measure its linked-String
  parser/Myers/list-indexing costs. Keep typed `FromJSON`, inspection rendering,
  CSV/TSV helpers, and compile-time quasiquoters evidence-gated.

## Shared contracts and delivery state

- Immutable support belongs to the toolchain epoch and existing artifact/export
  owners; session Lib/Val and effect-row shims remain mutable home modules.
  Package interfaces must satisfy ordinary GHC consumers and intrinsic authority.
- Display observes one execution; observation publication precedes presentation
  failure. Runtime budgets and keys are values, not source-specialization inputs.
- Host mounts reuse typed construction and existing machine-owned handles;
  imports validate type and session identity before installation.
- Temporary root chunks have stable addresses. Reusable DAG handles remain
  rooted; tree consumption is explicit and happens only after parent publication.
  No borrowed heap view survives collection or forcing.
- Static-region indexing identifies candidates only; exact object/tag checks and
  transactional installation/retirement remain authoritative.
- Fixture registrations name real producers and schema contracts; migration
  regenerates payloads and never patches version headers.

### Completed in this continuation

- `aaddaee12`: deleted unused native fault-recovery wrapper and obsolete claims.
  Machine-state unit tests: 55 passed. Scoped formatting passed.
- `f2c1f2b0a`: restored resident measurement snapshots and JSON measurement output.
  Tidepool library test target compiled; execution baseline in progress.
- `99fbcb566`: narrowed Cargo dependency propagation and target preparation,
  daemon startup and signal cleanup. Selection tests: 16 passed; command tests:
  4 passed; shell syntax/Python compilation/diff check passed.

### Historical completion state

The preceding wave completed at the revision described by the historical
integration evidence below. Its stable-support packaging proposal was later
retired rather than promoted to a supported owner. Cross-actor spec artifact
reuse remains evidence-gated: the profiles did not justify adding another
cache owner. Per-carrier native function/byte deltas were not measured; the
structural evidence proved one compiler request and one program for each
Text/Job carrier, not a wall-time or native-byte claim.

The current continuation is complete only after the authenticated JSON
contract, compiler representation repair, actor response streaming, and safe
JSON traversal have passed their focused and integrated acceptance checks.
Current results are recorded separately below so historical schema and timing
claims are not mistaken for the supported state.

### Retired selfharness requirements

The selfharness and operator-web crates remain in the repository as historical
source for future reference, but are outside the supported Cargo workspace and
are not compiled by the default or all-supported-target checks. The supported
surface retains Shoal, the toolchain-stamp utility, and the independent compile
reporter. The following requirements are retired with this parcel:

- `tidepool-selfharness` and `tidepool-selfharness-web` are no longer
  registered executables or deployment outputs.
- `tidepool listen` and the durable selfharness listen channel are no longer a
  supported operator path.
- Selfharness/web acceptance batteries and the crash-recovery executable test
  are no longer maintained; their source remains available only for reference.
- Cargo automatic binary discovery is disabled for the retained source trees,
  so an ordinary workspace build cannot rediscover those executables.

The immutable-support manifest/staging layer and compiler-bound MCP wrappers
were removed. Core/Authored source generation again uses the existing
content-addressed source owner, preserving the established compiler memo
reuse path.

### Baseline evidence

Frozen pre-elision executable copies under
`target/structural-performance-a/baseline` prevent concurrent builds replacing
measurement binaries. Source baseline `74aa192cf` plus the measurement-only
harness and dead native wrapper deletion. Worker SHA-256:
`d7a8b6c99f6ee16f5a5372666ab4f0a3113e601639d6309844124ca9fa99e9bc`;
frontend SHA-256:
`e414a5e029229fd895e7a5dcc771c9c8e839d479fcbe8be7f864319d708e19de`.
No timings from concurrent-build runs will be presented as controlled speedups.

### Additional evidence

- `7dc91b0f9`: JSON count no longer constructs HaskellValue trees or copies
  text/limb payloads. Exact integer visitation avoids intermediate HaskellValue
  nodes and cloning limb buffers. Bridge unit tests: 38 passed; expanded
  numeric-boundary counting assertion passed. HTTP unit group: 15 passed and
  one failed because ambient worker protocol was stale; exact JIT HTTP family
  rerun with frozen matched frontend/worker via `just test-lib` passed (1).
- `b2e32b272`, repaired by `3ae07389f` and `5e942226f`: checked leaf-interface
  elision retains the session Lib interfaces required by source-less Val
  interfaces and shares prepared tidy work. Focused metadata/prepared/TH/boot
  tests passed.
- `b3da35b33`, repaired by `f895bea4a`: chunked roots/explicit consumption,
  lazy observation roots, borrowed static inventory and reverse constructor
  lookup are generation-safe. Focused stale/duplicate/nested/tiny-nursery and
  allocation-failure tests passed.
- Initial frozen-executable cell harness passed: one-statement cells make 5
  compiler requests, six-statement cells 10, lookup 1. The run overlapped builds
  and is diagnostic rather than a controlled timing comparison. Its compiler
  stage log was not retained by the daemon wrapper; repeat matched runs with an
  explicitly retained compiler log. Native cumulative bytes grew from 3,729,234
  after first cell to 5,033,170 at final lookup; 14 live programs, 936 code exports.

### Integration progress (2026-09-22)

- `3ae07389f` independently repaired checked-interface elision: session Lib
  interfaces remain available to source-less injected Val interfaces.
  `5e942226f` shares prepared interface tidy work and elides unused prepared
  leaf interfaces. Worker and focused metadata/prepared/TH/boot tests passed.
  A notebook old-type retention test also fails with the frozen baseline
  worker; its source-less value-interface path is under investigation.
- `39b613495`, `5440ca555`, `9b60fc459`: seven embedded artifacts now use
  schema 12, with complete producer contexts and executable regeneration.
  Full embedded regeneration reproduced all seven committed payloads exactly.
- `ed7548597`: mutable turns no longer serialize unused dependency/ask sidecars
  or successful attempted source/provisional plans. Evidence is still revalidated.
  Last-attempt/failure/no-retry and notebook rejected-source preservation checks
  passed; worker compiled.
- `25d577b0f`, `f93f9b52f`: immutable raw bytes and parsed artifacts share Arc
  ownership; single-use compiled turns move graphs into installation. Runtime,
  actor and harness test targets compile. The composite runtime bind/import/
  park/resume/cancel/retire scenario passed. Shared artifacts retain borrowing.
- `f895bea4a`: independent managed-root review repaired stale-handle slot reuse,
  zero-before-registration, nested ownership cleanup, and allocation failure.
  Construction uses an O(1) free list; response fields mutate in place. Focused
  stale/duplicate/nested/tiny-nursery/failure tests passed; runtime compiled.
- `690ad57f9`: MCP dependency audit: removing obsolete production dependencies reduces the
  normal `tidepool-testing` graph from 222 to 153 package/version entries.
  Actor/model/tool and transport support no longer enter through MCP. This is
  a graph measurement, not a wall-time speedup. MCP/testing/runtime all-target
  checks passed; nine protocol goldens passed after restoring three missing
  already-shipped effects to the ordered declaration assertion. Facade/harness
  all-target consumer checks also passed.

### Historical final integration and typed host boundary

- `f13d8943f`, `5bc4c1bb3`: actor request JSON, retained tool-result Text, and
  command Job payloads use typed managed mounts. Payload bytes no longer enter
  generated Haskell source or its cache key. The request carrier is private and
  retired after preparation; leases preserve closures that captured an older
  request. Text and strict Job retain the authentic Text descriptor with one
  lightweight `Value.String` companion in the same compiler request/artifact;
  the companion receives no handle, root, or source-visible binding.
- `ba0ebc4d7`, `b0c946e38`: worker protocol v12 carries a closed compiler-issued
  JSON/Text/Job authority. JSON and Job require byte-authenticated shipped
  sources in the active home unit; Text comes from GHC's selected installed
  package. JSON validates the complete Value layout and selected Text field;
  Job validates its sole strict selected-Text field. Rust rejects absent,
  foreign, or mismatched authority before table merge or program installation.
  A fresh independent review accepted the repaired gate.
- `5d7cfb786`: fixed bridge, actor, and harness constructors resolve only by
  compiler-qualified identity and representation arity. Primitive decoding is
  guarded by the canonical constructor, Rust `Result` maps only to Haskell
  `Either`, and the obsolete resilient-name fallback and its tests are deleted.
- `6f12d8157`: a second programmatic dependency sweep removed eight dead Cargo
  edges and one lockfile package. Remaining machete reports are derive/build
  script inputs with verified consumers.

Historical integration checks passed: `just quick` (722 tests); `just fixtures-check`
(812 projection/validation/admission/compilation cases, 705 executable cases,
zero failures, all structural cohorts, seven embedded artifacts); bridge suite
(70); typed JSON/Text/Job carrier execution; A-to-B captured-input retention;
tiny-nursery rejection/reuse; worker v12/retired-v11 protocol checks; and the
focused harness/actor authority cases. `cargo check --workspace --all-targets`
and `cabal build all` compiled every Rust and Haskell target. Rust formatting
and `git diff --check` passed.

Resolved failures: two stale six-field/error-category unit tests were updated
to the new typed contract; the first carrier execution test exposed optimized-
away Text metadata and led to the one-request companion; the first fixture run
found a stale oracle fingerprint after `Job` changed from `newtype` to strict
`data`, and regeneration changed only that fingerprint before the full rerun
passed. Intermediate test failures are not counted as passed coverage.

Unrun in that historical wave: the approximately two-hour `just verify`, by
repository policy. No controlled actor-spec activation/reload timing or
isolated per-carrier native-byte measurement was claimed.

## Structural finish wave: 2026-09-22

This continuation retires unsupported build surfaces, establishes one
compiler-authenticated JSON contract, repairs constructor representation
consistency, streams actor responses, and bounds JSON traversal ownership.
The schema is version 14 with worker ABI 7; all seven registered embedded
artifacts were regenerated through their canonical producers.

### Implemented parcels

- `a2c7faa30`, `505795f3f`, `1972dc9cb`: retired the selfharness and operator
  web crates from supported workspace/deployment/selection graphs while
  retaining their source. Removed immutable-support staging and compiler-bound
  wrappers, restored the content-addressed Core/Authored source owner, and
  prevented automatic binary discovery.
- `5c12d3af0`, `79f639e52`, `04504eab0`, `05e8177f6`, `5d911c263`,
  `9626578a1`, `f3869ecc6`, `427ea3087`: centralized the 18 named JSON roles in the
  program contract, authenticated nominal identities and dependency owners,
  kept decode `Left`/`Right` operation-local, attached the parked program's
  layout to structural replies, shared immutable constructor metadata without
  deep response copies, removed eager JSON trees, and corrected the
  compiler-derived GHC role fixtures.
- `9f23cbf2b`, `134f59690`, `7070338b4`, `8b19d8a14`, `a59c979ef`,
  `e062b5ee0`, `287308aac`: made
  representation flags canonical before GHC loading, preserved prepared home
  interfaces and TH bytecode, rejected conflicting constructor evidence before
  publication across independent type graphs, retained that evidence at the
  canonical interner, removed the Scientific boxed-exponent fallback, and
  pinned the installed `Either` owner used by native decode.
- `29a7f8792`, `86ce60320`, `c1e6ec554`, `3a3db0a02`, `e07cfe7ad`,
  `2cd399277`: classified continuation responses from frame consumption,
  streamed wait/poll and every supported actor response projection, removed
  superseded eager builders, and consolidated framed-custody validation.
- `bc89ce165`, `12c105e06`, `457631734`, `f701519e4`: replaced list-wide JSON
  identity history with rooted Brent detection, bounded Value/map ancestor
  tracking, prompt temporary-root release, geometric fallible free-list growth,
  duplicate-key cleanup, primary-error preservation, real traversal metrics,
  and the intrinsic scope required while forcing lazy observations.
- `d25a4baf2`, `d238731d5`, `55ec05422`: selected shared shell output through
  Jev using the existing response contract, kept intrinsic fallback demand
  conservative through optimization, and authenticated every embedded source
  which can change the worker. Its compiler and runtime regressions are part of
  the joined acceptance boundary rather than a separate performance claim.
- `034503692`: restored the projection-only retained-import fixture's opaque
  boundary, updated the real unretained compiler baseline for canonical
  optimization, and regenerated the affected import artifacts and corpus
  fingerprints through their producers.

### Current evidence and remaining gate

Focused compiler, schema, runtime, actor, and GC checks pass, including the
compiler-produced Scientific installation test, shadow dependency rejection,
parked JSON reply, actor reload, classified response retry, real JSON failure
recovery, and the serial codegen suite. The supported Cargo all-target check
and `just quick` also pass. Independent GC review accepted rooted-cycle,
temporary-root, cleanup-error, and lazy-force ownership after the concrete
failure repairs. Independent compiler/schema review accepted canonical
cross-graph constructor interning, authenticated `Either`/JSON ownership,
prepared-interface consistency, conservative intrinsic demand, and COW
authority clearing; its stale provenance comment was corrected in
`4860ab773`. The complete `just fixtures-check` passed on the joined candidate:
845 projections, validations, admissions, and compilations passed; 738 finite
executions passed, with 104 intentionally non-closed and three without finite
observations; all 280 reached comparisons passed; all seven registered schema
14 artifacts passed their producer check. `just quick` passed 726 tests with
one ignored, and supported Cargo all-target compilation passed. Matched workload
measurements and the final post-audit revision are recorded after the required
second pass; `just verify` remains intentionally unrun by repository policy.

### Second-pass audit

The fresh post-integration pass found three bounded gaps and repaired them:
canonical constructor validation now retains evidence at the shared interner
instead of rebuilding an ephemeral declaration inventory; retained-import
fixtures distinguish the projection-only opaque boundary from real optimized
compiler behavior; and `a77f11b25` gives worker freshness, the toolchain doctor,
and changed-test selection one owner for all five Template Haskell embedded
authority sources. Its 33 selection/freshness tests, Bash syntax checks,
ShellCheck, and workspace metadata audit passed. No additional cache, package
boundary, request batcher, or dispatch redesign was justified.
