# Resident UX wave: implementation and validation

## Intended outcome

Make ordinary resident Haskell easier to discover, inspect, and reuse during
context-tree campaigns. Keep campaign-specific types and functions authored by
the model. Finish with a computation-heavy Astra medium root / low child
exploration and interview, retaining useful actors, worktrees, and history.

## Implemented surface

- Prompts teach targeted discovery and reusable local projections.
- Long single-line `Show` output receives conservative, quote-aware whitespace
  layout. This does not evaluate values again or summarize domain records.
- Initial and retained request activations show both `sessionInput` and
  `respond` signatures using the same reply-signature owner as compilation.
- Explicit child retirement retains the acknowledging supervisor, allowing the
  host to suppress that supervisor's redundant lifecycle notification. Terminal
  state, pending-request outcomes, and watch transitions remain authoritative.
- Provider observations expose first and latest durable source observations,
  with source identity and timestamp. Polling time does not supply activation
  attribution; equal token counts do not collapse distinct source observations.
- The existing launcher prints selected tools/model/effort, points preflight
  errors to the supported recipe, and waits boundedly for registry ownership.

The `TokenUsage` Rust type moved to `tidepool-model`; its serialized fields are
unchanged. Generated Haskell context/roster records replace parallel usage
fields with optional `ProviderUsageObservation` records. Generated consumers
and facade exports change together; old accessor compatibility is not retained.
Existing Codex rollout files remain readable. Live Haskell values stay resident.

## Verification

- Actor unit tests: 64 passed after retirement and observation changes.
- Agent unit tests: 72 passed, 3 ignored.
- Worktree binding tests: bounded timeout and delayed release passed.
- Focused protocol ABI pins and generated-file freshness passed.
- Public Shoal Haskell facade test passed, including dimensional deadlines.
- Published guidance executed through the real resident tool, including typed
  forks, retained requests, watches, local projections, and new usage accessors.
- A repeat with heap verification and allocation pressure while responses were
  pending passed (567.754 seconds).
- Supported launcher built and started the matched local stack successfully.

The repaired exploratory acceptance run and both interviews are recorded in
[the final field report](astra-ux-exploration-field-report.md).

## Original failed exploration, retained for diagnosis

Run `f290a56b-9db3-4c07-9092-1c8209f41644`, tmux
`shoal-console-ux-wave`, isolated source clone `/tmp/shoal-console-ux-wave`:

- Root thread: `01a070a4-2f00-7242-9beb-2e65886e4ff7` (Astra medium).
- Coding thread: `01a070a5-39f3-7d90-80ea-15be6948599f` (Astra low).
- Research thread: `01a070a5-3a35-7ef3-a891-a5da60762dbc` (Astra low).

The root authored `Probe` and `Evidence`, forked a temporal investigator and a
read-only projection investigator, and registered an applicative settled watch.
The researcher authored resident row-quantization and wrapping-hash functions.
It found that visibly overlapping samples need not be numerically equal and
that resize-dependent noise can change values as well as their projection.
The coder demonstrated that zero motion speed does not freeze time-dependent
noise, while pause does, and committed a focused test candidate:
`bbda547c61278d91c4c8af85e9fa8c68dece9cb2` (not merged).

Both reply effects were accepted, then Haskell continuation execution failed:
code expecting `Tidepool.Internal.ExitCell.ExitCell` observed unrelated heap
contents, including `Data.FTCQueue.Leaf`. Polling the parent's watch also failed.
This is a harness failure, not evidence that rich Haskell responses should be
restricted. Root and children are paused and retained; accepted replies must not
be replayed. The root was explicitly instructed to stop recovery probing.

The coder reported a shared native cwd, but the transcript's actual `pwd` was
`/tmp/tidepool-actor-workspace`, the deliberately stable actor-relative mount.
That report alone does not demonstrate an isolation fault. The candidate used
the receipt's explicit worktree path. Future verification should compare branch
or sentinel identity, not pathname equality.

## Repair and acceptance evidence

The focused rich-response reproduction failed under heap verification immediately
after `let (temporal, visual) = workers` and the next binding. `deep_force`
replaces constructor fields with forced children; a tenured constructor can
therefore acquire a nursery pointer. That write bypassed the existing barrier.
The repair routes this write through a barrier-protected store. Thunk
memoization now uses that same store; the old tenure-time scan predicting
future thunk writes is removed. The barrier rejects movable nursery slot
addresses, while retaining stable old-space and external-array slots. The
focused barrier suite (5 tests) and forcing/store unit checks (8 tests) passed.
The failing run is retained
at `target/tidepool-test-runs/20260905T083703Z-1951180-battery`.
The complete GC suite passed (38 tests), as did both resident scenarios on the
final repair (690.266 seconds total), including the previously failing rich
response case with heap verification enabled. Formatting and diff checks passed.

The first barrier-only prototype also demonstrated why nursery slot addresses
must be excluded: it suffered an unprotected native fault and its test process
had to be terminated after the eval thread exited. The current implementation
has the central exclusion and a focused test for it. That prototype additionally
exposed a separate diagnostic-fault containment concern: a fatal signal outside
JIT protection can leave an invocation waiting after its eval thread dies.
Preserved evidence: `target/tidepool-test-runs/20260905T084403Z-1993691-battery`.

Acceptance is established by the repaired regression plus the final exploration:
rich replies and heterogeneous folds in the executable guidance; new post-fork
types, retained follow-up, reusable views, typed observations, explicit stops,
and root/child interviews in the live run. The host log confirms watch delivery
without additional retirement wakes. First-child counts were independently
checked against both durable rollouts. Candidate history and worktrees remain.

The finishing pass also classifies unresolved suspension-site types as typed
source rejections with annotation guidance. All three public surface tests pass,
including rejection classification and concrete function-valued request results.
Canonical fixture regeneration changed only the source fingerprint, not CBOR;
all 217 semantic fixture tests pass. The final actor unit suite passes all 64
tests, including the hosted description limit. No full workspace battery ran.

Deferred: selective context inheritance, mandatory campaign schemas, and new
inspection APIs. The unprotected-native-fault containment concern above remains
a separate robustness follow-up; it is not claimed repaired by the write barrier.
The next development milestone is using Shoal to develop Shoal.
