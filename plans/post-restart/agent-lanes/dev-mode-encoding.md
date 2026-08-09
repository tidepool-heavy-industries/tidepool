# dev-mode-encoding — gate 1(a) + the eDSL contract algebra

PRD: `plans/self-iterating-harness/18-typed-subagent-spawning-prd.md`, sections
"Agent contracts use a Servant-style Generic record", "Servant-style agent
eDSL", and "Diagnostics are part of the API". Lane index:
`plans/post-restart/agent-lanes/README.md`.

You own `haskell/lib/Tidepool/Agent/Contract.hs` (and any new
`Tidepool/Agent/*.hs` you need) plus
`tidepool-runtime/tests/agent_mode_encoding.rs`.

## The two things you are delivering

1. **A gate-1(a) verdict**: does the Servant-style mode encoding
   `mode :- Call Question Decision` survive Tidepool's real extract and JIT
   with acceptable elaboration and diagnostics? GO, or take the named fallback.
2. **The contract algebra itself**: `Call`, `Notify`, `Tool`, `AsServerT`,
   the `compileTools` single-traversal shape, selector→snake_case naming, and
   the authoring diagnostics as `TypeError`s.

These are one job because they are one file. The spike is not a throwaway: the
encoding you prove is the algebra you ship.

## HARD territory boundary

generic-surface is a LIVE lane mid-queue. Until root announces their fold you
may NOT touch, import, or depend on:

- `haskell/lib/Tidepool/Form.hs`
- their Generic substrate / metadata utility modules
- `Harness.Prelude`
- `tidepool-mcp/src/preamble.rs`

Everything you write is a NEW file. For Generic reflection, use
**`GHC.Generics` from base directly** (`Rep`, `M1`, `Selector`, `selName`,
`Datatype`, `Constructor`). Rolling your own small traversal is the correct
move here, not duplication to apologize for — PRD 15 says the message/tool
interpreter is a separate interpreter from the form interpreter by design, and
gate 1(b) exists precisely because the two support different shapes.

