# Engine completion contracts and execution record

Approved continuation of `engine-harness-integration.md`, 2026-09-29, from
verified foundation `2adfe22f1ccf4add4eb77efb9b2c17a70161ea9b`. This document
records the implementation baseline and accepted shared boundaries. The
foundation checkpoint is complete; the overall implementation is not.

## Finish line

- M1: production sequential embedded Engine/Store/browser composition and
  deterministic real-resident acceptance, preserving Codex as default.
- Compiler/native: independently reusable durable module products across
  different cells and fresh workers, exact import provenance, reachable-group
  native demand and final-real-owner reclamation.
- M2: execution-local notebook state, atomic final publication, exact cleanup,
  independent reusable captures, shared runtime semantics for both backends.
- Git/process: retained execution evidence for cross-process admission and
  the production thin launcher's failure and cleanup paths.
- Delivery: matched package, full joined gates, verified remote dependencies
  once the user lifts the push hold, then approved live G5 acceptance.

Pushes remain deferred. G5 requires a concrete readiness report and separate
user approval. No running-session migration, default-backend cutover, automatic
effect replay, or shared-service replacement belongs to this implementation.

## Shared ownership and interfaces

### Harness / facade

Harness `OperationId` (origin conversation/incarnation, request, original call)
and schema v5 remain the operation authority. Preserve IDs on the wire and
in inherited claims. No Tidepool identity registry or duplicate model journal.
Facade owns immutable per-run backend selection and production composition;
one Store/scheduler lives under `run_root/harness/`. Actor admission binds the
conversation, tools, authority and lifecycle. Durable input precedes wake.
An exact retry returns the retained outcome without reevaluation.

Keep the harness's own pending-result wait tool; it is not an actor supervisor.
Do not expose standalone spawn/message/retirement tools in embedded mode.
Use the existing router, authentication and matching immutable browser assets.
Retain configured read-only Codex credentials, explicit public HTTPS scheme,
session expiry/rotation checks, compaction and unavailable-after-host-loss
semantics from `harness-integration.md`.

### Compiler / runtime

The compiler owner owns Haskell worker changes and its typed Rust transport,
including the declaration-join validator. The runtime owner consumes that
validator; it must not independently edit the worker protocol or GHC pipeline.

Neutral products are entry-free, versioned definitions with stable original
recursive-group ordinals. The toolchain's existing cache owns atomic pairing
of neutral bytes, skinny `.hi` bytes and complete dependency evidence. The
worker owns semantic evidence; Rust owns cache policy and filesystem storage.

Compilation has graph-inventory and product-consumption phases within the
existing transaction. Inventory includes exact source/options, ordinary/boot
nodes, direct resolution and negative witnesses, package-interface identities
and scoped retained context. Revalidate consumed evidence before publication.
Use SCC closure identities to avoid cyclic hashes. Assign module versions
before annotating exact inter-module owner references; validate every edge and
integrity-check the final bundle. Unknown compile-time inputs cause misses.

Imports distinguish an exact source module version/binder, retained binding
instance, and package origin. A spelling/signature match is not an owner match.
Demand closes reachable recursive groups before entry, seals checked batches,
and compiles outside checkout. Coalescing uses exact group identity. Immutable
code keys exclude roots, mutable imports and evaluatedness. Materialized CAFs
keep explicit semantic leases; unused exports are weak metadata.

Declaration validation takes the exact current public generation and private
write set/provenance. Its typed receipt represents either a validated candidate
or a rejection for that generation. Wrapper typechecking is supplemented by
combined class/family-instance consistency, including instance-only and
transitive imports. An unprovable join fails without visibility changes.
Stale successful or rejected staging must be retried, never treated as final.

### Runtime / actor

`ResidentSession` owns staged declaration and binding publication and one
synchronous no-await paired visibility swap. Preserve original binding IDs,
interfaces, roots and dependency leases; choose winners by completion order.
Do not recompile private source against the newer public environment.

The actor keeps lifecycle/shared coordination/model-turn state. Existing exact
execution identities key records owning private scopes, admitted source/tool
leases, cursor, control/reply, effects, reservations and continuations. Keep
`WorkbenchExecutions` as replay authority. Execution-owned tasks send exact
step-generation outcomes to Ractor; do not move the whole behavior or await
external work in the actor handler. Machine mutation retains one checkout.

The lock order is machine checkout then short publication-decision lock.
Cancellation never holds that lock while awaiting checkout or cleanup. Only a
successful visibility swap marks Published; stale staging leaves it unpublished.
Computing cancellation is non-preemptive until a boundary establishes outcome.

Before removing dispatch serialization, make request reservation and after-tool
hole cleanup exact to the creating operation. Direct uncorrelated raw cells use
local execution identities with no remote retry promise. Structured actor turns
remain explicitly non-reentrant, using owned completion messages across waits.

### Captures

