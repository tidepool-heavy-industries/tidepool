# Compiled-cell engine and embedded harness delivery

Approved 2026-09-30 after the independent architecture review. This owns the
next implementation wave and supersedes incompatible staging and migration
choices in earlier completion plans. Baseline: Tidepool `b954c4674e`, harness
`9986ca3cf8b1e4be9826cb7420de01e4371922c7`. Preserve all retained worktrees and
`test-source-boot/`. Reviewed commits may be pushed; default changes remain
deferred. The user will
drive the first live run after delivery; automated acceptance uses offline
providers and packaged startup without an initial model prompt.

## Approved completion wave — 2026-10-01

Finish the matched optimized deployment and existing correctness/performance gates.
Root owns source freezes, build-proof integration, headless proxy measurements and
canonical joins. Sol component owners work M2 domains, original support issuance,
parsed-product reuse, recovery/CAF isolation and packaging. Independent Sol reviews
rotate through ownership, compiler authority, harness/recovery and cost attribution;
Luna owns bounded fixtures, verifiers and trace analysis. Preserve isolated WIP and
failed evidence. No milestone closes on compilation or neighboring tests.

- Complete the shared GHC verifier from all raw `--make` actions and actual GHC
  source/object compilation records. Admit only the exact inactive owned Cabal
  macro pair. Retain the refused packet; use a fresh build. Recorder and reporter
  share verification. Final Nix/Buck binaries need their own action/input proof.
- M2 uses captured-view domains with exact owner transitions, installation-issued
  native attachment authority and rollback of newly added selections and shares.
  Compile actual test targets, then prove both materialized and unmaterialized
  nested captures, metadata-only staleness and final-reader reclamation.
- Separate immutable original-source projection from executable/live-binding
  requirements. Require positive package evidence and retain generation guards.
  Prove the Prelude/Core dependency closure is accepted, including fresh CAF state.
- Produce typed products and normalized singleton TPMOD framing in the first
  bounded decode. Share that request-owned inventory across certification,
  publication and export. Preserve nonminimal-input normalization and opaque group
  bytes. Prepare records before moving products, write only after admission.
- The package-facing Buck bundle uses Buck host, view helper and browser plus the
  exact Nix compiler pair, immutable source root and catalog as declared resources.
  The Buck compiler pair remains a native development/test target. Never attach a
  catalog sealed to another pair. Nix remains the installed distribution owner.
- Execute HTTP/browser M1 plus separate reload/late-output cases, both G3 worker
  trees, real nonempty recovery, the complete M2 actor matrix and structural corpus.
  Include real packaged init/AgentSpec/tool/stop with checkout/build scratch hidden.
- Measure the existing warm/cold/cancellation and B0/B100 × N1/N10/N100 gates,
  plus eight actors. N counts bindings within a cell; measure later-cell history
  growth separately. Attribute nested timing honestly and distinguish unique
  retained allocations from shared references. Final latency runs exclude
  competing expensive builds; measured failures remain delivery defects.

## Joined completion work — 2026-10-01

The canonical worktree now includes the independently reviewed captured-domain
admission and rollback repair, typed original-source projection, first-decode
product inventory, exact compiler invocation identity, and matched-package
assembly. These remain uncommitted integration work; they do not close M1, M2,
or delivery acceptance.

The domain rollback marker required an `AtomicBool` to keep the actor kernel
`Send + Sync`. The owner checked the actual actor and facade production consumers
and executed both rollback/held-receipt regressions after this repair. Earlier
`Cell<bool>` compile failures are retained. Evidence:
`target/completion-evidence/source-domains-m2-atomic-joined-20261001/`.

Compiler traces now qualify requests with daemon epoch, admission ID and request
ordinal; content digests can repeat. Twelve transport cases passed with owned
fake workers. The reporter's 61 executed tests pass after independent review and
repair of two false-acceptance paths: cold rows must carry package evidence, and
retained SQLite files must be reopened and checked rather than trusting reported
counts. Production timing acceptance remains open. Evidence:
`compiler-invocation-joined-20261001/` and
`resident-report-admission-joined-20261001/` under the same evidence directory.

