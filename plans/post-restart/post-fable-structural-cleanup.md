# Post-Fable structural cleanup

Status: recommendation for work after the current Fable run finishes  
Research baseline: `79a2cc16b773b7d0645e24406ab46fe93cea2266` on 2026-08-09  
Live recheck: through `3788b141f513`; the checkout is still moving  
Scope: architecture and subtraction, not the current bug hunt

> **Execution stamp (Fable, 2026-08-10):** steps landed this pass, each
> verified per its stop condition — step 2 (lazy streams deleted, eager
> iterative lists; −5.9k lines), step 3a (symmetric generic sums +
> Maybe-tolerant decode + CodecSpike deleted + honest tool-schema
> `required`; ModelCodec's deletion is step 3b, stamped
> separately below), step 4 core (FormAnswer deleted BOTH
> sides; answers are plain JSON through the one generic decode; author
> contract is now `deriving (Generic, FromJSON)` — the PRD-14
> "Generic-only" headline was retired deliberately, since a Generic-only
> decode would preserve the second traversal this step deletes; positional
> form fields now a compile-time TypeError). Step 4's REMAINDER:
> `Tidepool.Form.Legacy` + the flat `FormSpec.fields`/`Field`/`FieldKind`/
> `EnumOption`/`Submission` vocabulary and its render/JS path were removed in
> the post-Fable cleanup. Step 5's
> dead-vocabulary half landed (RuntimeAgentEvent/Workspace/ToolCallId/
> Usage/Other deleted). Human steering on 2026-08-10 explicitly preserves the
> Call/Notify mode interpretation seam: it is unfinished, not dead. Steps 6-11
> untouched, per the interference warning and decision gates.
>
> **Step 3b execution stamp (2026-08-10):** `Tidepool.Agent.ModelCodec` is
> DELETED. The design point step 3a left open is settled by the human: *"the
> whole point is not having a codec, just using JSON is better."* There is no
> options record and no ported error machinery — the model boundary uses the
> vendored generic `ToJSON`/`FromJSON` defaults directly, wire keys are
> selector names VERBATIM (the snake_case normalization is gone; breaking the
> wire was explicitly fine), and decode failures are the plain vendored
> messages (`key "caveats" not present`). ModelCodec's JSONPath-carrying
> errors were NOT ported and are not a future item.
>
> The schema derivation was the one part of ModelCodec that was not a codec,
> so it survives as `Tidepool.Aeson.Schema` (`JsonSchema`), a peer of
> `Aeson/Value.hs` and `Aeson/FromJSON.hs` rather than an agent module —
> because what it describes is the vendored JSON encoding, not anything
> agent-specific. It reads the same `Generic` metadata those two read and
> imports their `GAllFieldsNamed` witness rather than restating it, so
> "if it has a schema, it encodes and decodes" is a type-level fact. It
> ABSORBED `Tidepool.Agent.Contract`'s `AgentSchema` (deleted; `Contract`
> re-exports `JsonSchema` and its tool `input_schema` is now the same
> derivation): the two had the same field-name and `Maybe`-optionality rules
> and differed only in that `AgentSchema` rejected every sum, which was drift
> — `FromJSON` has always decoded sums, so a tool input can now BE one.
>
> ModelCodec's runtime collision validation died with the normalization it
> existed for. Duplicate normalized field names are unconstructible (no
> normalization; two selectors of one constructor cannot share a source
> name), and the tag-vs-payload collision is a compile-time `TypeError` from
> `GAllFieldsNamed`, pinned on the agent surface by
> `agent_mode_encoding.rs::compile_fail_payload_field_named_tag`. Proof types
> `WorkerResult`/`ReviewNote` left the stdlib: the surviving one is declared
> in `subagent_one_cycle.rs`'s own fixture, where a caller's result type
> belongs.
>
> **Rebaseline stamp (Fable, 2026-08-09, run complete):** the run ended at
> `8441b352`. The two commits after the live recheck are immaterial to this
> plan's facts: a Fork constructor-coverage test in tidepool-handlers and a
> prompt-voice alignment in the self-harness framing text. Every suite has
> now run on this tip (see `plans/post-restart/codex-review-2026-08-08.md`
> items 14–20 for the verified red/green state this plan should treat as
> ground truth). Known interactions between that state and this plan are
> recorded in the items themselves; the load-bearing ones: step 2 retires
> the two sanctioned reds pinning sum-type REJECTION
> (`sum_type_rejected_at_compile_time`, `mixed_nullary_sum…` — locked
> decision: symmetric support replaces rejection); step 5 subsumes the held
> "realm step 4" registry-only migration (its gate — boot-lazy's legs — is
> now green, so fold that held work into step 5 rather than running it
> separately); item 20 (hs-boot iface regression) is extract-internal,
> outside this plan's scope, and must be fixed independently of it.

## Recommendation

The next pass should reduce the number of runtime mechanisms, not finish every
partially built abstraction. The repository has accumulated several parallel
answers to the same questions:

- how an evaluation stops and resumes;
- who owns a resident machine;
- how values cross Rust, Core, Haskell, JSON, and UI boundaries;
- how agents and event handlers are described;
- how generated effects and authored effects coexist.

Most of the desired behavior can be retained with fewer mechanisms. The target
is not a new unifying framework. It is a small set of ordinary, independently
useful components with explicit ownership:

1. one permanently rooted continuation store in the JIT machine;
2. one small resident-session executor used by REPL, harness, and self-harness;
3. ordinary `ToCore`/`FromCore` and shared generic JSON machinery, with thin
   representation policies for persistence JSON and strict model output;
4. presentation metadata for forms, without a second answer encoding;
5. one coupled agent lifecycle whose synchronous spawn is composition over an
   asynchronous handle, not a second lifecycle primitive;
6. explicit worktree observation, unless the live parent-to-child poke use case
   proves a driver-level wakeup is required;
7. normal Haskell and Rust source for non-mechanical effects.