A successful capture owns completed private lexical state and strong leases on
the execution's admitted source, independently of later parent settlement.
Harness attachments preserve the same effect boundary and original pending
claims. Children acquire their own admitted scope/source ownership.

A user-visible Haskell token is a real explicit owner until ReleaseCheckpoint,
regardless of backend. Do not weaken it to a weak index merely because a harness
attachment exists. Harness attachments/children retain independent shares;
attachment-only captures need no hidden token owner. Release revokes future
token admissions and drops its share; existing children survive. Final scope
cleanup queues to the resident session owner, not the possibly retired issuer.
No new checkpoint registry and no serialized live-heap recovery.

## Assignment and verification rules

Integration owns shared contracts, cross-owner review and joins. Sol owns
compiler, runtime and facade transitions; Luna owns bounded independent leaves.
Use Astra only for consequential unresolved decisions and exact-commit review.
Each assignment uses its own real Git worktree and mutable build outputs.

Use declared Nix wrappers. Admit builds through the user-owned
`tidepool-completion-build.slice`: 88 GiB memory high, 104 GiB maximum, and
2 GiB swap maximum. Launch build commands with `systemd-run --user` under
that slice even when the interactive session itself is outside it. Begin with
one expensive build; increase concurrency after checking aggregate accounting.
Nix daemon work is not bounded by the caller slice; realize expensive closures
serially and account for it separately. Never restart shared daemons or obtain
sudo for a build.

Compile changed consumers, run exact focused success/failure tests, format, and
record full revision, command, executed count, exit status and retained log.
At joined boundaries run structural fixtures and all relevant broad suites.
Final verification must cover every `just verify` constituent in coordinated
bounded lanes, plus matched harness/assets/client package construction.

Acceptance includes source-hidden interface consumption with a validated
inventory, independent module hits, dependency/shadowing/unsafe-effect misses,
recursive/overlapping demand, unused-code omission, distinct installs/CAF state,
static and copied parcel failure/lifetime, final-owner release, both cancel/
commit orders, stale staging, exact cleanup, and children from an unfinished
parent surviving its later failure. M1 must use actual production composition,
including reconnect gaps, authentication rotation and pending-call compaction.

Remove obsolete ownership paths as their consumers migrate. Report structural
savings separately from matched time, allocation, memory and native-byte
measurements. Preserve source-box historical evidence and all existing bundles.

## Delivery and live acceptance

Publish companions before root gitlinks only after the user lifts the hold.
Verify remote OIDs and clean-checkout package/source capture. Until then retain
verified bundles and an explicit unpublished-dependency limitation.

G5 follows an approved exact binary/assets/workspace/task readiness packet:
a browser-operated Sol root delegates two small independent components through
recursive Luna owners, with at most four simultaneous model clients. Ordinary
Haskell orchestration, typed replies and a Haskell-only result join are required.
Retain commits/checks/reviews, trace, memory/timings, root interview before
retirement, and explicit remaining-resource disposition. Infrastructure
acceptance and a default-backend cutover remain separate.

## Implementation record

- Baseline scaffold: contracts recorded; implementation and gates pending.

## Implementation checkpoint, 2026-09-29

Integration revision before this record: `8e705d8d0`.

- `9bcbd7ef4`: request reservation rollback uses the creating workbench or route
  identity. Two focused actor regressions executed and passed; actor library
  consumers compiled. Evidence:
  `/srv/swarm/checkouts/tidepool-completion-runtime/target/completion-evidence/request-reservations.log`.
  The transitional active operation field remains sequential; moving it into
  execution state and exact continuation cleanup are still required for M2.
- `c5bfb9e8b` and `0bcf1a97c`: per-run backend selection and durable exclusive
  publication through the atomic-write owner. The atomic publication race,
  competing backend initialization, immutable resume selection and host failure
  diagnostic tests all executed and passed (four tests across focused commands).
  The facade binary compiled. This is bootstrap infrastructure, not production
  embedded Engine acceptance. Evidence is under
  `/srv/swarm/checkouts/tidepool-completion-harness/target/completion-evidence/`.
- `8e705d8d0`: one Git admission implementation replaces clone-wide exclusion;
  compatible reads and unrelated backings may proceed concurrently. Commands
  and transactions both revalidate identities after acquiring admission.
  Independent review found and verified repair of the post-wait identity gap.
  Fifteen admission tests and twelve other library tests executed and passed.
  Commands used the pinned dev shell and `cargo nextest run -p exomonad-worktree
  --lib`, with filters `test(git::admission_tests::)` and its negation.
  Exit status was zero for both. Logs:
  `/srv/swarm/checkouts/tidepool-completion-git/target/completion-evidence/git-admission-revalidated.log`
  and `git-other-unit.log`. Launcher and joined integration gates remain open.