Buck built and retained browser assets at harness
`b5f0cd2a890359def704a3c30bb19ad8fe15a5de`. This is a build, not an executed browser
test. The native Buck bundle supports explicitly configured embedded startup;
Nix remains the installed distribution owner and supplies the matched compiler,
source and catalog. Full Nix materialization, package execution, hidden-filesystem
startup, AgentSpec/tool calls and cleanup remain open. Evidence:
`matched-web-buck-20261001/` and `package-assembly-joined-20261001/`.

The captured worker-tree baseline still fails on missing original execution
provenance for a cached quotation dependency. The candidate record/manifest repair
is in progress. The optimized headless baseline passed its four actual proxy
calls, but each took about 41–46 seconds. Repeated GHC state reset, dependency
bytecode provisioning and candidate decoding are measured investigation targets.
The final pure call followed an RSS-triggered worker replacement under the
probe's explicit 7168 MiB cap; it is not proof of an already-warm GHC session or
of a live-heap leak. Final performance gates require uncontended matched runs.

## Consolidated delivery checkpoint

The published Tidepool checkpoint recorded here is `8fe64431e5`, with tested
harness revision
`b5f0cd2a890359def704a3c30bb19ad8fe15a5de`. The finite async candidate
`6933217339feb4590e8845b65da7266c5a6c1668` is merged, and workspace
`2390d350d4583a849280370b375c8b075d40ed50` is materialized. All five earlier
async topic branches are also merged. The merge preserved 97 engine worktree
paths; `refs/tidepool/checkpoints/pre-finite-async-wave` retains their pre-merge
snapshot. The remaining engine changes have not yet been committed or accepted.

Cargo and `flake.lock` retain harness pin
`b5f0cd2a890359def704a3c30bb19ad8fe15a5de`; canonical harness main is
`9c009df9a286df5579fa299e31ef606f85b0c5ea`. Their Git difference changes only
`PRD.md` from “master” to “main”; it is documentation-only, with no runtime or
schema difference. Keep the exact build pin recorded separately from main.

The incoming report certifies 1,000 source occurrences: 973 logical artifacts,
898 actual `check_cell` calls and 75 exact-response reuses. Independent review
checked the source hashes, reuse origins and all 172 workspace package files.
This is source acceptance, not hosted behavior. Reports remain in the authoring
checkout under `target/async-wave/canonical/`; merge and joined-check evidence
is in this checkout under `target/completion-evidence/joined-delivery/`.

The reviewed GUI candidate `ce53708c` is merged in the harness checkpoint with
follow-up inspector focus and linked history fixes. Its 169 GUI tests, seven
tooling tests, TypeScript checks and production build passed. Browser/axe tests
were typechecked, not executed. Tidepool's matched Cargo and Nix pins are updated
in the worktree; matched host/browser packaging remains a delivery gate.

The finite async helper selection also passed: 34 probe checks, 28 browser
workflow/helper cases, 17 native assertions, one pinned-source assertion and
eight prepared-runtime cases. The separate recipe smoke captured seven cells
and recorded four assertions without executing them; its generated workspace
interface is a declared test fixture, not a production workspace capture.

Joined native checks passed 41 cases. The repaired real ModelCall selection
passed all three cases; the queued-reload and completion-progress selection
passed both selected cases. Retained failed runs remain under
`target/completion-evidence/`, alongside the source and binary hashes of the
successful selections. These checks do not close the complete M1 or M2 matrix.

Default-stack production startup now passes after boxing the root-admission
future. The focused HTTP test executed one case with `RUST_MIN_STACK` unset;
static frame comparisons are retained separately and are not peak-memory
measurements. Evidence: `startup-boxing-default-stack-20261001T082936Z/`.

The browser scenario passed one executed case with the default stack. It proved
unresolved same-ID retry, authoritative recovery, real Haskell 42, persisted
output, reconnect without replay, disabled settled retry, installed typed calls
without recompilation, armed Sleep cancellation, continuation and retirement.
A fresh adversarial review found no demonstrated behavioral defect, but noted
incomplete retention of untracked build-time sources and discarded successful
diagnostics. The runner now supports bounded output artifacts; final matched
acceptance must freeze the complete source, including untracked files, before
building. This browser pass used the frozen old compiler pair and does not
accept the newer compiler repair. Evidence:
`m1-browser-identity-cell-repair-20261001T094327Z/`.