Every cleanup change should state both what capability remains and what code or
state disappears. A change which introduces a shared layer but leaves the old
paths intact does not count as cleanup.

## Rebaseline before implementation

Fable is modifying the same area this document studies. Do not implement this
plan against the moving tree.

After Fable finishes:

1. Record the final commit and review the diff from the research baseline.
2. Repeat read-only reachability searches for each deletion target below.
3. Classify each target as still live, already removed, or changed enough to
   require a revised recommendation.
4. Preserve fixes from the bug hunt. This plan supersedes neither verified bug
   findings nor new production call sites.
5. Update this document before starting if a stated fact is no longer true.

The baseline already includes the harness registry unification. The remaining
registry issue is therefore not “merge two harness maps”; it is eliminating
duplicated lifecycle facts across the harness, resident session, and machine.

The live recheck also includes the first working coupled `SubagentSpawn` path and
`Tidepool.Agent.ModelCodec`. Those are material changes from the original
baseline. This document treats their working behavior as evidence to preserve,
not as a requirement to preserve every provisional interface or duplicate
generic implementation introduced with them.

### Decision summary

| Area | Default decision | Important qualification |
| --- | --- | --- |
| Lazy effect results | Delete deferred host streams | Retain eager, iterative list materialization for stack safety |
| JSON/model output | Merge generic implementation | Retain distinct external representation policies where required |
| Forms | Replace answer protocol; delete legacy | Keep presentation metadata and `FormQQ` |
| Agent runtime | Replace provisional backend interface with the next working async path | Retain the coupled allocation/binding transaction and Codex adapter |
| Continuations | Replace all reified variants with machine-owned IDs | Keep native parked threads for unreified timeout stacks |
| Registries | Delete duplicated lifecycle state | Extract shared code only after transition laws match |
| Worktree callbacks | Prefer explicit observation | Validate latency against the live child-poke use case first |
| Effect generation | Shrink incrementally | Keep ordered tag/arity contract; do not create a new DSL |
| Persistence | Correct contracts, then simplify | Keep stores separate; source hash becomes audit metadata |
| Historical prose | Purge last | Do not erase rationale before structural decisions settle |

## Current mechanism map

### Evaluation and continuation paths

| Path | What it does differently | Why it exists | Recommendation |
| --- | --- | --- | --- |
| Blocking `run` / `run_fragment` | Runs to completion on the current native stack | Simple evaluation and REPL worker execution | Keep as the completion path |
| Single-slot `run_suspendable` / `resume_suspended` | Stores one raw continuation pointer, with special nested-child root handling | First threadless Ask/harness implementation | Replace, then delete |
| Parked continuation registry | Permanently roots multiple frames by ID and supports grouped cancellation | Intended generalization of the single slot | Make this the sole threadless mechanism, but remove unused policy around it |
| MCP `SessionEngine::AwaitingAnswer` | Stores an external closure plus machine state in a public continuation table | Stateless request/response API | Rebuild on the machine continuation store; keep only MCP-specific admission/expiry |
| MCP `SessionEngine::Paused` | Parks a native thread while an arbitrary native stack is active | Timeout can occur where no reified JIT continuation exists | Keep as a distinct fallback |
| REPL Ask dispatcher | Blocks the resident worker thread on a response channel | Historical implementation predating general parked continuations | Migrate Ask to the threadless store; reassess whether other REPL work still needs a dedicated worker |

The important distinction is not REPL versus MCP versus harness. It is whether
the suspended computation has a reified JIT continuation. Reified
continuations belong in the machine store. A native stack interrupted at an
arbitrary timeout cannot be represented that way and may legitimately retain a
parked-thread path.

Relevant code includes
[`jit_machine.rs`](../../tidepool-codegen/src/jit_machine.rs),
[`resident.rs`](../../tidepool-runtime/src/session/resident.rs),
[`persistent.rs`](../../tidepool-runtime/src/session/persistent.rs),
[`engine.rs`](../../tidepool-runtime/src/session/engine.rs), and the
[`REPL worker`](../../tidepool-repl/src/worker.rs).

### Session ownership and visible state

There are currently several overlapping facts:

- the machine owns the actual parked continuation frames;
- `ResidentSession` records a pending hole;
- the harness registry records whether a session is suspended and repeats the
  hole identity;
- `NodeConvo` and `NodeState` record the user-visible conversation state;
- self-harness directly owns another resident session and repeats parts of the
  run/resume loop.

The machine should answer whether a machine continuation exists. A resident
session should map the domain-facing `HoleId` to that machine continuation ID.
A session registry should answer whether a caller currently owns the machine.
Conversation state may retain form/table/UI metadata keyed by `HoleId`, but it
must not be the authority for continuation liveness.

The harness registry should converge on an ownership state such as
`Available(machine) | Running`, not another copy of `Suspended { hole }`.
Availability and continuation state are independent: a machine may be safely
checked in while it contains parked continuations.

### Registry shape map

The repeated maps do not form one abstraction. They fall into two small
implementation shapes plus domain-specific stores:

| Registry/store | Owns | Distinct law | Cleanup/share decision |
| --- | --- | --- | --- |
| Machine continuation table | GC-rooted unsafe frames by machine ID | Root/unroot/cancel must agree with JIT and GC | Keep specialized in codegen |
| Harness `SessionRegistry` | Transient exclusive possession of a resident machine | Checkout moves the machine out; RAII must restore or terminate it | Strip continuation states, then consider a runtime-local checkout helper |
| MCP continuation map | Public ID, expiry/admission, captured output, native pause or machine continuation | Holds permits across request boundaries and evicts by API policy | Keep MCP policy; point reified entries at machine IDs |
| `WorktreeRegistry` | Durable receipt catalog | File-per-ID, typo vs lost vs corrupt distinctions | Keep domain API; share only private durable-row I/O |
| Worktree `BindingTable` | Durable single-writer lease history | Process-owner lock plus memory/disk rollback on bind/settle | Keep domain API; share only private durable-row I/O |
| RepoEvent subscriptions | Broadcast queues and overflow poison | Lexical subscriptions see independent copies | Delete if explicit observation replaces callbacks |
| Future agent registry | Durable agent/thread/worktree identity and mailbox/lifecycle | Reattach and steering need one owner across effects/restarts | Build only with the next working async path; do not parameterize a generic registry |

