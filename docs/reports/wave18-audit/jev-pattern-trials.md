# Jev pattern authoring and live probes

2026-09-27 UTC. Operator-authored synthetic exercise; no live actor was steered,
command retried, message suppressed or acceptance granted by these judgments.

## What changed

Three small pattern modules now have six authored clients:

| Pattern | Clients | Useful boundary |
| --- | --- | --- |
| Command triage | Development check; package fetch | Questions compose in one packet; irrelevant optional questions are omitted; a typed planner consumes only the applicable answers. |
| Evidence selection | Diagnostic site; review source | Selection returns the original typed payload, source and excerpt. Candidate rendering is pluggable. |
| Update comparison | Review update; production-consumer checkpoint | Known missing facts are checked before inference; explicit incorporation is distinct from acknowledgment or silence. |

All use existing Jev questions, packets, policies and decoding. There is no new
scheduler, transport, universal decision-tree representation or notice history.
Full responses remain available beside interpretations. Service error, policy
doubt and settled insufficient evidence remain separate.

The strongest API improvement came from using the evidence helper in a packet:
its first version put excerpts in selection alternatives, while the sibling
coverage question had only an intent in shared state. The revised client puts
structured evidence in shared state once and references it with `J.field`.
Rather than add another question constructor, the existing candidate-rendering
function now controls the complete wording; `describeEvidence` is the standalone
default. The command coverage question also now asks about locations, since the
fixture does not supply a full definition body. Both changes were made together;
the trial does not isolate their individual causal effects.

Command clients also exposed test-shaped wording in the fetch case. Criteria now
include an exhaustive `FailureKind -> Text` function. The planner retains
`Settled p` in its result instead of erasing the caller's policy type. Repeat
allowance remains advisory; only the command owner can consume a real budget.

## Exact evidence

- Tidepool execution revision: `f4be1dc457960ec0ec48ebc167cfe3c4c7111c35`.
- Jev DSL pin: `f16f1363b4d389d6e34f9d695fbd254ca0735f2e`.
- Command branch candidate: `f703047a7080de10b261233479d26a49154b9c54`.
- Evidence branch candidate: `604f8da7843c9a93d6f44a4a1f4decdc2103b956`.
- Coordination branch candidate: `bd9daaa391dbc9cdb73150eb6bc7691269669e49`.
- Integrated shared branch: `rsi/w18-shared-integration`,
  `a3d1d42af97728f310b65abd9ba7e3b345c5e2f6`.
- Worktrees are under `/home/inanna/dev/rsi-wave18/`.
- Native runtime/reload, deployment and future-wave adoption are not claimed.

The operator prepared exact requests with Haskell `J.request`, sent them to the
live `/v1/systemone` endpoint without retries, then replayed captured responses
through Haskell `J.decode` and the actual pattern interpreters. Recipe sessions
have no Jev backend, so this deliberately does not claim an end-to-end resident
Jev effect test. Response model: `jev-1.13.0`.

[v1 requests/responses](jev-pattern-trials/v1.json) contains twelve calls.
[v2 requests/responses](jev-pattern-trials/v2.json) contains only the five changed
requests; unchanged requests reused v1 evidence. Total: **9,800 input tokens,
1,039 output tokens, 17 requests**. Recorded request wall times were 0.092–0.177s
in this local trial, excluding Haskell preparation/compilation; not a general
latency estimate. No frontier-turn savings have yet been measured.

The retained [request exporter](jev-pattern-trials/PatternProbe.hs) and
[final decoder replay](jev-pattern-trials/PatternReplay.hs) are compiled fixtures,
not model-facing instructions. Copy into `Project/` beside the three pattern
families in an isolated workspace to reproduce the recipe entries. The replay
uses the updated responses where present and original responses elsewhere.

## Results and limits

- **16 construction assertions** across command (1), evidence (9), coordination
  (6). Optional omission is checked against the actual prepared request;
  duplicate candidate keys are rejected by the existing Jev validator.
- **12 first replay assertions**, then **14 final replay assertions** including
  the two coverage judgments. Final revision definitions identity:
  `f15d5623ea21db0c05edb54d4eaf2162fac9f9f109f71d26732bd0649a56221d`.
- Ten decision cases settled as expected. Missing command output and consumer
  silence returned policy doubt, which is an accepted handback, not success of
  an operational action. Silence's raw winner was attention (0.53), but confidence
  0.30 did not clear the careful policy. This is why callers must retain doubt.
- Coverage judgments changed from 0.23/0.25 to 0.92/0.93 after the evidence-state
  and wording repair; intended candidate selection remained intact.
- The review evidence choice remains competitive (budget 0.66, page 0.32,
  confidence 0.58). It clears the lenient read-selection policy; it is not a
  strict review acceptance. Page code is itself useful evidence.
- The 503 case offers one repeat only with caller allowance and positive
  transience. Credit exhaustion does not offer a repeat. No repeat was executed.

Focused command used throughout, serially with an owned one-worker daemon:

```
TIDEPOOL_DAEMON_ARGS='--workers 1 --rss-ceiling-mb 7168' just exomonad-run -- check --workspace /home/inanna/dev/rsi-wave18/verify-patterns --recipes
```

The wrapper selected only the stated construction/export/replay recipes. Modules
and authored effectful clients compiled. The effectful `J.ask` entry points were
not invoked in a resident host. Formatting was reviewed and `git diff --check`
passed; no configured Haskell formatter was available on PATH.

Earlier attempts exposed a packet kind mismatch and ambiguous structured-object
keys in the coordination example, plus a monomorphism error in the private
exporter. All were repaired before the successful checks. An earlier attempt to
use resident `turn` for a pure construction check returned an opaque Haskell
exception; the pure check now runs in the recipe driver. That separate receipt
problem was not diagnosed by this exercise.

## Next use

`JEV-PATTERNS.md` and the coordinator discovery paragraph are staged in the shared
integration branch. Let an owner specialize one client for a recurring task,
retaining request/response evidence and the action chosen. Evaluate wrong paths,
handbacks, saved coordination steps and packet costs across real waves. Do not
promote synthetic success into automatic notification suppression or broad
retry authority. The broader inter-wave integration and deployment gates remain.