The actual headless host now executes the raw quotation regression and returns
`Right "headless-command-ok"`. Its normal compiler authority uses frontend
`30288c14` and worker `09bf8871`; all owned host/compiler processes stopped,
and the Store remained empty of model requests and embedded inputs. Evidence:
`headless-proxy/20261001T090311Z-quotation-gate/`. This verifies the quotation
repair, not production acceptance of the subsequent candidate wire migration.

The parsed-products bundle removes the second product decode and full-tree
comparison. A controlled actual headless run observed seven decode/ownership
checks and zero second-decode/comparison events. Its retained-cell call still
took 93.738 seconds. The earlier phase baseline took 96.595 seconds, but the
CLI also changed startup boxing; this difference is descriptive rather than a
causal speedup claim. Evidence: `headless-proxy/20261001T084504Z-typed-products/`.

Exact-context candidate reuse is being joined across Rust and Haskell. Native
selection tests passed 15 cases, and private-support admission passed six.
The worker manifest now retains the original native-product path and bytes;
production reuse must still prove a closed disjoint dependency graph, original
module versions, source/package invalidation, and preservation of private
lexical meaning. Final cached-dependency admission passed two focused refusal tests and independent
review. The joined wire-v6 production probe executed declaration 42, retained
read 43, a real quoted command, and the original read 43 afterward; all passed,
but calls still took 76–87 seconds. Offered counts do not prove accepted cached
origins. Discovery is currently constrained by whole-include cache buckets,
and closed-source selection needs positive package evidence for package edges.
These measured reuse blockers are being repaired without weakening current GHC
resolution or final child-context validation. Evidence:
`headless-proxy/20261001T091933Z-candidate-v6/`.
New timings separate private-product materialization, verification and
certification; unknown stage costs remain unknown until an executed trace.

The native-reuse worker `ade9d561` accepted 30 cached modules in each of two
real operator cells, then quotation failed with an incomplete exact interface
dependency closure. That failed packet remains at
`headless-proxy/20261001T110255Z-native-reuse-component/`.

The repair retains one authenticated complete interface closure and derives
checked-value admission from it. A real `Command` regression passed with
nonempty type dependencies, a separately captured copy of the same value
interface, completed-prefix use and required refusals. Independent authority
review approved it. The distinct frozen worker `b22da90e` then passed all four
real host cells: declaration 42, retained 43, quoted command marker and final
original binding 43. Times were 79.503, 73.751, 77.013 and 71.241 seconds;
startup took 137.105 seconds. Each operator native compilation certified 30
cached modules. Normal authority, binary/source hashes, empty-input Store and
owned-process cleanup passed. Evidence:
`headless-proxy/20261001T114007Z-checked-command-closure/`.
This accepts the frozen v7 CLI/scope4 repair composition, not current scope5,
G3 captures or final latency gates. Ongoing worker changes require a new matched
source freeze for the final M1 and recovery campaigns.

Rust execution-source provenance passed 13 focused native tests, including
canonical bounded decoding, original-owner seals, shared durable recovery,
selected graph closure and production v5 scope emission. A Rust-emitted scope
fixture also passed the Haskell decoder regression. Actual GHC execution of
retained source recipes has now passed three owning real-GHC gates: sealed
source-free quoters retain their original private dictionary while fresh
providers cannot acquire it; changed sources refuse before preprocessing;
same-worker cancellation and A/B/A restore linker state; protected thin live
values refuse before GHC load. Five affected regressions passed against the
same frozen source, including the actual Rust v5 fixture and checked Command
type closure. Final independent loader review, matched worker build and
composed capture scenarios remain open.
Evidence: `execution-source-final/`.
Current GHC evidence: `execution-consumer-final/`. The final joined source
passed eight owning/affected gates and the schema gate, with independent loader
approval. The matched worker is frozen at SHA-256
`e62f437db06438ebd1a03fee7b988c1040cf124f30d73dbea89ee72191f2331f`.
Its first actual composed declaration failed after 70.268 seconds: compilation
succeeded, but Rust rejected an invalid recovery artifact reference before
publication. No later cells were attempted; the Store remained empty and all
owned processes stopped. Eighteen cached modules were certified on the rejected
declaration path. Core was published once at a 19,577,115-byte record body, then
correctly refused as generation dependent in later compilations. This is
diagnostic evidence, not M1/M2 acceptance or a Core cache-hit claim.
Evidence: `headless-proxy/20261001T130443Z-current-matched-performance/`.