The plausible shared code is therefore deliberately small:

- a runtime-local exclusive-checkout primitive, after every consumer has the
  same `Available | Running | Terminated` law; and
- a worktree-private atomic JSON row helper used by receipts and bindings, with
  one explicit fsync/rename contract.

Neither warrants a new crate initially. In particular, a `Registry<K, V,
State>` framework would hide the very differences—GC roots, admission permits,
durable rollback, queue fanout—which need to stay visible.

### Value boundaries

The repository has several related but non-equivalent conversion systems:

| Boundary | Correct mechanism |
| --- | --- |
| Rust values to and from the Core heap | `ToCore` / `FromCore` |
| Persistent or external data | `ToJSON` / `FromJSON` under an explicit representation policy |
| Structured model output | A thin schema/decode API backed by the same generic implementation |
| Description of JSON | JSON schema generated under the same representation policy |
| Form rendering | Presentation metadata derived from the Haskell type |
| Parent/child execution inside one runtime | Ordinary in-heap values |

There is no need for a second generic “codec” universe. There is a real need for
strict structured model output: schema and decoding must agree, model-facing
field names are snake-cased, optional fields may be omitted, and unsupported
constructor shapes must be rejected. Put those differences in explicit options
over one generic implementation. Form answers can then be ordinary JSON decoded
by the same machinery. Parent/child values do not need serialization at all.

### Agent paths

The first end-to-end path is now live:

`SubagentSpawn` → `SubagentHandler` → `CoupledSpawner::spawn_one_cycle` →
`OneCycleBackend` → `CodexOneCycleBackend`.

This path creates or resolves a managed worktree, binds an agent identity,
starts a backend thread, runs one cycle, settles the binding, and returns a
typed receipt. That transaction is useful domain behavior. Its current sync
trait and one-cycle lifetime are explicitly provisional: the handler blocks for the
whole cycle and the Codex adapter owns an inner Tokio runtime. Durable agent
identity, mailbox/steering, reattach, and general runtime-event projection are
not yet integrated. Cleanup must distinguish the proven transaction from those
future promises.

### Orchestration layers

The harness and self-harness have genuinely different policy:

- the harness owns the agent conversation tree, model calls, forks, child
  answerers, and conversation logs;
- self-harness owns the authored outer loop, checkpoints, compaction, iteration
  policy, and operator interaction.

They should remain separate policy layers. The duplicated part is the mechanics
of checking out a resident session, running or resuming it, and reporting a
yield. Extract that small executor only after the continuation model has one
source of truth. Do not merge the two policy state machines.

### Persistence and observation

Four stores have different jobs and should not be collapsed:

- harness logs are an audit/replay record, not restart state;
- the self-harness checkpoint is restart state;
- the self-harness transcript is best-effort human-readable output;
- the worktree journal preserves the worktree manager's correctness baseline.

They can share a run identifier and a minimal event envelope for correlation.
They should not share one storage abstraction merely because they write files.

## Action plan

### 1. Delete transparent lazy effect streams

#### Why

`Response::Stream`, `ValueStream`, `ValueSource`, deferred tail thunks, and
`parked_streams` form a substantial lifetime and GC subsystem. The only
non-test producer found is `FsReadGlob`, and that handler has already expanded
the glob and read every file into a `Vec<FileRead>` before constructing the
stream. Only Core conversion and cons-cell construction are deferred.

The current cleanup rule is also unsound as an ownership model: stream entries
are cleared based on continuation presence, even though a lazy thunk may escape
into a persistent binding or completed value. A stream is not owned by a
continuation simply because both live in the same machine.

#### Change

- Make `FsReadGlob` return the complete list it already materializes.
- Remove `Response::Stream`, `ValueStream`, `ValueSource`, stream response
  helpers, deferred stream-tail host functions, stream IDs, and
  `JitEffectMachine::parked_streams`.
- Remove registry-guard conditions which couple stream lifetime to suspended
  continuations.
- Retain the iterative list-spine probe, dismantling, and
  `materialize_cons_list` behavior as an eager stack-safe response path. Move
  it out of `host_fns/streaming.rs` and rename it around list materialization;
  deleting lazy streams must not reintroduce recursive conversion or drop of a
  long `Value` list.
- Remove `TIDEPOOL_LAZY_RESULTS` and the lazy/eager configuration after the eager
  path has the relevant long-list regression coverage.
- If large glob reads later require backpressure, add explicit pagination or a
  bounded file API. Do not reintroduce an invisible lazy host bridge.

#### Capability retained

Callers still receive the same complete list. Long lists are still constructed
iteratively and remain stack-safe. The removed behavior is deferred Core
conversion and a machine-global iterator registry, not deferred filesystem IO.

#### Stop condition

No production runtime type or host thunk represents an effect-result stream,
continuation cleanup contains no stream-specific lifetime rule, and a long
eager list still materializes without recursive stack growth.

### 2. Use one generic JSON implementation; delete the parallel codec universe

#### Why

[`Tidepool.Aeson`](../../haskell/lib/Tidepool/Aeson.hs) defines the established
external value representation. Its generic `FromJSON` already supports tagged
record sums more broadly than `ToJSON`. The new 600-line
[`ModelCodec`](../../haskell/lib/Tidepool/Agent/ModelCodec.hs) is now used by
`spawnAgent` and proves a real model-output requirement, but reimplements the
entire generic recursion. `CodecSpike` adds a third, positional `{tag, fields}`
experiment in tests.

