# Structural performance A: implementation ledger

The user authorized a broad structural sweep after the first STG performance
wave, including adjacent low-risk simplifications and Rust compile/test costs.
Baseline: `57aa159cf`. Historical timings in `stg-specialization-followup.md`
are not a matched post-JSON baseline. Report scopes separately; do not sum
category savings with defining-module savings.

## Accepted core

1. Eliminate full GHC interfaces for leaf targets only after proving no
   downstream compilation, metadata query, recovery, or TH consumer needs them.
2. Package immutable Tidepool support under the toolchain identity; keep mutable
   workspace, effect-row shims, and session generations outside that boundary.
   Reuse existing artifact, export, and validity owners. Audit code retained
   solely because support exports reside in authored programs.
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
- Unify embedded prepared-fixture inventory and migration checks. The six files
  under haskell/test-prepared-stg/fixtures still carry schema 11; the corpus
  check does not validate them. Regenerate from their producers, not header edits.
- Remove deep artifact/program copies at consuming ownership boundaries.
- Audit unused mutable-turn evidence/sidecars before removing their production.
- Remove JSON guard placeholder trees, bigint intermediate values, repeated
  observation bitmap/region copies, and eager observation-root reservation.
- Audit linear constructor lookup in structural response validation.
- Reuse immutable actor-spec artifacts with fresh actor instances and exact
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

### Pending

Compiler leaf interfaces; support packaging/export lifetime; artifact ownership;
turn-product deletion; display fusion; typed mounts; actor-spec reuse; temporary
and observation roots; static catalog; JSON visitor follow-through; test-support
and asset dependency reduction; immutable test preparation; embedded fixture
migration; independent reviews; integration checks and matched measurements.

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
- `b2e32b272`: checked leaf-interface elision implemented; independent review
  found session Lib interfaces needed by injected source-less Val interfaces.
  Repair and regression coverage in progress; not accepted as complete yet.
- `b3da35b33`: chunked roots/explicit consumption, lazy observation roots,
  borrowed static inventory and reverse constructor lookup implemented.
  Independent review/repair in progress: slot reuse must fence stale handles,
  clear pointer words before registration and preserve nested root ownership.
- Initial frozen-executable cell harness passed: one-statement cells make 5
  compiler requests, six-statement cells 10, lookup 1. The run overlapped builds
  and is diagnostic rather than a controlled timing comparison. Its compiler
  stage log was not retained by the daemon wrapper; repeat matched runs with an
  explicitly retained compiler log. Native cumulative bytes grew from 3,729,234
  after first cell to 5,033,170 at final lookup; 14 live programs, 936 code exports.