The ordinary-after-exact compiler lifecycle repair now passes its real-GHC
success, refusal and cancellation transitions. Typed compiler origins trigger
the existing full-state reset when leaving protected source-free state;
ordinary-to-ordinary requests preserve validated memo entries. Independent
review approved the repair. The canonical lifecycle/candidate/metadata selection
passed three cases and the schema selection passed one. That component's matched
worker was
`b10e4310`; pinned tools and 453 worker source files matched its retained build.
Evidence: `execution-lifecycle-final/`.

The composed recovery-reference failure was a producer-identity mismatch:
execution graphs used raw producer bytes while exact artifacts use their
canonical SHA-256 identity. The shared typed identity repair passed 17 focused
tests and independent review; a downstream typed recovery loss now preserves
the mismatch and tombstones the original rather than resurrecting it.
Evidence: `execution-source-producer-fix/` and `runtime-producer-loss/`.

The joined CLI `996a4d85` and worker `b10e4310` passed all four actual headless
cells: declaration 42, retained 43, quoted command and original 43. Calls took
78.329, 71.961, 74.312 and 66.814 seconds; startup took 130.018 seconds. The
second compiler round dominates each call; command execution took 65 ms.
Eighteen cached support modules were certified per operator, but Core remained
fresh and generation dependent. Normal authority, retained source/binary hashes,
empty model-input Store and owned-process cleanup passed. This accepts that
functional composition, not the optimized latency gates, full M1 or G3/M2.
Evidence: `headless-proxy/20261001T135455Z-producer-repair-performance/`.

Current actor acceptance executed three cases: two passed, while the two-parked
cell case failed on multiple mutable instances of `Tidepool.Internal.Resume`.
Review traced this to treating retained native custody as ambient instance
selection. Distinct A/B instances must remain retained; installation must use
explicit selected-instance provenance. The repair and later declaration-domain
acceptance remain open. Evidence: `durable-recovery-20261001-r4/`.

The custody/selection repair passed six focused native cases. It keeps retained
native owners separate from explicit installation selection. Nested authored
captures still require importer-relative domains; this component pass does not
close concurrent publication or the actual actor matrix. Evidence:
`source-selection-m2/`.

The G3 campaign executed two cases and both failed before children launched,
with `ExecutionSourceLinkableMissing(main,Tidepool.Agent.Assignment.Internal)`.
A small real-GHC regression traced the missing linkable to an excluded thin
reexport facade. The joined repair forces bytecode for authenticated original
closure targets during GHC load, restores the target policy afterward, and
keeps linkable and provenance validation. Six canonical Haskell gates passed,
including the real 6,037-group candidate reader. A new matched worker and actual
G3 rerun remain required. Evidence: `g3-captured-current-20261001-r2/` and
`g3-bytecode-demand-joined/`.

The earlier M1 fixture campaign passed reload/compaction and failed late output:
the raw binding statement returned `[bound answer]` while its assertion expected
`42`. The fixture now evaluates the same sleeping expression directly; this
repair was independently reviewed but not yet rerun at that checkpoint. Store
snapshots prove applied
compaction and two qualified claims. Seven Haskell paths changed during that
campaign, so its results do not certify the current source tree. Evidence:
`m1-exact2-20261001T151343Z-r2/`.

The optimized exact-two rerun now passed request-pinned typed-handler reload
across pending compaction and real late Haskell output: two passed, zero failed,
exit 0. `source-qualification.json` records no source or HEAD changes during
execution. This supersedes the earlier not-rerun status for those two selections,
not full M1/M2 or performance acceptance. Evidence:
`m1-exact2-optimized-20261001-r3/`.

The current recorded optimized worker is the successful `ghc-build-v2` r3 build:
exit 0, three raw `--make` actions and 52 compiled sources. Worker SHA-256:
`a966fbb489d976e42a34d678b64344c6c3784cb3f50627c5cbfc3b93c1406bad`.
The shared verifier result records 16 home-action, 16 recorder, 31 reporter and
five Rust-recorder tests passing, with no actual-worker reporter problems.
Preserve the refused r1/r2 packets as historical failures. This accepts the
recorded build/verifier components, not composed G3/M2, final Buck/Nix/package
or latency acceptance. Evidence: `ghc-worker-o2-20261001-r3/packet.json` and
`ghc-verifier-joined-20261001/result.json`.

