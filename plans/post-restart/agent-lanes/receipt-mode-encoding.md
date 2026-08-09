# Receipt — dev-mode-encoding (PRD 18 gate 1(a))

Base commit: `a5e80a9e` (`feat(agent): scaffold PRD 18 containment crate + pinned
app-server fixtures`), plus the swarm-wide `ghc-heavy` per-run cap fix
cherry-picked at `15f66662` (`fc3363dc` on root's tip; verified
`.config/nextest.toml`'s `test-groups.ghc-heavy.max-threads = 1` after
picking). Deliverable: `haskell/lib/Tidepool/Agent/Contract.hs` (new file) +
`tidepool-runtime/tests/agent_mode_encoding.rs` (new file). No other files
touched.

## Verdict: GO on the Servant-style mode encoding

`mode :- endpoint` — a type-family application in an HKD field position —
elaborates on the real extract/JIT, dispatches correctly, and its
diagnostics fire with usable messages. The flattened `Tool m input output`
fallback (the PRD's named v1 fallback) was **not built in parallel** — there
is no compile-time, term-size, or elaboration-time comparison number between
the two encodings in this receipt, and none should be inferred. The GO call
rests on the mode encoding working cleanly on its own terms (compiles,
diagnoses, dispatches), not on a measured advantage over the alternative.
One real finding *did* surface during elaboration and is reported below in
full, because it changed the design.

## Finding: `HasAgentApi` as an empty class tripped a real extract-pipeline bug

The first working version of `compileTools`'s constraint bundle was a
zero-method class with only a superclass context:

```haskell
class (Generic (tools (AsServerT m)), GCompileTools (Rep (tools (AsServerT m))) m)
    => HasAgentApi tools m
instance (Generic (tools (AsServerT m)), GCompileTools (Rep (tools (AsServerT m))) m)
    => HasAgentApi tools m
```

Compiling a module that called `compileTools` through this class (smoke
fixture, not committed — `scratchpad/Smoke.hs`, ad hoc, not part of the
deliverable) failed the extract, not with a GHC diagnostic but with an
extractor-level error:

```
Error: Dangling NVar reference(s) — the emitted program references these but nothing binds them; forcing one at runtime would trap as an unresolved variable:
  0xfec5b4a8c8bbd5bd = C:HasAgentApi [Tidepool.Agent.Contract]
This is an extract-pipeline bug (a binding was renamed, culled, or missed by reachability) — not a user error.
```

Instrument: `tidepool-extract-bin`'s own diagnostics JSON, captured live in
`scratchpad/smoke5.log` (ad hoc local run, not part of the committed test
suite — this finding is reported from that log directly, not re-derived).
Reading: a class with no methods elaborates to a dictionary with nothing in
it; something in the reachability/culling pass dropped the `C:HasAgentApi`
dictionary-constructor binding while a reference to it survived elsewhere in
the emitted program — a real extract-pipeline bug, independent of whether
the authored Haskell is otherwise sound.

**Fix applied:** replaced the class/instance pair with a `ConstraintKinds`
type synonym:

```haskell
type HasAgentApi tools m =
  ( Generic (tools (AsServerT m))
  , GCompileTools (Rep (tools (AsServerT m))) m
  )
```

A synonym has no dictionary of its own — it macro-expands to the raw tuple
at every use site during typechecking, so there is nothing for the
reachability pass to cull. Confirmed clean in a rerun of the same fixture
(`scratchpad/smoke6.log`, same instrument): `tidepool-extract-bin` completed
with `{"version":1,"diagnostics":[]}` and wrote `result.cbor` (13015 nodes),
`meta.cbor` (183 entries), `asks.json` (0 sites) — no dangling-NVar error,
no other diagnostic. This is committed as-is in `Contract.hs`; the
class/instance version never reached the committed tree.

**What this means for the gate:** it is evidence *for* the mode encoding,
not against it — the bug was in an auxiliary constraint-bundling class, not
in the `mode :- endpoint` family application itself, and it has a clean,
general fix (prefer constraint synonyms over zero-method classes for exactly
this kind of bundling, in this codebase, until the extractor's culling pass
is fixed upstream). No other elaboration problem was observed anywhere else
in `Contract.hs`.

## Dynamic dispatch proof — what actually executed

Instrument: `cargo-nextest`, filter `-E 'binary(agent_mode_encoding)'`,
`--no-fail-fast` passed explicitly (`scripts/battery.sh` does not carry
`7d57cea5` in this worktree, per the swarm advisory). Nextest run ID
`938f5a93-41e2-4f5d-a112-5342d26a728f`, log
`/tmp/tidepool-ghc-detach.1MEjt4.log` (via `ghc-slots.sh detach`, slot1,
71s wall including a full workspace test-target build). Header line: `Starting
10 tests across 1 binary (258 binaries skipped)` — confirms the filter
selected only this binary, not the two other sanctioned-red binaries
(`generic_deriving_337`, `mock_stack_lockstep`) or anything else.

```
Summary [  68.222s] 10 tests run: 10 passed, 0 skipped
```

Completed vs selected: **10/10** (the filter selected 10 tests; all 10 ran
to completion and passed — `--no-fail-fast` means a mid-run failure would
not have truncated the count, and none occurred).

`dynamic_dispatch_executes_on_real_jit` is the load-bearing test — the one
that answers "does a module that merely compiles prove anything about the
JIT," which the spec explicitly says it does not. What it executes,
end to end, through `EvalHarness::run` (real extract → real Cranelift JIT →
real effect dispatch, not a mock oracle):

1. Builds `workerTools :: WorkerTools (AsServerT M)` where `M = Eff
   '[Console]` — a real one-effect row, not `IO`/`Identity`. `askParent`'s
   handler performs `send (Print ...)` (a real effect, handled by
   `tidepool_testing::eval_harness::mock::MockConsole`) before returning a
   `Decision`.
2. Runs `compileTools workerTools`, getting a `CompiledTools M` back as a
   `Right`.
3. Calls `dispatch compiled "ask_parent" (toJSON (Question "should we ship
   this quarter?"))` — a real dispatch call by wire name, not a direct
   handler invocation.
4. `dispatch`'s single generated leaf: decodes `Question` via `FromJSON`,
   runs the handler (which performs the real `Print` effect), re-encodes the
   `Decision` via `ToJSON`.
5. Rust asserts `out.is_ok()` and `out.json() == {"approved": true}` — the
   handler's own logic (`T.length (questionText q) > 5`) ran for real:
   `"should we ship this quarter?"` has more than 5 characters, so `approved
   = true`. A stub or a decode-bypassed dispatch could not produce this
   value from the wire-level `Question` payload alone.

`notify_endpoint_dispatches_through_the_same_path` dispatches
`"report_progress"` (the `Notify Text` field) through the identical
`dispatch` function and asserts the JSON result is `null` (`Tool m input
()`'s output, `()`, encodes as `Null`) — proving `Call` and `Notify` share
one dispatch leaf rather than being two code paths that could silently
diverge.

## Single-traversal invariant — named pass lines

Both tests below are direct pins of the property `compileTools` exists to
guarantee (declaration key set == dispatch key set, because both are
projections of one `GCompileTools` traversal result, `named`, never
re-derived). Instrument: same nextest run (`938f5a93-…`), same log.

- **`single_traversal_invariant_declaration_and_dispatch_keys_match`** —
  PASS, 9.850s. Asserts `map dtdName (declarations compiled) ==
  dispatchNames compiled` evaluates to `true` on the real JIT for
  `workerTools`.
- **`single_traversal_invariant_names_are_the_expected_two`** — PASS,
  7.678s. Guards against the first test being vacuously true over an empty
  list: asserts `dispatchNames compiled == ["ask_parent",
  "report_progress"]` exactly — both selectors present, in field order,
  correctly snake_cased.

## Diagnostics — named pass lines, split honestly by mechanism

Every fixture below is its own `#[test]`, asserts on message TEXT (not "it
failed"), and states which machinery in `Contract.hs` produced that text.
Instrument for all: same nextest run `938f5a93-…`,
`/tmp/tidepool-ghc-detach.1MEjt4.log`.

**On the sanctioned-red question (swarm advisory, inbox-2109315-7):** none
of the five fixtures below route through
`Tidepool.Aeson.FromJSON`/`Tidepool.Aeson.Value.ToJSON`'s vendored
generic-deriving sum-rejection machinery — the machinery
`generic_deriving_337::sum_type_rejected_at_compile_time` (currently
sanctioned red, cause not yet established) exercises. Checked directly:

- `compile_fail_tools_record_missing_generic` and
  `compile_fail_unsupported_endpoint_type` never derive `FromJSON`/`ToJSON`
  on a multi-constructor type at all — their errors come from GHC's own
  instance resolution and from `Contract.hs`'s own `mode :- endpoint` closed
  type family, respectively.
- `compile_fail_multi_constructor_call_input` uses `data Verdict = Yes |
  No deriving (Generic, FromJSON, AgentSchema)` — an ALL-NULLARY sum.
  `Tidepool.Aeson.FromJSON`'s generic default *accepts* all-nullary sums
  (enum decode via `GSumNullaryFromJSON`, no rejection) — `Verdict`'s
  `FromJSON` instance compiles and contributes no error here. The
  compile failure is entirely `Contract.hs`'s own `GAgentSchema (a :+: b)`
  `TypeError` instance, which (unlike `FromJSON`/`ToJSON`) rejects EVERY sum
  shape, nullary or not, because this gate's schema interpreter never
  special-cased enums. The asserted substring `"single-constructor records
  only"` is deliberately worded to echo `Tidepool.Aeson.Value.ToJSON`'s own
  message (shared phrasing, shared design precedent) but is a separate
  `TypeError` instance on a separate class (`GAgentSchema`, not
  `GToJSONSum`) — not the same binding, not affected by whatever is
  currently making the `ToJSON`-sum-rejection fixture red.
- `compiletools_time_duplicate_wire_name` and
  `compiletools_time_invalid_identifier` assert on `Text` values returned by
  `renderToolCompileError`, computed entirely by `Contract.hs`'s own
  `checkDuplicates`/`checkIdentifier`/`validIdentifier` — no JSON codec
  involved on this path at all.

Also confirmed: the `-E 'binary(agent_mode_encoding)'` filter is correctly
scoped — the nextest header line above reports `258 binaries skipped`, and
neither `generic_deriving_337` nor `mock_stack_lockstep` appears anywhere in
this run's log.

### Type-level (source-level `TypeError`, or GHC's own error)

- **`compile_fail_tools_record_missing_generic`** — PASS, 1.834s. Fixture:
  `BadTools` has no `deriving` clause at all. Mechanism: **not** an authored
  `TypeError`. `HasAgentApi` is a constraint synonym expanding to `(Generic
  (...), GCompileTools (Rep (...)) m)`; with no `Generic` instance, GHC's
  own instance-resolution error surfaces. Asserted substrings: `"BadTools"`
  (names the record), `"Generic"` (names the missing capability), and
  absence of `"JSON-RPC"`/`"codex-codes"`. **Deliberately not** asserting
  Rep-freedom here (see the code comment and below) — `classify_compile`
  joins every diagnostic GHC emits for the module, and since the constraint
  synonym expands to TWO constraints, GHC's solver can report a *second*,
  follow-on "no instance for `GCompileTools (Rep (BadTools ...)) Maybe`"
  alongside the primary one — and that second message legitimately contains
  the substring `Rep`. Investigated why an overlapping-instance rescue can't
  suppress this: it would need two `HasAgentApi tools m` instances with the
  *same head*, distinguished only by whether their context happens to be
  satisfiable — GHC's overlap resolution works on head specificity, not
  constraint satisfiability, so that rescue is not available. Reported
  honestly rather than asserted around.
- **`compile_fail_unsupported_endpoint_type`** — PASS, 1.766s. Fixture:
  `BadTools2 { oops :: mode :- Text }`. Mechanism: the `mode :- endpoint`
  closed type family's third (catch-all) equation, an authored `TypeError`
  — fires when GHC must reduce that field's type while building `Rep
  (BadTools2 (AsServerT Maybe))`. Asserted substrings: `"unsupported agent
  tool endpoint"`, `"Call input output"` AND `"Notify input"` (both valid
  shapes named as the fix), and absence of `"Rep "` — clean here because
  only ONE constraint (the type family reduction itself) is in play, not
  two independently-solved ones as above.
- **`compile_fail_multi_constructor_call_input`** — PASS, 1.592s. Not one
  of the PRD's required four; a bonus the `GAgentSchema (a :+: b)`
  `TypeError` instance buys for free (see the sanctioned-red analysis
  above for exactly which mechanism this is and isn't). Asserted substring:
  `"single-constructor records only"`.

### `compileTools`-time (`ToolCompileError` values, via `renderToolCompileError`)

Both below require inspecting the actual normalized STRING, not just types
— per the spec, that puts them here rather than in `TypeError` territory.

- **`compiletools_time_duplicate_wire_name`** — PASS, 8.610s. Fixture:
  `DupTools { askParent :: ..., ask_parent :: ... }` — a selector already
  spelled snake_case colliding with its camelCase sibling once both
  normalize (realistic, not contrived). Mechanism: `checkDuplicates` builds
  the `DuplicateWireName` value at `compileTools` runtime; text via
  `renderToolCompileError`. Asserted substrings: `"DupTools"` (record),
  `"askParent"` AND `"ask_parent"` (both selectors), `"\"ask_parent\""`
  (the colliding wire name), and (lowercased) `"rename"` (the smallest
  fix).
- **`compiletools_time_invalid_identifier`** — PASS, 9.859s. Fixture:
  `UnderscoreTools { _askParent :: ... }` — a leading-underscore selector
  (a real, common Haskell record-field convention), whose snake_case form
  still starts with `_`, violating the backend-identifier rule (must start
  `a`-`z`). Mechanism: `checkIdentifier`/`validIdentifier` build the
  `InvalidToolIdentifier` value; text via `renderToolCompileError`. Asserted
  substrings: `"UnderscoreTools"` (record), `"_askParent"` (selector), and
  `"lowercase letter"` (the violated rule, stated plainly).
- **`compiletools_time_well_formed_record_compiles`** — PASS, 7.699s.
  Negative control: `workerTools` compiles with neither `ToolCompileError`
  branch hit, so the two fixtures above are not vacuously passing against a
  validator that rejects everything. Asserts the eval result's `constructor`
  field is `"Right"` (the runtime's generic top-level-ADT rendering, `{"constructor":
  …, "fields": […]}` — distinct from `Tidepool.Aeson.Value.ToJSON`'s
  hand-rolled `Either` instance, since `result` here is a raw `Either Text
  Text` never passed through `toJSON`; this was in fact caught live by this
  very test on the first run — see Rework below).

## `cargo check --workspace` / clippy / fmt

- `cargo check --workspace`: clean, rc=0. Instrument: `ghc-slots.sh detach`
  log `/tmp/tidepool-ghc-detach.MPczZe.log`, `Finished \`dev\` profile
  [unoptimized + debuginfo] target(s) in 1m 03s`, `command exited rc=0`.
- `cargo clippy --workspace`: rc=0. Instrument:
  `/tmp/tidepool-ghc-detach.tllqw6.log`. All warnings in that run are in
  other crates (`tidepool-codegen::jit_machine::ResponsePlan`,
  `tidepool-harness::engine::TurnOutcome` — pre-existing `large_enum_variant`
  lints, not touched by this lane). Additionally ran `cargo clippy -p
  tidepool-runtime --tests` directly (single-crate, unwrapped, no slot) to
  confirm `agent_mode_encoding.rs` specifically: zero warnings attributed to
  it.
- `cargo fmt -p tidepool-runtime -- --check`: clean after one `cargo fmt`
  pass (the initial diff was pure rustfmt line-wrapping, no logic change).

## Rework during this lane (for the record, since correctness evidence should show its own history)

The first battery run (nextest run ID `3dc69c92-37e7-4997-95e6-8eed38a92d45`,
log `/tmp/tidepool-ghc-detach.vJlfuf.log`, superseded by `938f5a93-…` above)
reported `10 tests run: 7 passed, 3 failed` (same filter, same 10 selected —
`--no-fail-fast` ran all of them despite the failures), all three failures
test-authoring bugs, none in `Contract.hs`:

1. `dynamic_dispatch_executes_on_real_jit` failed to compile
   (`No instance for 'ToJSON Question'`) — `Question` in `SHARED_TYPES`
   derived `FromJSON`/`AgentSchema` but not `ToJSON`, and this test calls
   `toJSON (Question ...)` directly from Rust-side test source to build the
   dispatch argument. Fixed by adding `ToJSON` to `Question`'s deriving
   clause.
2. `compile_fail_multi_constructor_call_input` failed with `Ambiguous
   occurrence 'Choice'` — the fixture's local `data Choice = Yes | No`
   collided with `Tidepool.Prelude`'s own re-export of
   `Data.Profunctor.Choice`. Renamed the fixture's type to `Verdict`.
3. `compiletools_time_well_formed_record_compiles` failed on an assertion
   that assumed `{"Right": …}` shape; actual shape is `{"constructor":
   "Right", "fields": […]}` (see above). Fixed the assertion.

All three fixes are in the committed `agent_mode_encoding.rs`; none touched
`Contract.hs`.

## Boundary compliance

- Every file this lane wrote is new: `haskell/lib/Tidepool/Agent/Contract.hs`,
  `tidepool-runtime/tests/agent_mode_encoding.rs`. Verified via `git status
  --short` before and after — no other tracked file modified except the
  cherry-picked `15f66662` (two files, `.config/nextest.toml` +
  `scripts/ghc-slots.sh`, per the swarm-wide cap-1 fix, not this lane's own
  work).
- `Tidepool.Agent.hs` untouched; sits alongside as `Tidepool.Agent.Contract`.
- No import of `Tidepool.Form`, generic-surface's substrate, `Harness.Prelude`,
  or `tidepool-mcp/src/preamble.rs` — `Contract.hs`'s only non-base imports
  are `Tidepool.Aeson.Value` and `Tidepool.Aeson.FromJSON` (both pre-existing,
  unrelated to the generic-surface hold).
- No dependency on `tidepool-agent`; `DynamicToolDeclaration`'s field shape
  (`{name, description, input_schema}`, spelled `dtdName`/`dtdDescription`/
  `dtdInputSchema` in Haskell to avoid clashing with `Tool`'s own
  `description` field) mirrors `tidepool_agent::seam::DynamicToolDeclaration`
  without importing it.
- No codec overlap with dev-structural-codec: `AgentSchema`/`GAgentSchema`
  is a new, self-contained, deliberately shallow (no lists, no recursion)
  interpreter in `Contract.hs`; `Tidepool/Agent/CodecSpike.hs` and
  `tidepool-runtime/tests/agent_structural_codec.rs` were not touched or
  read for this deliverable.
- Operational rules followed: every slot-taking invocation went through
  `/home/inanna/dev/tidepool/scripts/ghc-slots.sh` (absolute path), `detach`
  once the swarm advisory landed (`run` before that, per the original spec
  — one instance of a `run`-while-queued leg was lost to the environment's
  process kill before `detach` existed; re-run cleanly after switching),
  never `exclusive`, `XDG_CACHE_HOME` exported for every extract invocation,
  `--no-fail-fast` passed explicitly per the swarm advisory, at most one
  brokered leg in flight at any time, no new slot-taking work started during
  the root-announced enqueue hold.

## Done-criteria checklist

- [x] Contract algebra typechecks and dispatch RUNS on the real JIT —
      `dynamic_dispatch_executes_on_real_jit`, PASS (see above).
- [x] Gate 1(a) verdict returned with elaboration evidence — GO, with the
      `HasAgentApi` dangling-NVar finding and fix as the evidence that
      actually decided something (no fallback comparison was run or is
      claimed).
- [x] `compileTools` single-traversal invariant pinned by a test — two
      named tests, both PASS (see above).
- [x] Diagnostics fixtures committed, split honestly type-level vs
      `compileTools`-time, asserting message TEXT — five fixtures, each its
      own named pass line, each quoted above with its actual asserted
      substrings and which mechanism produced them.
- [x] This receipt.
