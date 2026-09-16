# Jev decision frontier experiments

Date: 2026-09-16  
Observed service model: `jev-1.13.0` via requested alias `jev-latest`

These experiments ask where a Jev-shaped oracle stops being a useful semantic
branch and starts being an unreliable substitute for deterministic execution.
They are synthetic, have machine-checkable answers, perform no external actions,
and use one request per case with no retry.

The result is encouraging but sharp: Jev is much stronger at **broad, shallow
semantic discrimination** than the earlier conservative examples assumed. It is
not a dependable interpreter for repeated opaque state transitions. Haskell
should compute exact structure; Jev should decide what the structure means.

## What was tested

`frontier --output-dir <new-directory>` currently exercises:

- Conjunctive selection among 8, 32, 128, and 255 alternatives. Exactly one
  candidate satisfies all six typed requirements; every distractor differs in
  one disqualifying field. Four 32-way relabelings test sensitivity to opaque
  names and placement.
- Explicit absence among 8, 32, and 128 near misses. `none` is a real Choice
  alternative because Choice always ranks something.
- Selection of a valid three-, five-, or seven-edge path from a supplied graph.
  Each edge must be current, connected, unflagged, and have the required
  alternating semantic kind. Another case has no valid path.
- Exact traversal of an opaque node-to-next relation for 1, 2, 4, 8, 16, 32,
  or 64 transitions. The relation is shuffled and the answer is independently
  known. This deliberately asks Jev to act like a tiny interpreter.
- Resolution of accepted, superseded, draft, off-branch, and exception-bearing
  decisions against an exact temporal question.
- Six overlapping positive and negative Noul judgments over revision approval,
  receipt versus handling, and whether a local contract resolves an issue.

Strict success requires the expected answer, HTTP success, and a response that
passes the harness's provisional contract checks. Thus a correct selected key
can still fail if the returned probability distribution is malformed.

## Results

The initial 18-case suite passed 18/18. It included the full 255-way valid
selection, 128-way absence, graph paths through depth seven, both temporal
cases, and all six overlapping judgments.

After adding pointer traversal, two further runs produced:

| Task | Run 003 | Run 004 | Selected-answer probability |
|---|---:|---:|---:|
| pointer depth 1 | pass | pass | 0.99, 0.99 |
| pointer depth 2 | pass | pass | 0.92, 0.89 |
| pointer depth 4 | pass | pass | 0.28, 0.30 |
| pointer depth 8 | pass | fail | 0.09, 0.08 (winning answer) |
| pointer depth 16 | fail | fail | 0.07, 0.08 (wrong answer) |
| pointer depth 32 | fail | fail | 0.15, 0.14 (wrong answer) |
| pointer depth 64 | fail | fail | 0.28, 0.26 (wrong answer) |

An earlier run also failed depth 8, so its observed record is one pass and two
failures. Depths 16, 32, and 64 failed every observed attempt. The exact
depth-eight endpoint is independently verified by following the captured links;
this is not an answer-key error.

The broad selection cases continued to choose the expected key. However, three
responses across runs returned rounded Choice probabilities whose sum differed
from one by more than 0.01: one 128-way absence response and two 32-way
responses. In every case the chosen key itself was correct. This is a response
contract/quantization failure, not a semantic miss, and it was intermittent.

Representative cost and latency from the first clean run:

| Task | Input tokens | Output tokens | Elapsed | Result |
|---|---:|---:|---:|---|
| 32-way six-field choice | 3,270 | 337 | 171 ms | correct, 0.96 |
| 128-way six-field choice | 12,160 | 1,297 | 269 ms | correct, 0.84 |
| 255-way six-field choice | 24,076 | 2,567 | 373 ms | correct, 0.97 |
| seven-edge semantic path | 3,118 | 278 | 128 ms | correct, 0.98 |
| six overlapping Nouls | 492 | 118 | 154 ms | all correct-side |

These are observations, not latency or calibration guarantees. The samples are
small and the endpoint is stochastic.

## The Pareto frontier

The surprising positive result is fan-out. Jev can inspect hundreds of rich,
nearly matching candidates in one fast call, enforce several simultaneous
constraints, recognize that none qualify, and handle local semantic graph and
temporal-policy questions. That is enough to collapse many tool-observe,
inspect, choose, and route turns into one resident microprogram.

The failure is sequential exact execution. With opaque identifiers, confidence
degrades rapidly as Jev must repeatedly apply a relation: depth four is correct
but already low-margin; depth eight is unstable; depth sixteen is consistently
wrong. More tokens and a larger candidate set do not repair this. Jev is not a
small deterministic VM, graph walker, parser, counter, or proof checker.

The practical frontier is therefore not “simple versus complex.” It is:

- **Give Jev semantic width:** many alternatives, nuanced descriptions,
  overlapping questions, exceptions, and rich local evidence.
- **Keep exact depth in Haskell:** graph traversal, joins, reachability,
  counters, ordering, revision ancestry, and state-machine stepping.
- **Put Jev at semantic cuts:** after deterministic code has computed a
  frontier, ask which continuation is relevant, whether evidence is sufficient,
  whether interpretations agree, or whether to escalate.

A strong Shoal microprogram can still compress four or five conventional turns:
run Bash/LSP queries, normalize and join their outputs deterministically, expose
the resulting semantic frontier as one rich Choice plus independent Nouls, then
execute the selected continuation. What should not be compressed into the Jev
call is the exact execution of that join or traversal.

## Consequences for the effect and DSL

The single Jev operation should preserve the provider's full question language;
the integration should not impose an arbitrary small-choice ceiling. The
combinator layer around it should make the safe pattern pleasant:

1. deterministically acquire and reduce tool results;
2. construct a bounded semantic frontier with explicit no-match/escalate cases;
3. issue one rich Jev request, possibly containing related Choice, Noul, and
   Score questions;
4. validate the entire response independently of the winning key;
5. gate action on answer identity, probability/margin policy, and consistency;
6. escalate or gather more evidence on low margin, contradiction, or malformed
   distributions.

Do not treat the top Choice as authority merely because the endpoint returned
200. The Rust interpreter must validate finite probabilities, exact key sets,
sum tolerance, and selected-key membership. The authored Haskell layer should
receive typed evidence sufficient to express an abstention policy. At high
fan-out, rounded distributions may need either a provider clarification or a
documented tolerance policy; silently renormalizing would hide contract drift.

Future boundary work should measure repeated-run reliability and calibration,
test distractor prose and long irrelevant state independently, and compare one
wide call with deterministic partitioning plus one final semantic call. These
experiments should optimize total tool turns and correctness, not maximize the
amount of deterministic work delegated to Jev.

## Reproduction

```sh
bash scripts/dev-shell.sh cargo build -p jev-integration
TYPESAFE_API_KEY="$(< /path/to/key)" \
  target/debug/jev-integration frontier \
  --output-dir jev-integration/evidence/frontier-new
```

Evidence directories are git-ignored, exclusively created, and mode 0600. The
recorded local runs were `frontier-001` through `frontier-004`; summaries report
18/18, 17/22, 20/25, and 21/25 strict passes respectively. Failures in runs 002
and 003 include the intermittent probability-sum diagnostics described above.