Fresh-product certification now hashes each immutable aggregate buffer once
per certification instead of once per fresh receipt. Two focused tests passed;
`products.certify_fresh_buffers` records elapsed time and aggregate bytes.
End-to-end savings remain unmeasured. Evidence: `fresh-buffer-digest-once/`.

The real source-publication filesystem-fault composition passed one outer test
and two isolated children, retaining two actual fsync EIO injections. It proves
stale-spec disposal, typed visible-but-unconfirmed publication and fresh-process
recovery of the visible source; it does not retroactively confirm durability of
the failed write. Evidence: `source-publication-fault-20261001-r9/`.

The measured large program deltas spent about 5.1 seconds materializing,
rereading and recertifying 54 MB. Requests now reuse opaque native witnesses
anchored to the exact immutable original byte buffers. Issuance and complete
cold recovery retain parsed groups and dependency projections once; hot
admission checks selected full owners, binder/ordinal closure and current
package evidence. Shared-group comparison uses an ordinal index rather than
quadratic search, and interface-only deltas do not reconstruct native closure.
Actual worker-consumed file tamper checks remain. Inactive recovered originals
retain decoded groups earlier, an explicit memory tradeoff.

The joined native witness and candidate selection gate passed 26 focused tests;
normal library compilation also passed. Review found a zero-group package
collision and it was repaired at the shared admission entry point, with hot and
legacy refusal tests. Evidence: `original-native-witness-final/`. These are
component proofs; post-change production latency remains unmeasured.

Candidate records now encode opaque buffers as CBOR byte strings. A retained
2,603,090-byte original product previously occupied 4,899,981 encoded bytes as
an integer array. Candidate cache namespace/framing/version is invalidated
explicitly; original TPMOD, interface, package and native version identities
remain unchanged. Typed publication dispositions and opt-in per-owner counters
will identify why stable Core support remains fresh. The earlier explanation
that Core was a generated request target was disproved: its actual dependency
source is an absolute content-addressed path.

An equivalent actual retained `Tidepool.Agent.Reply.Internal` candidate record
shrunk from 5,590,951 to 3,223,193 bytes. Separate unoptimized Rust processes
preserved all original buffer hashes and the module version. Ten warm rounds
per mode measured median body encoding 108.261 to 0.707 ms and decoding
495.372 to 3.238 ms. Eight focused tests and both benchmark processes passed;
independent review found no migration blocker. These measure the CBOR body,
not compiler execution, hashing, filesystem cost or production wall time.
Existing nonopaque record vectors still have no per-domain fanout cap.
Evidence: `core-publication-codec-benchmark/`.

The current Rust CLI component compiled with the joined native witness,
byte-string cache and launch tracing changes. All recorded source files and
submodule revisions matched before and after the build; the immutable binary
SHA-256 is `ae4b732ba7091155a2cda8c9e2a0b8b97818b0abd11799a94acf7eb9f9ed4f49`.
This is an unoptimized component build with 69 existing facade warnings,
not final full-source acceptance or a matched current compiler run.
Evidence: `candidate-current-rust-build/`.

Host launch now forwards both `TIDEPOOL_TIMING` and `EXOMONAD_TRACE`, so the
opt-in owner counters can reach the structured trace consumer. The focused
launch environment test passed and compiled current library consumers.
Evidence: `host-timing-environment/`. Actual next-run event observation remains
required. Offline report hashing uses bounded streaming; three comparisons on
a 135 MB binary retained identical digests and reduced process peak RSS from
about 147 MiB to 18 MiB. All 22 report tests passed. Evidence:
`performance-report-streaming/`; this is report-generation memory, not runtime
latency.

The composed capture scenarios are not accepted. A trace established that
cold-debug preparation consumed the former 90-second fixture deadline, so the
semantic fixture now has an explicit 300-second cell budget while typed-reply,
control and optimized performance budgets remain unchanged. Both setups then
completed; parent compilation exposed missing executable GHC dependencies for
quasiquoters and rejection of authenticated checked-value imports. Repair these
owning compiler boundaries before rerunning the child/failure compositions.
Evidence: `g3-capture-budget300-20261001T094417Z/` (diagnostic source-overlap
limit recorded).