Also: `Tidepool.Agent` is already taken by an unrelated module
(`haskell/lib/Tidepool/Agent.hs`, the harness answerer's capability row). Do
not rename or edit it. Sit under `Tidepool.Agent.*`.

## The spike question, precisely

PRD 18 asks for one real extract/JIT proof of:

```haskell
data WorkerTools mode = WorkerTools
  { askParent     :: mode :- Call Question Decision
  , requestReview :: mode :- Call ReviewRequest Review
  , reportProgress :: mode :- Notify Progress
  } deriving (Generic)
```

with

```haskell
data AsServerT m
type family mode :- endpoint
type instance AsServerT m :- Call input output = Tool m input output
type instance AsServerT m :- Notify input      = Tool m input ()
```

What is actually in question is a type-family application sitting in an HKD
field position, and what that does to dictionary elaboration and error
messages under Tidepool's extract. The PRD explicitly permits a class-based
implementation instead of open type-family instances "if that produces smaller
inferred terms or better errors under the JIT" — the authored *shape* is the
contract, the mechanism is yours to choose.

**Proof must be dynamic, not just a typecheck.** A module that compiles proves
nothing about the JIT. Build a `WorkerTools (AsServerT (M effs))` value with
real `tool` handlers, run `compileTools` over it, and then *execute* a
dispatch: feed a tool name and a structural argument in, run the handler, get
the encoded output back. That is what makes the elaborated dictionary code
actually run. Drive it from `tidepool-runtime/tests/agent_mode_encoding.rs`;
`tidepool-runtime/tests/generic_deriving_337.rs` and
`nullary_sum_generic_deriving.rs` are the nearest existing patterns for
compiling and running authored Haskell through the real pipeline.

## The fallback — take it without ceremony

The named v1 fallback is flattening:

```haskell
data WorkerTools m = WorkerTools
  { askParent     :: Tool m Question Decision
  , requestReview :: Tool m ReviewRequest Review
  , reportProgress :: Tool m Progress ()
  } deriving (Generic)
```

This preserves the selector/schema/handler invariant, which is the property
that actually matters. If elaboration or diagnostics are materially worse with
the mode encoding, **take the fallback and move on**. Do not spend the lane
rescuing the prettier encoding. Report the evidence that decided it: the
concrete failure or the concrete diagnostic comparison, not an impression.

"Materially worse" means something you can show: a compile that fails, a
compile time or term size that is dramatically larger, or an error message for
a realistic authoring mistake that no longer names the record and the selector.

## `compileTools` — the invariant, not just the function

```haskell
compileTools :: HasAgentApi tools
             => tools (AsServerT m) -> Either ToolCompileError (CompiledTools m)

data CompiledTools m = CompiledTools
  { declarations :: [DynamicToolDeclaration]
  , dispatch     :: ToolName -> StructuralValue -> m StructuralValue
  , synopsis     :: Text
  }
```

One field-ordered traversal emits BOTH the declaration and the dispatch entry
from the same leaf visit. That is the whole point: schema and dispatcher cannot
drift because there is one visit, and description and handler cannot drift
because they inhabit the same `Tool` value. If your implementation traverses
twice, you have written something that can disagree with itself — restructure
it. Make that invariant visible in a test: a record whose selectors and
endpoints are checked to produce exactly matching declaration and dispatch key
sets.

Per leaf: read the selector name → normalize to snake_case → validate as a
backend tool identifier → input schema from the `Call` input type → output
encoder from the `Call` output type → description and handler from the `Tool`
value.

Naming is one deterministic camelCase→snake_case conversion (`askParent` →
`ask_parent`). No `Named` override in v1.

`declarations` should line up with `tidepool_agent::seam::DynamicToolDeclaration`
(`{name, description, input_schema}`) — the Rust side of the same contract,
already committed. You do not depend on that crate; just do not invent a
gratuitously different shape.

Structural schemas/encoders: define your own minimal structural interpreter in
your new files, or coordinate with dev-structural-codec (lane 1(b)) who is
building exactly that for lists and recursion. **Talk to them via the TL rather
than each writing half a codec.** For your gate the schema can be shallow —
the deep shapes are their gate.

## Diagnostics fixtures

PRD 18 lists the authoring failures that must become concise source-level
`TypeError`s. Cover at least:

- the tool record does not derive `Generic`;
- a record field is not a supported endpoint;
- two selectors normalize to the same wire name;
- a normalized name violates backend identifier rules.

Split them honestly by mechanism, because they are not all the same kind of
error: some are genuinely `TypeError` (compile-fail fixtures), and some — a
wire-name collision across selectors, in particular — may only be detectable
at `compileTools` time, in which case they are `ToolCompileError` values with
a test, not a `TypeError`. Say which is which in the receipt rather than
claiming type-level coverage you do not have.

An error must name the agent record, the selector, the endpoint type, and the
smallest corrective action. It must NOT expose `Rep`, JSON-RPC, `codex-codes`,
or backend schema types. Write the fixtures as the actual expected message
text, and check the message text — a fixture that only asserts "it failed" does
not defend the property that matters here, which is that the message is good.

## Verification

- `haskell/CLAUDE.md` first — the rebuild/deploy steps and the Known Limits
  section. The Known Limits list will tell you in minutes what would otherwise
  cost you an hour of JIT debugging.
- Real extract/JIT via battery tier 2:
  `scripts/battery.sh -p tidepool-runtime -E 'binary(agent_mode_encoding)'`.
  **Never bare `scripts/battery.sh`.**
- `cargo check --workspace` before submitting; clippy + fmt clean.
- GHC slots are capped at 3 box-wide with two other waves live. Expect to
  wait. Batch your runs; do not poll.

## Deliverable

A receipt at `plans/post-restart/agent-lanes/receipt-mode-encoding.md`:

- the gate-1(a) verdict — GO on the mode encoding, or fallback taken — with
  the elaboration evidence that decided it;
- what the dynamic dispatch proof actually executed;
- the diagnostics fixtures, split into type-level versus `compileTools`-time;
- per-binary test counts, never exit codes.

## Operational rules (VERBATIM — do not paraphrase, do not skip)

- Every GHC-heavy run goes through
  `/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- <cmd>` (absolute
  path). NEVER `exclusive` mode.
- `export XDG_CACHE_HOME="$PWD/.cache"` before any tidepool-harness test shard
  (persistent per-worktree, not mktemp).
- Spawns pass an explicit `model: sonnet` (or `opus` for sub-TLs); never fable.
- Never path-unscoped `pkill -f`; scope kills to PID or full worktree path.
- Commit with `--no-verify`. Never `git add -A`. Repo-root `tmp/` is protected.
  Grep/Read over LSP.
- Inherited-red claims require a cache-consistent A/B in the dev's own worktree
  (same cache state both legs, diff absent vs present); diff-file-overlap
  arguments are invalid for global surfaces.
