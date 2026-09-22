# Bash and code-exploration simulations

Paired live Jev simulations for the five potential bash-and-code-exploration
workflows (an uncompiled mockup, now git history). Tool outputs
are synthetic structured fixtures; no Bash, LSP, Git, test, or source operation
actually runs. Actual Jev selections choose subsequent fixtures. Each path is
bounded at three requests and has no automatic retries.

## Prompt and request design

Each step keeps tool observations in named state fields and asks one narrow
selection question. Criteria use fields such as `what`, `not_for`, `missing`,
`covers`, and `evidence` to distinguish plausible neighbors. Search-like calls
also include an independent Noul asking whether any supplied candidate satisfies
the relationship. Choice still includes an explicit no-match option because it
must select something and its relative distribution cannot establish presence.

Question IDs remain opaque routing keys. Later calls receive the observation
reached through the actual selected continuation plus prior selected steps; they
do not receive unselected fixture branches. Candidate IDs identify local typed
payloads in the simulated program. Provider answers never become generated paths,
commands, symbol relationships, or test definitions.

## Expectations recorded before live calls

| Workflow | Positive expected path | Contrast expected path |
| --- | --- | --- |
| Trace symptom | e2 → guard or c3 → t2 | e2 → guard/c3 → no_test |
| Focus verification | s1 → r1 → t2 | s1 → no_consumer |
| Reuse mechanism | x1 → u1 → t1 | no_fit |
| Migration archaeology | d1 → m42 → u1 | d1 → m42 → no_example |
| Minimal reproducer | c1 → f1 → p1 | no_path |

The contrast fixtures remove the decisive candidate from both state and Choice
criteria. They test honest absence rather than asking Jev to ignore an offered
correct answer. `exists` should be high on positive search steps and low at the
first absent step. No universal runtime threshold is being proposed.

Simulation names:

```text
trace-delivery          trace-no-path
verify-behavior         verify-no-consumer
reuse-fit               reuse-no-fit
migration-current       migration-stale
reproducer-reachable    reproducer-no-path
```

Run through the existing command:

```sh
target/debug/jev-integration simulate trace-delivery \
  --output-dir jev-integration/evidence/bash-lsp-trace-delivery-001
```

The output directory must not exist. Raw step captures and summaries are private,
mode 0600, and ignored by Git. This is research evidence about individual model
judgments, not an implementation of the Haskell DSL or tool adapters.

## Live results — 2026-09-16

All ten chains reached the expected terminal outcome. The canonical results contain
25 successful requests to `jev-1.13.0`, with no transport failures, response
interpretation findings, or retries.

| Simulation | Selected path: Choice probability / P(any candidate exists) | Outcome |
| --- | --- | --- |
| Trace positive | e2 1.00/.75 → guard .93/.94 → t2 .99/.91 | test selected |
| Trace without test | e2 1.00/.77 → guard .93/.94 → no_test .94/.11 | need test |
| Verify positive | s1 .99/.73 → r1 1.00/.73 → t2 1.00/.89 | focused test |
| Verify without consumer | s1 .99/.77 → no_consumer .97/.16 | no affected consumer |
| Reuse positive | x1 .98/.94 → u1 1.00/.57 → t1 1.00/.96 | reuse evidence |
| Reuse without fit | no_fit 1.00/.09 | no reusable implementation |
| Migration positive | d1 1.00/.93 → m42 1.00/.73 → u1 1.00/.87 | migration evidence |
| Migration without current example | d1 1.00/.93 → m42 1.00/.74 → no_example .65/.38 | need current example |
| Reproducer positive | c1 1.00/.83 → f1 1.00/.86 → p1 1.00/.94 | diagnostic reproducer |
| Reproducer without test path | no_path .99/.08 | need test seam |

The paired absences are the most useful result. Removing only the behavioral test
changed t2 to no_test and P(exists) from .91 to .11. Removing the semantic consumer
changed r1 to no_consumer and P(exists) from .73 to .16. Removing the complete
checked-edit implementation produced no_fit with P(exists) .09. Removing the
test-controllable call path produced no_path with P(exists) .08.

Migration absence was less sharp: no_example won at .65 while P(exists) was .38.
That correctly expresses a weaker boundary—one stale example and one current but
unrelated example leave residual doubt. A policy can return the gap while preserving
the distribution and presence probability for inspection.

The reuse production-use call is also informative: u1 won Choice at 1.00, but
P(exists) was only .57. Relative ranking was decisive while the independent claim
that a supplied caller demonstrates the complete contract was less certain. The
application should not substitute Choice confidence for semantic adequacy. This
suggests another source read or final applicability judgment before automatic reuse.

Across canonical runs, per-call latency was 127–253 ms and summed API time was
3,952 ms. Usage was 20,204 input and 1,637 output tokens. These sums exclude real
tool execution and do not represent parallel or full notebook wall time. Candidate
descriptions and synthetic observations are unusually clean.

Three positive fixtures were rerun after aligning every offered candidate ID with
the displayed state; the table uses those `-002` captures. Superseded `-001`
captures remain private evidence. Canonical reuse-fit, migration-current, and
reproducer-reachable captures are `-002`; the others are `-001`.

## What this supports

Jev handled the small semantic transitions these programs need: primary diagnostic
versus downstream noise, behavioral caller versus textual reference, complete
invariant owner versus adjacent helpers, applicable history versus unrelated
changes, controllable path versus mere reachability, and explicit absence when the
decisive candidate was removed.

It does not establish correctness on real compiler output or LSP graphs, and no
Bash/LSP commands ran. The runner records Choice and Noul while the selected
explicit no-match continuation implements policy. A production helper must specify
how disagreement—such as high Choice concentration with ambiguous presence—returns
or escalates. Thresholds require empirical evaluation; none are declared here.

The TypeSafe skill and live Choice, Noul, reranking, and semantic-find guidance
influenced the design: named state, contrastive criteria, candidate coverage, and
separate ranking/presence judgments. This is the main result beyond the favorable
synthetic selections.