After this checkpoint the session sandbox changed: existing sibling worktrees
and original Git metadata became read-only. The originals and all evidence are
retained. Independent source checkouts were created in `/tmp/tidepool-engine-*`
from exact recorded revisions; WIP transfers preserve only source diffs, not
build caches or credentials. The declared dev-shell probe in scratch failed
with `cannot connect to socket at '/nix/var/nix/daemon-socket/socket': Operation
not permitted`. New source changes require verification once the declared
build environment is available. Existing passing evidence does not cover them.
Push hold and live G5 approval gate remain unchanged.


## Resumed admission, 2026-09-29

- Infrastructure fix `91151b0f9` opens the pinned dev flake through the worktree
  root. It is retained on integration before the recovered checkpoint record.
- Nix access is restored. The declared shell reports Rust 1.93.0 and GHC 9.12.2.
- A systemd user service probe ran inside the completion build slice and exited
  zero. The slice reports 88 GiB high, 104 GiB maximum, and 2 GiB swap maximum.
  The interactive session remains outside it, so each build must be admitted.
- Original worktrees and scratch candidates remain retained. Compiler and facade
  owners reconcile their source before new edits; source reconciliation does not
  establish verification of the recovered candidates.
- Compiler structural fixtures get the first expensive build slot. Runtime exact
  continuation cleanup and M1 production wiring proceed independently. Sol owns
  the compiler boundary; Luna owns the two bounded runtime/facade parcels.
- Pushes and live G5 remain gated as above.

### Entry-free products structural gate

Integrated sidecar `4d2f6950f` from compiler `6d4e14a76`. An independent decoder
review found no blocker. Required `just fixtures-check` passed at compiler
`49df9da92` (sidecar plus the dev-shell infrastructure fix): 692 Suite tests
passed, zero failed; 261 target corpus and seven embedded artifacts accepted.
Command ran through the admitted user service `tidepool-compiler-fixtures-0929`
and exited zero. Retained log:
`/srv/swarm/checkouts/tidepool-completion-compiler/target/engine-completion-fixtures.log`;
structured result: `target/prepared-corpus/latest-success.json` in that checkout.
The shared build slice peak was 5,461,962,752 bytes, including an overlapping
helper build; this is not an isolated fixture memory measurement.

This validates entry-free product serialization and existing corpus behavior.
Independent module cache hits, precompile evidence, hydration, native demand,
and declaration joins remain open.

### Production view helper acceptance

Integrated `d1600316b` from `2fa40a81f`: two production helper tests passed,
covering non-UTF-8 arguments/environment, environment removal, cwd, null stdin,
separate stdout/stderr, missing executable/cwd, payload exit 127, SIGTERM, and
subsequent reuse of the retained view. The exact target was
`cargo nextest run -p exomonad-node --test overlay_rotation -E 'test(view_helper_)'`
through the pinned dev shell and admitted user service; exit zero, 2/2 executed.
The production `exomonad-view-helper` binary was built first.

The existing ignored paired benchmark ran 100 pairs with 1,024 MiB touched host
RSS, 20 warmup pairs and alternating order: legacy median/p95 39.107/40.913 ms;
helper median/p95 3.930/5.089 ms. This is a local synthetic command-launch
measurement, not end-to-end worker throughput or historical compiler evidence.
A separate two-pair strace run passed and showed the production helper launch
using `clone3(CLONE_VM|CLONE_VFORK|CLONE_CLEAR_SIGHAND)` followed by helper and
payload execs in the same child. The legacy path used a separate non-CLONE_VM
clone. Traced timings are not performance measurements.

Evidence under `/srv/swarm/checkouts/tidepool-completion-git/target/completion-evidence/`:
`view-contract.log`, `view-measure.log`, `view-trace.log`, and `view-spawn.trace`.
Holder/lease cleanup and other process-scope/terminal paths still need their
remaining acceptance checks; these tests do not close the entire process track.

### Module graph evidence gate

Integrated `1e6934f4c` from compiler `8607712ce`: versioned post-downsweep home
module graph evidence is paired with products in the existing atomic cache
bundle. Each direct ordinary/boot import must match its unique resolution
witness; each emitted product must name a recorded graph node.

Exact-source gates passed: 11 cache tests, two matched-worker tests, and full
`just fixtures-check` (261 targets, 692 Suite passes, seven embedded artifacts).
Logs in `/srv/swarm/checkouts/tidepool-completion-compiler/target/`:
`engine-completion-inventory-v2-fixtures.log`,
`engine-completion-cache-suite.log`, `engine-completion-inventory-v2-tests.log`,
and `engine-completion-worker-inventory-v2.log`. The worker was built in an
isolated build directory after incremental artifact inconsistency; source and
command details are retained by the compiler owner.

Independent cache hits remain disabled pending precompile evidence and worker
hydration. Product availability and exact package owners remain required.
See `engine-completion-findings.md` for tracked structural findings.

The additional helper acquisition regression in `aafac64d1` passed alongside
the previous two helper tests (3/3, log `view-lease.log` in the Git checkout's
completion evidence). It proves independently acquired descriptor retention and
rejection of an expired reference, not exporter-process-death coverage.