The representations are not accidentally identical. Persistence JSON keeps
exact selector names; model output snake-cases selectors, rejects positional
payload constructors, makes `Maybe` fields omittable/null, and emits a strict
schema. Those are policy choices, not justification for parallel generic
implementations. The agent tool schema is another separate and currently
shallow derivation: it does not cover the same recursive type shapes and includes
optional fields in `required`.

#### Change

- Introduce one internal generic representation implementation with explicit,
  small options for field-label transformation, tagged-sum/nullary style,
  optional omission/required behavior, and positional-constructor rejection.
  Do not invent a public descriptor language.
- Preserve the existing persistence representation as the default policy.
  Provide a thin model-facing `StructuredOutput`-style API with
  `outputSchema`/`decodeOutput` (names illustrative) selecting strict model
  options. Avoid “codec” as the public subsystem name.
- Extend generic `ToJSON` to be symmetric with the existing `FromJSON` support
  for named-field payload sums, and correct the stale `Tidepool.Aeson` comments
  which still claim those sums are rejected on decode.
- Make the agent tool-input schema and decoder use the same implementation and
  policy. Preserve exact input field names unless there is an intentional wire
  migration; only compiled tool *names* are currently snake-cased.
- Move any valuable recursive cases from `CodecSpike` into tests of the real
  generic JSON path, then delete the spike and its positional representation.
- Move `WorkerResult`, `ReviewNote`, and similar proof fixtures out of the
  production model-output module into tests or real domain modules.
- Once callers use the shared implementation, delete `GModelCodec` and the
  duplicate traversal. Retain the structured-output capability, not two copies
  of generic recursion.
- Reserve `ToCore`/`FromCore` for heap crossings. Do not name any replacement
  module or trait “codec.”

#### Capability retained

Rust/Haskell boundaries still exchange JSON. Persistence compatibility and
strict model output may use different named policies, but encoder, decoder, and
schema share one structural implementation. Runtime-internal values stay
runtime-internal.

#### Stop condition

There is one generic implementation for the supported algebraic data types.
Each external policy has a schema matching its decoder, and no production or
test-only parallel structural codec remains.

### 3. Collapse forms onto ordinary JSON; delete the legacy form protocol

#### Why

The generic form path currently has a type-derived shape plus a separate
Haskell/Rust `FormAnswer` language and web-side answer reconstruction. The old
flat `FormSpec.fields` protocol is also still public even though no Haskell
caller was found. These are parallel serialization systems around data which is
already JSON.

`FormAnswer.Product` even uses an array of key/value pairs to preserve duplicate
keys. That is not a useful browser-authored typed-form contract: the controls
are known in advance, and once an ordinary JSON object has been parsed,
duplicate-key preservation is not a meaningful guarantee.

[`FormQQ`](../../haskell/lib/Tidepool/FormQQ.hs) is unrelated output/UI syntax
and should not be removed.

#### Change

- Keep type-derived form shape as presentation metadata. It may be a small
  form-specific generic traversal; do not distort JSON Schema with labels and
  layout hints merely to claim one physical representation.
- Have `askUser` receive raw submitted JSON and decode it using the authoritative
  generic `FromJSON` path under the form input policy.
- Have the browser collector directly construct that target JSON: record
  objects, tagged record sums, strings for nullary choices, `null`/omission for
  optionals, and ordinary arrays where appropriate.
- Reject positional payload constructors for generic forms at compile time,
  matching the model/JSON boundary, instead of maintaining form-only numeric
  field keys.
- Treat `choose` as the selected JSON value and `chooseMany` as an ordinary
  JSON collection defined locally by those helpers.
- Delete the Haskell and Rust `FormAnswer` ADTs, wire answer encoders/decoders,
  and web answer reconstruction.
- Delete `Tidepool.Form.Legacy`, flat `FormSpec.fields`, `Field`, `FieldKind`,
  `EnumOption`, `Submission`, and legacy-only tests and examples.
- Keep form diagnostics and type-derived shape checks where they explain an
  actual user error.

#### Capability retained

Typed forms, generic nested form rendering, `choose`, `chooseMany`, and web
submission remain. Only the duplicate answer representation and unused legacy
API disappear.

#### Stop condition

A form has one submitted value: JSON. The form layer describes and displays
that value but does not define another recursive value language, preserve
duplicate object keys, or support a form-only constructor convention.

### 4. Keep the coupled agent behavior and finish the mode API

#### Why

The Haskell agent contract uses a Servant-style mode parameter, `Call`/`Notify`
markers, `AsServerT`, an infix type family, and a generic traversal. Only the
server interpretation is implemented today, but human steering identifies the
interpretation seam as intentional unfinished architecture rather than dead
abstraction.

Unlike the research baseline, there is now a working production path. The
generated Subagent effect reaches `SubagentHandler`, the coupled spawn
transaction, and the real Codex one-cycle backend. Its allocation, binding,
rollback/settlement, and typed receipt behavior should be retained. The
`OneCycleBackend` interface
is nevertheless a synchronous two-method prototype interface: it blocks the
effect handler for an entire cycle, the Codex implementation owns a nested Tokio
runtime, and its own docs say steer, interrupt, reattach, and events belong to
later work. It should not become the durable agent architecture by accretion.

The Codex adapter itself is useful containment. Its hand-written dynamic-tool
wire is justified where the upstream experimental protocol does not supply
stable generated types. The problem is the unused neutral framework around it,
not adapter isolation.

#### Change

- Preserve `Call`, `Notify`, `AsServerT`, `(:-)`, and the generic traversal.
- Document the intended next interpretation before extending the algebra, and
  keep `AsServerT` as the only implementation until that consumer is real.
- Do not collapse unfinished extension structure merely because only one
  interpretation is currently wired.
- Use the shared JSON conversion and schema from step 2 for both tool input and
  structured model output.