Two new runtime publication tests passed through the actual staging and
publication owner, each in durable and ephemeral modes. Cancellation before
publication leaves public snapshots and manifest bytes unchanged; publication
before cancellation preserves the committed winner after private retirement and
GC. They are deterministic ordering tests, not actor cancellation transport or
in-flight rename fault injection. Evidence: `m2-publication-linearization/`.

Finish full M1 browser acceptance, the composed M2 capture/publication/recovery
scenarios, and the complete compiler/native boundary. In particular, retain
executed evidence for both cancellation/publication orders, two capture children
before parent completion and later parent failure, and nonempty recovery with
corruption refusal. Existing component tests are not substitutes for these
compositions. Reload preparation remains owned work while publication stays
serialized; parked cells must not prevent independent cells or controls from
progressing.

The runtime source contains all six ignored durable B0/B100 × N1/N10/N100
fixtures and emits owner-issued checksum encode bytes, recovery
validation/materialization hash bytes and manifest write bytes. Inventory's
`structural_whole_graph_copies` is structural evidence, not a measured copy
counter. This implemented coverage does not establish an accepted executed
six-row matrix or its performance results.

Materialize and exercise the matched immutable support catalog. Complete five
packaged cold-start measurements, exact-operation eight-actor attribution, the
durable B0/B100 by N1/N10/N100 matrix, and actual publication cost counters.
Retain per-run executed host paths and hashes, real daemon epochs and compiler
optimization provenance. Unknown costs are not zero. Existing latency limits
below remain required; repair demonstrated failures at their owning boundaries.

Root owns M1/browser integration, joined checks and commits. Component owners
edit disjoint files on canonical main in both repositories; shared-file insertions go through
the owning editor. Freeze source inputs for compilation and acceptance. Use
Luna for bounded fixtures, runners and documentation, Sol 6.1 for component
integration, and fresh Sol reviews of accepted M1/engine/M2 candidates. Root
continues implementation while coordinating. Final delivery includes the
verified package, retained closure/source bundles, evidence and browser launch,
status, resume and stop instructions. No scripted live assignment is required.

## Accepted contracts

- Declarations separate inference segments. Preserve ordinary whole-do inference
  inside each executable segment and check every segment before effects.
- The toolchain returns one immutable `CellProgram` from one admitted compiler
  transaction, containing ordered prepared items, original identities, result
  interfaces, exact dependency requirements and presentation plans. Multiple GHC
  passes inside the transaction are permitted. Runtime must not reconstruct and
  compile items after earlier effects. Publication join compilation stays separate.
- One toolchain-owned artifact inventory represents original module products,
  value interfaces and lexical join interfaces. Use petgraph `StableDiGraph` with
  typed direct dependencies and shared rooted views. Persist artifact IDs, never
  graph indices. Keep lexical selection, interface dependencies and native group
  requirements distinct. No per-cell whole-graph or historical payload clones.
- Future Val interfaces authorize typechecking only. Actual native imports need
  exact completed binding leases. Original identities and hidden dependencies
  survive shadowing, captures, publication and durable hydration. Lost live values
  stay unavailable after restart; never replay source or effects to restore them.
- Configured compiler authority is distinct from the observed worker identity.
  Admission requires both deployment authority and the exact consumed producer.
- A kernel execution owns cursor, continuation, reply, cancellation, private writes
  and resources. Ractor owns scheduling and actor lifecycle. Keep typed stateful
  actor protocols non-reentrant, while independent notebook executions progress.
- One qualified original-operation boundary contains origin/incarnation, request
  and original provider call. Nested local invocations remain beneath that
  boundary. Dispatch, captures, acknowledgments, reconciliation and fork cleanup
  retain it unchanged. Internal route boundaries use a typed alternative.
- Invocation cancellation uses the existing native safepoints with an invocation
  flag, not the shared actor realm flag. External/compile owners report actual
  settlement. Preserve the existing cancellation/publication commit decision.
