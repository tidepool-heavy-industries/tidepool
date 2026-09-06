# Usage/evidence TL — better Shoal practice, not another framework

## Assignment

Improve repeated unfold/fold/review work using small reusable on-disk Haskell
helpers, real consumers, and focused test recipes. Independent from the control
implementation. User prioritizes Shoal *usage* over a large repository dependency
refactor. Full-prefix forks stay default; selected small workers are a later gate.
Baseline `e5a1842dd4d3302d6eb8c0519a678937599d9c7e`; read nearest contributor guidance,
including `prompts/shoal/AGENTS.md` before prompt edits.

Owners: model-facing Haskell library/examples, shipped `prompts/shoal/`, existing
`scripts/codex-worktree-guidance.md`, existing toolchain/test scripts. Root owns
shared manifests; service TL owns runtime semantics. Coordinate any public evidence
type with root; do not duplicate runtime response or watch registries.

## Small scaffold and consumer

Start from the recurring distinction below, not an all-purpose campaign DSL:

```haskell
data ExecutionOutcome = ExecutedPassed | ExecutedFailed | CompileOnly | DidNotExecute
data CheckExpectation = RequirePassing | ReproduceKnownFailure
-- RevisionCheck also records exact tested revision, command and retained evidence.
```

These are proposed task-level contract shapes, not claims of a shipped API. Separate
observed execution from expected result; include zero-selected/blocked explanations.
Keep compilation, source review, attributed execution and independent rerun distinct.
Use ordinary records/functions, `Member Effect effects`, `traverse`, retained handles
and `Await`. Do not invent bulk operations or encode control state in rendered labels.

Exercise helpers through a representative actual coordinator/reviewer consumer and
compile/load through the existing toolchain. A pure acceptance predicate plus compact
projection can be enough. Promote only helpers useful more than once; no fake backend
success. Document captured-definition/source revision stability and no arbitrary
closure persistence on restart. Existing requests keep their captured result types.

## Practice changes grounded in the run

- Commit shared types/fixture and clear file ownership, then fork before unrelated
  diagnosis fills the prefix. Avoid giant process listings inherited by every leaf.
- One owner per shared test module; logical independence did not prevent append
  conflicts. Fresh reviewer gets exact candidate, contract and implementer handle.
- Keep repair rounds with reviewer/implementer. Root receives decision changes,
  accepted candidate and evidence limits, not every administration exchange.
- Distinguish publication, acceptance, integration, baseline receipt, incorporation
  and resulting checks. Use typed surveys where a cumulative report is useful.
- Group user updates by new diagnosis/integrated baseline/blocker; don't relay every
  acknowledgment watch. Retain exact evidence; don't hide unresolved failures.
- Cleanup is its own checked obligation, not implied by reply or idle pane.

## Test-cost scope

Existing facts: tidepool-repr uses tidepool-testing's `arb_core_expr` in two tests,
while tidepool-testing has unconditional runtime/codegen/toolchain/MCP dependencies.
Both quick and battery scripts resolve extractor infrastructure. This is an
opportunity, not authorization to make dependency surgery the critical path.

First supply correct focused recipes and measure setup versus execution with explicit
cold/warm conditions and binary/toolchain identity. Preserve repository Nix setup
for extractor-backed tests. Prefer owning `just test-lib`/`just test-target`; report
selected/executed counts. `NEXTEST_SUCCESS_OUTPUT=immediate` guidance already exists.
Use cargo fmt for workspace edition2021; manual edition2024 caused churn last run.
Only propose a narrow dependency cut if measurements justify it; one generator and
one test launcher must remain. No broad worktree batteries or warm-vs-cold speed claims.

## Waves / delivery

After scaffold, split helper + real consumer from usage/test measurement; independent
review checks evidence logic and whether guidance matches actual runtime capability.
Send runtime/API gaps to their owner rather than papering over them in prompts.
Deliver committed helpers, compiled consumers, concrete recipes, measured limits,
reviewed prompt delta and concise usage findings. Do not claim the prior tree proved
cheaper than serial work: no controlled comparison exists.