- Keep backend-specific protocol translation inside the Codex adapter.
- Delete the transitional `Workspace`/`WorkspaceAccess` API; managed worktree
  identity and per-cycle `cwd` already carry the live behavior.
- Delete unpopulated `RuntimeAgentEvent` variants rather than freezing a future
  event hierarchy. Populate a small neutral event only when the driver actually
  consumes it. Remove `AgentActivity::Other` if it remains only an extension
  bag; backend-specific detail can stay in structured logs.
- Preserve `CoupledSpawner`'s domain transaction, but do not widen
  `OneCycleBackend`. Replace it with the next working async path using one
  driver-owned session interface: start returns an agent handle; await,
  send/poke, interrupt, and event projection operate on that handle as proven
  necessary.
- Define synchronous `spawnAgent` as library composition of start plus await.
  It must not be a second primitive or a second ownership path.
- Put durable agent identity, backend thread ID, managed-worktree binding,
  lifecycle, and mailbox/steering state in one driver-owned `AgentRegistry`
  before checkpoint/reattach is attempted. The effect handler is not the
  durable owner.

#### Capability retained

The live server-side tool interpretation, Codex adapter, coupled allocation and
binding transaction, typed failure stages, and receipt remain. The removed
capability is hypothetical alternate interpretation and anticipatory neutral
API, not current behavior.

#### Stop condition

Every public agent abstraction has a production consumer or contains a
backend-specific incompatibility. There is no mode system for a single mode,
no sync and async spawn implementations with separate lifecycle rules, and no
provisional one-cycle trait widened into the durable registry.

### 5. Replace the continuation variants with one rooted machine store

#### Why

The single-slot continuation and its nested-child workaround encode constraints
which the newer parked-continuation table is meant to remove. Retaining both
means every owner must know whether it is handling a single pending child, a
nested stowed root, a parked frame, an external MCP continuation, or a native
thread.

The newer table is not integrated simply because its tests are extensive.
Current non-test searches find no caller of the parked-ID run/resume APIs;
RepoEvent and Subagent acceptance tests exercise them while the product harness
still uses the single slot. Treat the table as the candidate mechanism to land,
not as already-established architecture.

The parked table itself should remain specialized unsafe runtime code. It is not
a candidate for a generic registry abstraction.

#### Change

- Make the machine's permanently rooted continuation table the only storage for
  reified JIT continuations. Keep it specialized unsafe code, not a generic
  registry crate.
- Migrate in dependency order: first stabilize the JIT parked-ID API; then move
  `PersistentSession`/`ResidentSession`; then harness, REPL Ask, and MCP
  `AwaitingAnswer`; only then delete the single-slot API. Do not leave both
  mechanisms as permanent compatibility modes.
- Have `ResidentSession` own a domain mapping from `HoleId` to the opaque
  machine continuation ID. Replace singular `pending_continuation()`-style
  queries with lookup/enumeration appropriate for multiple holes.
- Let `NodeConvo` own only user-visible request, form, table, and classification
  metadata keyed by `HoleId`. It must not mirror the machine frame.
- Delete the single raw-pointer slot, `run_suspendable`, `resume_suspended`,
  nested-child depth/root transfer, `ChildSuspended`, and the marker-generic
  `SuspensionMechanism`/`Threadless` vocabulary.
- Make `run_child` an ordinary exclusive use of the machine. The JIT need not
  require a suspended parent, and a child which suspends can return its own
  continuation ID. If harness policy only permits child work while a parent is
  awaiting input, enforce that in the harness rather than in pointer-rooting
  machinery.
- Keep the native parked-thread timeout path, but name and document it as a
  fallback for an unreified native stack rather than another general
  continuation implementation.
- Use distinct ID types for machine continuations, public/API continuations, and
  form holes. Make conversions explicit at boundaries.
- Remove `RealmId` unless grouped cancellation has a live caller. If grouping is
  real, first check whether an existing session/node/agent ID is already the
  correct scope. Otherwise call it an execution or cancellation scope; do not
  grow a “realm” subsystem around an ID.
- Preserve the acceptance cases which prove multiple roots, cancellation, GC,
  and handler suspension, but rewrite them against the sole public path rather
  than retaining test-only entry points.

This does not require one machine for an entire self-harness cycle. A resident
machine can own multiple parked continuations without being globally shared.
Machine-sharing is a separate performance and product decision.

#### Capability retained

Single and multiple suspended computations, nested child work, cancellation,
MCP answer resumption, and REPL Ask all remain. Arbitrary timeout interruption
retains the native-thread fallback.

#### Stop condition

Any reified continuation is stored and rooted in exactly one place. There is no
single-slot API, no child-suspension prohibition, and no owner mirrors raw
continuation liveness. The parked-ID APIs have product callers, not only tests.

### 6. Simplify session ownership, then share only proven lifecycle code

#### Why

Registries repeat a useful shape—checkout, exclusive ownership, return,
terminate—but they do not all own the same resource or transitions. Abstracting
the `HashMap` shape now would produce a generic registry framework while leaving
domain state duplicated.

#### Change

- Reduce the harness session registry to ownership state such as
  `Available(machine) | Running` plus termination. Remove `RunningChild` and
  `Suspended { hole }`; all operations use the same exclusive checkout path.
- Keep a visible/current hole in conversation state only where UI or policy
  needs it; name it accordingly and do not consult it for machine liveness.
- Make the resident hole-to-continuation mapping and machine store authoritative
  for resume/cancel availability. Validate the public hole before checkout,
  then resume the mapped machine ID while holding exclusive ownership.
- After these migrations, compare the REPL, harness, and MCP ownership
  transitions. Extract a small lifecycle primitive into
  `tidepool-runtime::session` only if at least two live consumers have identical
  operations and failure rules.
- Do not create a new crate initially. Do not abstract a registry merely because
  both implementations contain a map and a mutex.