- Publication atomically joins the final declaration/binding delta into the latest
  public environment; stale joins restage without repeating effects. A returned
  capture independently retains its completed private prefix and survives parent
  failure. Child admission still revalidates current authority.
- Startup atomically journals admission plus exact intent; the actual actor stays
  in Boot with its original entry and sealed inventory. Confirm manifest ownership,
  then exact Store binding, then durable ApplicationBound, then release original
  initialization once. No tools/provider/authored work before the activation gate.
- ApplicationBound precedes release and means activation may have happened. A
  crashed unactivated intent can roll split owners directly to a fresh admitted
  incarnation through exact CAS and a linear journal-proven intent chain. Never
  reactivate old live authority or weaken the bootstrap inventory fence.
- New artifact and startup journal formats may reject old versions. The user
  explicitly permits breaking experimental compatibility: preserve old state with
  typed refusal, omit migration machinery, and never infer missing evidence.

## Owners and integration order

Root owns operation identity, harness/facade composition, recovery integration,
joined verification and delivery. Component owners scaffold their public contract
and notify dependents before consumer edits. Use canonical branches with disjoint
file ownership for this delivery wave; preserve older worktrees. Root reviews
and commits exact ready files; no automatic application of historical patches.

1. Compiler owner: Haskell parser/segment checking and complete cell producer;
   coordinate the Rust immutable program boundary with artifact/runtime owners.
2. Artifact owner: petgraph inventory, retained interface kinds, configured
   producer admission and declaration context integration.
3. Runtime owner: consume complete cells, native completed-prefix authority,
   atomic publication, captures and new durable artifact graph.
4. Actor owner: keyed executions, prepared effect boundary, invocation cancellation,
   exact cleanup and deletion of whole-behavior serial fallbacks.
5. Startup owner: journal intent/activation contract, pending root and child
   initialization, confirmation/re-entry and exact startup release.
6. Performance owner: production resident measurement and immutable compiler reuse.
7. Buck owner: optimized matched host/worker profile, generated edges and packaging.
8. Fixture owner: required-worker gates, deterministic fixtures and evidence tools.

Use Luna for bounded leaves and Sol 6.1 for component ownership/review. Up to 16
workers may be used for meaningful independent work, not duplicated ownership.
Root works the integration lane while coordinating. Broad acceptance runs only on
joined candidates; compile every changed target and execute focused owner tests.

### Completion policy

The user selected completion followed by measurement-led performance work.
Finish the in-flight repairs and known correctness defects, freeze the joined
engine/harness revision, and execute functional acceptance and the specified
latency gates. Performance failures remain delivery defects; do not waive a
missed target or replace measurements with counts of passing unit tests. Record
further unmeasured optimization ideas for follow-up instead of continually
expanding the acceptance candidate.

Component owners proceed through routine implementation, focused checks and
repairs without asking for permission again. Escalate shared API/authority
decisions and actual blockers. Reuse each component's checkout and private Cargo
target for compatible checks; never share a mutable Cargo target across checkouts.
Use the pinned direct-tool environment for extractor-free tests. Batch reviewed
component commits, compile their combined production consumers before distributing
the next baseline, and repair a failing join once at its owner. A running acceptance
check keeps its recorded source and binaries when unrelated integration advances.

First vertical compiler gate: native bind -> declaration importing that value ->
expression yielding 42. Preserve original module/binding identities. A bad final
item must execute zero effects. Keep reviewed parser/reservation/publication
primitives, replacing unfinished per-item issuer machinery as needed.

### Immutable support compilation

Cold-start investigation found that the package supplies immutable library
source but no compiler products for it. The existing GHC candidate admission
already skips lowering and product emission for accepted modules. Extend that
owner rather than introduce another compiler cache. This parcel is required
before claiming the packaged cold-start target; its speedup remains unmeasured.

- The artifact owner exports a closed `Tidepool.Prelude` support cohort through
  the existing compile front door after dependency and target certification.
  Keep original product bytes, owners, skinny interfaces and package witnesses.
  Runtime-generated Effects dependencies and live values are excluded.
- For the first version, compile and consume at identical canonical immutable
  deployment roots. Nix supplies the final source root and product catalog;
  Buck declares the resources it consumes. Do not relocate authored identities,
  copy an ordinary cache tree into a package, or invoke Buck at runtime. The
  initial producer action is batched; per-module files alone do not establish
  independent per-module build actions or cache granularity.
