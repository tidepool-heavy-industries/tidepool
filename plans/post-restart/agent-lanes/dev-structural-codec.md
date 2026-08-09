# dev-structural-codec — gate 1(b): lists and recursion through the structural codec

PRD: `plans/self-iterating-harness/18-typed-subagent-spawning-prd.md`,
"Communication types" and "First fork — independent GO/NO-GO gates" item 1(b).
Lane index: `plans/post-restart/agent-lanes/README.md`.

You own `haskell/lib/Tidepool/Agent/CodecSpike.hs` (and any new
`Tidepool/Agent/*.hs` you need beyond dev-mode-encoding's `Contract.hs`) plus
`tidepool-runtime/tests/agent_structural_codec.rs`.

## The one-sentence goal

Prove on the **real extract and JIT** that a list-carrying type and a
**recursive** ADT round-trip through a structural codec — the exact polarity
the operator-form interpreter rejects and this interpreter requires.

## Why this is a gate and not a formality

PRD 18: "The supported structural algebra for messages, tool inputs/outputs,
and results is a separate Generic interpreter from operator forms. It may
support lists and recursive containers even when `askUser` does not. The shared
Generic substrate must not collapse the supported set to the intersection of
all consumers."

If recursion does not survive the JIT, PRD 18's authored surface loses
recursive result types — and the recursive-delegation story (a worker parked in
`requestReview` whose handler spawns a reviewer returning a structured
`Review`) is exactly where nested shapes show up. So the interesting failure
mode is specific: a `Generic` instance whose `Rep` refers back to its own type
produces a self-referential dictionary. Whether that elaborates and runs is the
question. A non-recursive list test alone does NOT discharge this gate.

## HARD territory boundary

generic-surface is a LIVE lane mid-queue. Until root announces their fold you
may NOT touch, import, or depend on:

- `haskell/lib/Tidepool/Form.hs`
- their Generic substrate / metadata utility modules
- `Harness.Prelude`
- `tidepool-mcp/src/preamble.rs`

Write NEW files. Use **`GHC.Generics` from base directly**. In particular, if
you need an occurs/visited check to stop a recursive type's schema generation
from diverging, write your own — do not reach for theirs, and do not wait for
their fold. Separate interpreters is the design (PRD 15), not an accident to be
deduplicated later.

`Tidepool.Agent` (`haskell/lib/Tidepool/Agent.hs`) is an unrelated existing
module — the harness answerer's capability row. Do not touch it. Sit under
`Tidepool.Agent.*`.

## Shapes to prove

At minimum:

1. **List-carrying record** — e.g. PRD 18's own `WorkerResult`:
   `Completed { summary :: Text, caveats :: [Text] }` /
   `Blocked { blocker :: Text, evidence :: [Text] }`. A record-and-sum shape
   with a list field, not a bare list.
2. **A genuinely recursive ADT** — e.g. `data Plan = Step Text | Seq [Plan]`.
   Pick one whose recursion goes *through* a list, since that is the shape a
   real `DevPlan`/review tree has and it stresses both properties at once.
3. **A nested/empty edge** — an empty list, and a value nested at least two
   levels deep. Empty containers and depth-1-vs-depth-N are where codecs
   quietly disagree.

For each: encode → decode → assert equality with the original, **running on
the JIT**, driven from `tidepool-runtime/tests/agent_structural_codec.rs`.
`tidepool-runtime/tests/generic_deriving_337.rs` and
`nullary_sum_generic_deriving.rs` are the nearest patterns for compiling and
running authored Haskell through the real pipeline.

A round-trip that only typechecks proves nothing. The value must be
reconstructed by code that actually executed.

## Codec design notes

Keep it minimal and honest. You are proving the algebra survives, not shipping
a finished codec.

- Target a structural value the rest of the wave can consume. Aeson `Value` is
  the pragmatic choice (it is what backends speak, and
  `tidepool_agent::seam::DynamicToolDeclaration` already carries
  `serde_json::Value`), but if a dedicated `StructuralValue` reads better,
  that is fine — say which you chose and why in the receipt.
- Encode and decode must be **inverse by construction**, derived from the same
  traversal. `plans/post-restart/codex-review-2026-08-08.md` item 8 is a ledger
  of what happens when they are not: `Nothing`/`Just x`/unit all collapsing to
  null, nullary constructors becoming strings while records become `_con` and
  positional constructors a third shape, `Just ()` and `Just Nothing` losing
  information. Read that item before you design the encoding — it is a list of
  mistakes already made once in this repo, and re-making any of them here is
  avoidable.
- Sum encoding: pick one shape and use it for every constructor form. Do not
  special-case nullary constructors into bare strings.
- Loud rejection over silent coercion. An unsupported shape is an error value,
  never a sentinel string.

## The verdict you must return

Not "it works". One of:

- **GO** — lists and recursion both round-trip on the real JIT; here are the
  shapes, here are the counts.
- **GO with a named limit** — e.g. recursion works to depth N, or through a
  list but not directly, or requires an explicit occurs check that costs X.
  State the limit precisely; a limit is a usable answer, a vague one is not.
- **NO-GO** — with the concrete failure (the extract diagnostic, the JIT trap,
  the case-trap breadcrumb). Per PRD 18, a NO-GO stops convergence and routes
  to substrate repair; it does not get worked around with agent-authored JSON
  schema. Report it and stop rather than inventing a shortcut.

If you hit a JIT case-trap, `tidepool-codegen/CLAUDE.md` documents the
poison+breadcrumb path (`emit_case_trap`) — the breadcrumb is the diagnostic,
read it rather than guessing.

## Verification

- `haskell/CLAUDE.md` first, especially Known Limits — it will tell you in
  minutes what would otherwise cost an hour of JIT debugging.
- Real extract/JIT via battery tier 2:
  `scripts/battery.sh -p tidepool-runtime -E 'binary(agent_structural_codec)'`.
  **Never bare `scripts/battery.sh`.**
- `cargo check --workspace` before submitting; clippy + fmt clean.
- GHC slots are capped at 3 box-wide with two other waves live. Expect to
  wait. Batch your runs; do not poll.

## Coordination

dev-mode-encoding needs a shallow structural schema for its `compileTools`
leaves. You are building the deep one. Route any overlap **through the TL** —
do not each write half a codec, and do not edit their `Contract.hs`.

## Deliverable

A receipt at `plans/post-restart/agent-lanes/receipt-structural-codec.md`:
the verdict, the exact shapes proven, the encoding chosen and why, any named
limit, and per-binary test counts — never exit codes.

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