- Narrow `SessionEngine` to its actual MCP use. Its API registry may own the
  public continuation ID, machine plus machine continuation ID, admission
  permit, and TTL; it should not own a second resume closure. Remove unused
  `RenderPolicy` and `Retention` choices and claims that REPL and MCP already
  share this engine.
- Keep `SessionEngine::Paused` only for the native-stack timeout case. Rename
  `AwaitingAnswer` around its actual API role after the closure is removed.
- Reassess the REPL worker after Ask migration. The claim that suspension must
  pin the worker thread is already false for threadless Ask, while
  `ResidentSession` demonstrates controlled cross-thread machine movement.
  Keep the worker only for a remaining thread-affinity or unreified-timeout
  reason which can be stated and tested.

#### Capability retained

Exclusive checkout, termination, cancellation, and domain-visible pending
questions remain. Only duplicate authority disappears.

#### Stop condition

For every lifecycle fact, the code review can point to one authority. Any shared
helper has at least two current consumers with the same transition law.

### 7. Share resident execution mechanics without merging policy

#### Why

The harness and self-harness should not become one state machine, but they do
repeat run/resume/yield mechanics. Operator interaction also crosses an async
driver through a synchronous trait, `block_in_place`, blocking receive, and an
EOF sentinel of `{}` which is later treated as malformed user input.

Today the harness runs checked-out resident sessions through `spawn_blocking`,
while `SelfHarnessDriver` directly owns an `Option<ResidentSession>` and repeats
the outer run/resume loop. `OperatorGate` is synchronous; the web gate blocks on
a receive, the driver uses `block_in_place`, and the stdin fallback converts
transport/parse failure to `{}`. These are concrete seams to remove, not a case
for merging all orchestration policy.

#### Change

- Extract a small `SessionHost` or `ResidentExecutor` for exclusive machine use,
  run/resume by continuation ID, and typed yields after steps 5 and 6 establish
  the final lifecycle. It should not own conversation trees, checkpoints, or
  retry policy.
- Have harness policy and self-harness loop policy call that executor; do not
  merge their state enums or checkpoints.
- Make operator requests async with typed outcomes such as form submission,
  continue, cancel, superseded, and EOF.
- Let the web gate await its oneshot normally. Isolate blocking terminal input
  with `spawn_blocking` at the transport boundary.
- Separate transport failure/cancellation from invalid submitted data. Retry
  malformed answers, not disconnected transports.
- Combine operator methods only if they are the same request with
  different payloads. Do not build an operator framework.
- Remove `block_in_place` from the driver and the `{}` EOF convention.
- Preserve the current invalid-answer retry bound, but make exhaustion a typed
  policy outcome rather than a transport error disguised as invalid JSON.

#### Capability retained

Harness tree policy, authored self-harness iteration, web and terminal operator
input, retry on invalid answers, and restart checkpoints remain.

#### Stop condition

There is one resident run/resume loop, two explicit policy layers, and no async
operation is hidden behind a synchronous operator trait.

### 8. Prefer explicit worktree observation to the callback event subsystem

#### Why

The managed worktree, registry, journal, and monitor are real domain machinery
and should remain. The generic `RepoEvent` layer adds `withHandler`/`pumpEff`, a
subscription registry, per-subscription queues, overflow poisoning, polling
interval policy, and callback execution interposed before unrelated effects.

This is no longer unused scaffolding. The authored dogfood harness wraps each
live node in `withHandler (headChanged preparedTree) ...` so a parent HEAD move
pokes its children with rebase instructions. That behavior must remain. The
architectural question is whether it requires a generic scoped callback effect,
or only durable observation plus scheduler wakeup.

#### Change

- Keep `tidepool-worktree` core management, journal, snapshot, and monitor.
- Expose explicit observation through the Worktree effect, for example
  `worktreeChanges`/`observeWorktrees`, returning observations after a durable
  journal cursor from the existing monitor/journal.
- Rewrite `withLiveNode` around explicit driver/scheduler observation at a
  deliberate boundary, then call the existing `pokeChildren` policy with the
  observed head. Persist or derive the cursor so changes made while nobody is
  observing are not swallowed.
- Establish the required latency before deletion. If observation at turn/cycle
  boundaries is sufficient, polling is enough. If a parent commit must wake a
  running scheduler promptly, add a driver-level watcher/wakeup or an explicit
  `awaitWorktreeChange`; do not implement that wakeup as arbitrary Haskell
  callbacks interposed through every freer effect.
- Delete the generic `Event` effect, `withHandler`, `pumpEff`, subscription
  queues, overflow policy, callback combinators, and event-handler registry
  only after the dogfood child-poke path is migrated and its latency contract is
  met.
- Record the provisional-status crash repair/reconciliation gap as a known
  limitation. It is new feature work, not part of this cleanup.

#### Capability retained

Worktree creation, ownership, journaled state, monitoring, child pokes on parent
head movement, and deliberate event observation remain. The removed capability
is generic implicit callback interposition, not the live orchestration behavior.

#### Stop condition

Worktree observation is explicit in scheduler/control flow, the dogfood parent
to child signal still meets its latency requirement, and no generic
subscription runtime exists without another production need.

### 9. Reduce effect generation to the mechanical contract

#### Why

The ordered base-effect row is a useful single source of truth because effect
tag order is a runtime contract. The current effect-definition DSL goes much
further: it embeds large raw Haskell modules, callback algorithms, request
shapes, methods, and fake handler fields needed to fit authored/interposed
effects into one macro grammar.

`effect_defs.rs` is now over 2,100 lines. The agent addition extends the same
pattern: authored Haskell data and helper code live inside Rust string literals.
The generic JSON asymmetry also forces hand-written generated JSON instances for
error sums. Fixing step 2 removes some of the pressure which made the generator
baroque.

The valuable invariant is constructor order and arity—not having all source
code originate in one Rust macro.

#### Change

- Keep one small ordered manifest for the base effect row and numeric tags.
  “Single source of truth” means one authority per fact, not one physical file
  containing both languages.