- The configured catalog binds source, producer, paths and file content. A
  malformed, missing or mismatched configured catalog fails with a typed error.
  Current GHC source selection, package interfaces and closed dependency checks
  still decide which valid candidates can be reused. Shadowing invalidates the
  affected candidate and its importers; ordinary authored-cache rules stay intact.
- The facade represents frozen runtime libraries as captured sources or a pinned
  deployment source root. Authored actors remain captured. Reuse deployment
  sources directly, avoiding a second copy in every run. Resume refuses changes
  of mode, source, producer, catalog or canonical deployment path. Older frozen
  workspace formats may be refused while retaining their bytes.
- Begin with ordinary startup and empty exact contexts. Subsequent private
  contexts inherit only admitted originals through their existing artifact graph;
  never inject the entire packaged instance environment into every context.

Sequence: artifact API and strict exporter; facade source-root composition;
declarative package wiring; joined production verification. Independent owners
may prepare these components concurrently against the same frozen API. Root
reviews their join and the package closure before running the cold gate.

Acceptance requires fresh-process reuse with empty user caches and zero fresh
lowering or product emission for the packaged cohort, followed by real native
evaluation and display. Exercise instances/families, source shadowing, changed
source/package/producer, missing closure and product tampering. Confirm later
exact contexts retain original owners and refuse lost live values. The installed
package must run without Buck or build scratch paths. Retain optimized timing
separately from the existing debug investigation.

## Acceptance

- M1 real Engine/Store/browser: raw Haskell and installed typed tools; retained
  output; input inclusion; request-pinned reload; interrupt/continue; compaction,
  late output, reconnect and lost acknowledgments without replay. Repeated
  provider call IDs across requests cannot release/reconcile another operation.
- Record and repair the current browser native failure with bounded typed evidence;
  it occurs before Sleep and is not an established timer/cancellation defect.
- Separate-process nonempty recovery, crash at every startup transition, repeated
  split-owner failures, uncertain writes, stale completions and lost release ack.
  No premature readiness or replay. Unsupported old records remain untouched.
- M2: A parks/B publishes/A resumes preserving B; two parked plus a progressing
  third/control; completion-order shadowing; invalid joins publish nothing; two
  capture children reply before parent completion and survive later failure;
  cancellation, partial launch, revocation, drain and last-reader reclamation.
- Compiler/native: all segment semantics, exact original/hidden/qualified/instance/
  family facts, future-interface native refusal, producer/source/package/boot
  invalidation, fresh CAFs, demand omission, rollback and final-owner reclamation.
- Optimized production composition with actual daemon/PID/epoch evidence:
  at least 50 varied warm simple cells including display, p95 <= 1 second;
  five cold packaged reference-workspace starts, each <= 10 seconds;
  active cancellable-effect acknowledgment p95 <= 250 ms, cleanup separately.
- B0/B100 x N1/N10/N100 plus eight-actor workload: record queueing, graph visits,
  closure selection, copies, decodes, hash/write bytes, retained memory and release.
  Zero whole-graph copies per cell, no per-item compiler round trips after effects,
  no repeated payload publication for unchanged durable artifacts.
- Complete structural corpus and embedded producers, matched Rust/Haskell/browser
  checks, Buck cache/invalidation and packaged startup. Missing required workers
  must fail acceptance, not silently pass. Capture exact source/pair/binary hashes,
  commands, executed counts, exits, logs and cleanup.
- Fresh-context Sol 6.1 adversarial reviews of exact accepted M1, engine and M2
  revisions. Finish selected commits, evidence, verified bundles and live-trial
  packet. No milestone closes on compile-only or neighboring-test evidence.

## Server use

Use existing `tidepool-completion-build.slice` (88 GiB high, 104 GiB maximum,
2 GiB swap) and actual bind-mounted Buck outputs. Preserve shared daemons.
Start production compiler measurement with two workers and explicit 10240 MiB
rotation ceiling, measuring peaks and replacement overlap before expansion.
Admit expensive work through the existing slice; bound build parallelism and
measure aggregate memory. Remote execution remains disabled. Do not build Codex
with Buck or put runtime compilation behind Buck.
