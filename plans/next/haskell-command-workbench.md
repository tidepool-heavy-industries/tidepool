# Haskell command workbench

Status: in-progress design, not an implementation assignment. This follows the
command-resource isolation release; it does not expand that release's acceptance
scope. API names below illustrate the desired experience, not existing exports.

## Direction

Make the resident Haskell environment the agent's programmable workbench for
commands, process interaction and coordination. Bash commands are inspectable,
transferable values, not merely opaque actions buried in an effect monad. Agents
can construct, inspect, modify and send a command before an authorized owner runs
it. Haskell replaces the competing JavaScript tool-orchestration layer.

Keep the surface fluent for GHCi-style use. Rust retains subprocess, PTY, resource,
output-buffer and cleanup ownership. Extend existing owners rather than building
another executor or process registry. Command descriptions convey intent, never
execution authority; the receiving actor runs under its own grants.

## Commands and results

```haskell
let search = [bash| rg 'CommandAllocation' tidepool-node |]
result <- run search

let tests = [bash| cargo test -p tidepool-node |]
              & withMemory (GiB 8)
result <- run tests
```

The bash quasiquote constructs a command value. Inspection should expose the
script and execution settings without launching it. Define working-directory and
environment semantics explicitly, especially when transferring a value to another
actor. Keep literal shell source separate from safely passed Haskell arguments;
implicit textual interpolation would recreate shell-quoting hazards.

Results remain Haskell values for inspection and composition. Model-facing output
is a deliberate projection, not a compulsory transcript dump. Preserve exit
outcome, stdout/stderr access and resource failure distinctions. Large output
needs bounded storage/access and explicit truncation, not an unbounded value sent
to the model. Do not automatically retry failed commands: side effects may already
have occurred.

## One memory setting

- Default command hard limit: **256 MiB**.
- An override such as `withMemory (GiB 8)` sets the command-tree hard limit.
- Admission accounts for that same value. The sum of admitted active limits must
  fit the host command-memory allowance. Replace fixed two-slot admission with
  this weighted capacity model.
- No separate model-facing reservation setting and no initial small/large lanes.
- No inline usage dashboard or token-preview ceremony. Record resource usage and
  admission/outcome evidence in logs for later analysis.

Keep the existing host aggregate boundary, OOM containment, queue cancellation,
identity and descendant-custody contracts. A shell exiting does not free capacity
while background descendants remain. Reject a request larger than total capacity
explicitly. Specify fair queue behavior so small jobs cannot indefinitely starve
large ones; sophisticated knapsack optimization is unnecessary initially.

The initial release's 8 GiB-per-command defaults remain unchanged until this
migration lands. Resolve swap policy alongside weighted admission; it must not
become an unaccounted escape from the aggregate policy. External service workers
such as Nix daemon builds are not descendants of the requesting shell and need
separate integration if they are to participate in these limits.

## Ongoing processes compose with actors

```haskell
job <- start tests
sendInput job "..."
closeInput job
result <- await job
```

A running process has a typed handle. Output and completion can feed the existing
owned Haskell routing actors; handlers may accumulate results or send ordinary
steering when model judgment is needed. Avoid waking an LLM for polling mechanics.
The input side replaces `write_stdin`; output observation uses routing rather
than a bespoke model-operated polling protocol. `run` is the convenient ordinary
case, while `start` supports continued interaction.

Reuse process ownership beneath the actor abstraction. Define cancellation,
terminal input/PTY selection, retained output and owner retirement against existing
mechanisms. A routing handler's lifetime does not prove process cleanup or release
its memory allocation. Delivery ordering, buffering and failed-handler behavior
must agree with the existing actor contracts.

## Editing

Keep native `apply_patch` for the foreseeable future: it is useful and familiar.
Offer precise Haskell editing as an additional capability, aligned with
[typed file tools](../typed-file-tools.md):

```haskell
edit "src/example.rs" $ do
  replaceOnce oldDefinition newDefinition
  insertAfter uniqueAnchor newFunction
  deleteOnce obsoleteClause
```

Read one file version, apply edits sequentially in memory, and commit the result
only when every operation succeeds. Missing or ambiguous matches abort the whole
single-file edit. Preserve untouched bytes and file metadata as appropriate.
Provide optional preview; do not force a separate approval/tool turn for every
ordinary authorized edit.

Require stale-source checks at the mutation owner. Atomic publication and
protection against concurrent writers are separate properties: rename alone does
not provide compare-and-swap, and advisory locking covers only cooperating writers.
Specify that boundary honestly before implementing. Do not promise multi-file
transactions initially. Reuse current file/patch owners, not a parallel filesystem
service. The existing typed-file plan owns detailed file semantics.

## Incremental migration

1. Inventory exposed execution and orchestration tools and production owners.
   Confirm the resident compiler supports the intended quasiquote path; implement
   a vertical command-value → existing resource owner → typed result slice.
2. Add weighted admission with the single hard-limit setting and focused failure,
   cancellation, fairness and descendant-retention checks.
3. Ship concise examples and developer guidance directing potentially expensive
   commands through Tidepool. Keep direct shell tools available during adoption.
4. Remove `functions.exec` early from Shoal-managed Codex tool exposure: Haskell
   owns composition. First ensure useful nested capabilities remain accessible
   directly or through Haskell; removing the wrapper must not strand tools.
5. Add typed ongoing-process interaction/routing and useful single-file editing
   through existing owners. Keep `apply_patch` exposed.
6. Improve from actual use. Retire direct shell tools only when effectively unused
   and remaining recovery/interaction needs are covered.

This is a parallel option first, not an immediate universal execution mandate.
Developer guidance is the initial adoption mechanism. Until bypasses are removed,
resource accounting covers work routed through the owning mechanism, not every
possible subprocess in the system. Keep the runtime/tool definitions and examples
consistent; avoid changing live-wave core surfaces mid-wave.

## Implementation questions still open

- Exact public command/result/handle types and effect signatures, chosen from
  production consumers and existing actor/process capabilities.
- Portable command-value encoding and transfer, cwd/environment binding, safe
  argument passage, and output access without incidental disclosure of secrets.
- Fair weighted queue discipline and swap accounting.
- PTY versus pipe selection and the smallest useful process-event source API.
- Single-file concurrency guarantees with native apply_patch and external writers.

Acceptance should exercise the resident Haskell surface end to end, including
inspect/send/run under recipient authority, hard-limit OOM with interactive control
survival, queued cancellation, descendants retaining capacity, routed completion,
and all-or-none file edits on ambiguous/stale input. Use deterministic local
fixtures; paid inference and an elaborate evaluation program are unnecessary.