- First move authored Haskell type definitions and helper algorithms for a
  representative small effect, helper-heavy `Fs`, and cross-language
  Worktree/Subagent into normal `.hs` modules. Generated modules may import or
  re-export them during migration.
- Keep the mechanical constructor/order/arity projection temporarily. Write
  Rust request enums and handlers as normal Rust where generation buys little.
- Enforce the remaining cross-language constructor name/order/arity contract
  with a narrow extraction or build-time check.
- After the authored source has moved out, reassess what remains. A small
  manifest/projection macro may be worth keeping; full replacement is not a
  goal in itself.
- Delete callback grammar, large raw-Haskell escape hatches, fake handler slots,
  and generated authored algorithms as their consumers migrate.
- Do this after step 8 so the largest event callback algorithm is not preserved
  in the replacement.
- Do not build a second code-generation DSL to replace the first one.

#### Capability retained

Stable effect tags, cross-language request agreement, ordinary handlers, and
authored higher-order effects remain.

#### Stop condition

Changing a Haskell helper does not require editing a Rust string literal, and
the generator contains no mini-language for authored control flow.

### 10. Clarify persistence and observation contracts

#### Why

The stores have distinct purposes, but comments and interfaces sometimes imply
stronger recovery or non-blocking guarantees than the implementations provide.
For example, observer APIs claim not to block while JSONL output performs
synchronous locked writes.

The checkpoint's whole-source fingerprint is also the wrong compatibility
boundary for a self-iterating program. Any edit to the harness source discards
`State`, compaction, and iteration history even when the `State` representation
is unchanged. The restore path already performs typed `FromJSON` decoding and
has a controlled fallback when decoding fails; source identity and state-schema
compatibility should not be conflated.

Finally, the checkpoint writer uses temp-write plus rename but does not fsync
the file or containing directory, while the harness audit log fsyncs each event
under its writer lock. Those can both be legitimate choices, but the comments
must not call the former crash-durable or leave the latter's latency cost
implicit.

#### Change

- Give outer self-harness execution and nested harness turns a shared run ID for
  correlation.
- Keep worktree journal identity separate from harness conversational events.
- State explicitly which data is restart-authoritative, audit-only, or
  best-effort.
- Make observer delivery buffered/async if non-blocking behavior is required;
  otherwise weaken the guarantee. A bounded background writer should expose an
  overflow/error counter rather than silently turning backpressure into driver
  latency. Do not keep a false contract.
- Remove a duplicate log observer if structured tracing already supplies the
  same data to the same consumer.
- Give checkpoints an explicit schema version and migration/rejection rule.
- Stop invalidating checkpoint state on the whole harness-source hash. The
  simplest policy is to attempt typed decode and fall back once with a clear
  incompatibility event; a stronger policy may store a fingerprint of the
  `State` JSON schema from step 2. Keep the source hash as audit/change metadata.
- Decide and implement the checkpoint durability promise. If surviving power
  loss is required, fsync the temporary file before rename and the directory
  after rename; otherwise document process-crash atomicity without claiming
  stronger durability.
- Reassess the audit log's `sync_all` per event. Retain it if acknowledged-event
  audit durability truly requires it; otherwise batch it. Do not merge the log,
  checkpoint, transcript, and worktree journal to solve this policy question.

#### Capability retained

Restart, replay/audit, human transcripts, and worktree correctness records
remain separate and understandable.

#### Stop condition

Every persisted file has one declared purpose, and its durability/blocking
contract matches its implementation.

### 11. Remove historical narrative and archive completed planning material

#### Why

The planning tree is currently 82 Markdown files and roughly 22,273 lines.
Across non-plan source/docs, a lexical sweep finds about 1,800 occurrences in
317 files of terms such as wave, lane, receipt, spike, falsifier, oracle,
landing, folding, realm, substrate, spine, rung, and “load-bearing.” This is a
triage signal, not a deletion rule. `SpawnReceipt` may be a real audit record;
the tree-walking `EffectMachine` may be a real independent oracle. Phase names,
sibling-branch history, and claims that code “has landed” are not architecture.

The newest agent and event files are especially dense with lane numbers, PRD
references, “until the fold” constraints, and stale future-tense statements.
Some worktree comments still describe the monitor or caller as unlanded even
though both now exist. These comments actively mislead once their campaign ends.

Do this last. The historical material is useful while the structural cleanup is
underway, and Git already retains it afterward.

#### Change

- Delete or move completed execution plans, status ledgers, and duplicated
  archaeology out of the live planning tree once their decisions are reflected
  in code or a current architecture document.
- Keep one current architecture document per subsystem plus small ADRs for
  decisions which still constrain future work.
- Rewrite production comments to state a local invariant, safety condition, or
  non-obvious reason. Remove branch history, phase names, validation rhetoric,
  and claims that a change has “landed.”
- Replace “load-bearing” with the invariant and failure it means. Replace
  “frozen” with the compatibility owner and change procedure. Delete
  “falsifier” prose once the enduring regression test has an ordinary behavioral
  name.
- Prefer conventional names: continuation, session, execution scope, event,
  checkpoint, adapter, and manifest. Keep specialized terminology only where it
  distinguishes a real domain concept.
- Review production module headers separately from plans. A header should say
  what the module owns now; it should not contain the implementation campaign,
  rejected-branch story, or future lane map.
- Replace prose assertions with types or tests where feasible.
- Include `CLAUDE.md` files in the review; they should describe current commands
  and constraints, not narrate completed campaigns.

#### Capability retained

Current architectural rationale and actionable operating instructions remain.
Historical detail remains in Git.

#### Stop condition

A new maintainer can learn the current design without reconstructing the order
in which agents invented it.

## Things to retain

Subtraction should not flatten real distinctions. Keep these unless new evidence
appears:

- the tree-walking `EffectMachine` as an independent differential-test oracle;
- eager iterative list materialization for long effect-result lists;
- `FormQQ`, which is separate from submitted form data;
- strict structured model output, backed by shared JSON generic machinery;
- the managed worktree core, journal, registry, snapshot, and monitor;
- parent-head-to-child-poke behavior in the dogfood harness;
- the native-thread pause path for timeouts with an unreified native stack;
- backend isolation around Codex protocol translation;
- the coupled agent worktree/binding transaction, rollback semantics, and typed receipt;
- separate harness and self-harness policy;
- separate restart, audit, transcript, and worktree-correctness stores;
- the ordered base-effect manifest and its stable tag contract.

## Guardrails for the cleanup

- No compatibility wrapper without a named current caller and removal date.
- No generic registry crate until two migrated consumers share identical
  transitions, errors, and ownership rules.
- No replacement codec universe, schema language, event runtime, or
  code-generation DSL. A thin structured-output policy over shared JSON
  machinery is not a separate codec.
- No state field which mirrors an authoritative fact only to make a local match
  statement convenient.
- No new “future backend” or “future interpretation” types without a working
  production path.
- No phase should end with both old and new paths enabled indefinitely.
- Prefer deleting an unused capability over documenting its hypothetical use.
- When behavior must remain distinct, name the reason directly rather than
  forcing it behind one trait.

## Suggested change boundaries

These steps are ordered to reduce interference and make each subtraction
reviewable. They need not map one-to-one to pull requests, but each merged change
should leave the tree in a coherent state.

1. Rebaseline after Fable and amend this document for changed facts.
2. Delete deferred effect streams while retaining eager stack-safe list
   materialization.
3. Unify generic JSON internals, migrate `ModelCodec` and `AgentSchema`, and
   delete `CodecSpike`/duplicate traversals.
4. Move forms to ordinary JSON and delete the legacy/form-answer paths.
5. Remove immediately unused agent APIs and simplify the Haskell contract.
   Preserve the coupled transaction; replace `OneCycleBackend` only together
   with the next working async agent path, never as a speculative standalone
   refactor.
6. Consolidate reified continuations in dependency order; then simplify session
   ownership and MCP policy.
7. Extract the resident executor and make operator interaction truly async.
8. Prototype explicit cursor-based observation against the dogfood child-poke
   path. Delete callback events only after its latency contract is satisfied.
9. Move authored Haskell out of effect generation incrementally, starting with
   representative small, helper-heavy, and cross-language effects.
10. Clarify persistence/observer contracts and replace source-hash invalidation
    with state compatibility.
11. Purge historical terminology and archive completed plans.

Avoid parallel changes in the JIT machine, resident session, harness registry,
and self-harness driver during steps 6 and 7. Those files jointly define
ownership and are where a partially migrated design is most dangerous. JSON,
forms, and agent cleanup can proceed independently only after confirming Fable
is no longer editing their shared files.

### Initial touch map

This is a review map, not permission to edit all files at once.

| Step | Primary files/modules to recheck |
| --- | --- |
| Streams | `tidepool-effect/src/dispatch.rs`, `handlers/fs.rs`, `host_fns/streaming.rs`, `jit_machine.rs`, `machine_state.rs` |
| JSON | `Tidepool/Aeson.hs`, `Agent/ModelCodec.hs`, `Agent/Contract.hs`, `Agent/CodecSpike.hs` |
| Forms | `Tidepool/Form/{GForm,Shape,Wire,Legacy}.hs`, `selfharness/operator.rs`, web form collection |
| Agent | `Agent/{Contract,Spawn}.hs`, `tidepool-agent/{seam,spawn,backend}.rs`, `handlers/agent.rs` |
| Continuations | `jit_machine.rs`, runtime `session/{persistent,resident,engine}.rs`, harness `registry.rs`, REPL `worker.rs`/`ask.rs` |
| Resident/operator | harness `engine.rs`, `selfharness/{driver,operator}.rs`, runtime `session/resident.rs` |
| Events | `handlers/event.rs`, `Tidepool/Event.hs`, `effect_defs.rs`, dogfood `Harness.hs` |
| Generation | `effect_defs.rs`, `effect_decls.rs`, `effect_glue.rs`, generated Haskell modules |
| Persistence | `selfharness/{persistence,observer,driver}.rs`, harness log writer |

### Required decision gates

- **Agent:** do not design reattach until durable identity and registry ownership
  are settled; do not widen `OneCycleBackend` in the meantime.
- **REPL:** before deleting the dedicated worker, identify the remaining
  thread-affine or native-timeout behavior. Ask suspension alone is insufficient
  justification.
- **Events:** write down the maximum acceptable parent-commit-to-child-poke
  latency and restart/missed-event rule before choosing polling versus wakeup.
- **Persistence:** choose process-crash atomicity versus power-loss durability,
  and audit durability versus batched throughput, separately.
- **Generation:** migrate authored source first; decide whether the residual
  mechanical macro is still objectionable only after seeing its smaller form.

## Final architecture test

At the end, each question should have a short answer:

- Where is a resumable JIT continuation? In the machine continuation store.
- Who may operate a resident session? The owner holding its checkout.
- Where is the current user-visible question? In conversation/policy state.
- How does an external value cross the boundary? JSON through the generic
  implementation under a named representation policy.
- How does structured model output cross the boundary? Schema and decoder from
  the strict model policy over that same implementation.
- How does a form answer cross the boundary? It is that same JSON value.
- How does an internal value cross parent/child execution? It stays a Core value.
- Who owns a running agent? The driver-owned agent registry; synchronous spawn
  is start plus await.
- How are worktree changes observed? An explicit cursor-based Worktree operation
  or a driver-level wakeup with the same durable cursor.
- What is generated for effects? Only the stable mechanical contract.
- What recovers self-harness execution? The versioned checkpoint.
- What explains the current architecture? Current docs, not execution history.

If any answer requires choosing among legacy, experimental, nested, generic,
and compatibility paths, the cleanup is not finished.
