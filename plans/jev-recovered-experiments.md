# Jev experiments recovered from earlier designs

Status: exploration plan, 2026-09-17. Companion to
[swarms sharing and improving System 1 code](jev-shared-components.md).
This is a menu of small experiments, not a required implementation sequence
or an addition to the active dogfood wave.

## Goal and method

Give agents ways to combine ordinary code with a little semantic intelligence,
then discover what becomes useful. The strongest outcomes can become Haskell
components that agents exchange, adapt, and improve.

Start with existing effects, project modules, files, Git, and manual operator
help. Frequent restarts let us adopt changes quickly. Build additional runtime
machinery only when a concrete experiment encounters a missing capability.
No dedicated notebook UI, TUI frontend, or web frontend is part of this plan.

Choose experiments when the corresponding difficulty appears in real work.
Retain the source, a few observed cases, any counterexamples, and what the
consumer found awkward. Interviews and the next useful attempt guide iteration;
a large benchmark campaign is not a prerequisite.

## 1. Prepare a better question

**Program:** gather observations, select relevant evidence with Jev, perform
the available follow-up reads, and return either a useful finding or the
specific unresolved distinction with its evidence.

The agent can continue from that result and extend the function for next time.
Partial automation succeeds when it removes useful work even if it cannot
finish the investigation.

**First try:** one recurring failure, two evidence-gathering steps, and an
explicit return to the model when the available explanations do not fit.
Ask whether the recipient had to repeat the investigation.

## 2. Keep competing explanations alive

**Program:** an agent supplies plausible hypotheses and available probes.
Code manages the candidate set, observations, and limits; Jev judges which
probes could discriminate among the remaining explanations. Gather evidence
before deciding whether a full model worker is needed.

**First try:** two or three explanations for a real failure. Allow the program
to return multiple survivors or reject the offered explanations. Examine
whether the selected probes actually changed what the agent knew.

## 3. Pass executable criteria with assignments

**Program:** an assignment includes a function that checks a result and returns
counterexamples. Add Jev judgments for explicitly semantic conditions, keeping
those judgments distinct from executable assertions. Implementers and reviewers
can run, challenge, and improve the same criteria.

**First try:** save a check alongside one task contract; have another agent
apply it to its candidate and return a concrete failing case. Source exchange
is enough. Passing the check establishes only the conditions it actually checks.

## 4. Recognize recurring trouble and request a Luna

**Program:** retain the last N relevant events or Jev observations and their
underlying evidence. Jev asks whether they suggest a recurring semantic problem,
such as repeated misunderstanding of one API despite different error messages.
Code handles known waiting states, duplicate suppression, budgets, and admission.
An authored policy may request a Luna with the assembled history when the
condition holds.

**First try:** have the observer produce a proposed investigation brief; the
operator launches the Luna manually. If useful, connect the same trigger to
existing actor admission. Verify which context source a background actor can
use; do not assume it can inherit an interactive transcript at an arbitrary time.
Repetition of correlated judgments alone is not stronger evidence.

## 5. Reuse prior decisions by meaning

**Program:** match a new question against retained decisions and retrieve the
original evidence. Distinguish an applicable decision, an open duplicate,
a changed premise, and no relevant decision.

**First try:** a plain list of a few decisions with their scope/source and one
incoming question. Return a suggested match for the agent to inspect. No request
interception, automatic reply, or durable decision registry is needed.

## 6. Catch drift in proposed work

**Program:** compare a proposed plan or assignment with explicit requirements.
Ask per requirement whether it is incorporated, omitted, contradicted, or
challenged with a reason. Gather consequential differences for the owner.

**First try:** use a real short plan and its readback. Preserve legitimate
disagreement rather than rewarding paraphrase. Code supplies the requirement
inventory; Jev does not establish that the inventory covers the whole goal.

## 7. Make small semantic workers into functions

**Program:** perform routine observations and semantic classification in Haskell
plus Jev, returning typed results. Only unresolved cases become model requests.
Examples: reports describing the same defect, changes affecting an assumption,
or tests exercising a named condition.

**First try:** one classification task currently repeated by a worker. Keep
the current worker as the consumer of the results and uncertainties. Avoid
creating a generic worker framework to run an ordinary function.

## 8. Find a useful component another agent already wrote

**Program:** enumerate saved Haskell components and their examples. Jev matches
the current difficulty to potentially useful components. Code retrieves the
actual source; the agent decides whether to try, adapt, or compose it.

**First try:** a handful of files from the shared-components experiment and a
new task. No registry is needed. Learn which descriptions make applicability
clear and which omissions cause plausible but unhelpful matches.

## 9. Find an implementation, a consumer, and a test

**Program:** search for an existing mechanism, find a representative production
caller, and find a test demonstrating the relevant behavior. Jev connects the
meaning of the inquiry to candidates supplied by search and source inspection.

**First try:** one agent about to add a helper gets this evidence bundle first.
Use Git/search commands; do not wait for a general LSP integration. A missing
consumer or edge-case test is a useful finding, not permission to invent one.

## 10. Recover a migration from history

**Program:** start with a compiler mismatch, inspect relevant history, select
the change that may explain it, and locate a current caller already migrated.
Return the error, historical patch, and working example to the repair agent.

**First try:** one real type/signature migration. Jev selects relationships
among supplied evidence; the agent writes the repair. Historical intent remains
evidence to compare with current behavior, not an overriding specification.

## 11. Produce small reusable counterexamples

**Program:** retain an executable reference model and observed failing cases.
Code generates or shrinks candidates; Jev helps judge which preserve the
interesting semantic condition. Executable checks confirm that failure remains.

**First try:** reduce one real failure into a small example that another agent
can understand and run. Keep the smallest confirmed case found, without claiming
global minimality. Jev can prioritize candidates but must not replace the actual
failure predicate where one exists.

## 12. Identify assumptions affected by a change

**Program:** enumerate callers or related assignments, then use Jev to distinguish
dependence on the changed behavior from incidental mention of the same name.
Return focused checks or a proposed message naming the affected assumption.

**First try:** one shared API change and its known consumers. Have the owner
inspect the suggested affected set before sending messages or choosing checks.
This can later help propagate relevant discoveries across a swarm without
broadcasting every diff. Focused selection is not exhaustive verification.

## 13. Make memory behavior revisable

**Program:** define functions for what to recall, relate, retain, or bring to
attention. Jev judges semantic relevance, novelty, or resemblance to an earlier
failure; code retrieves the actual records. Agents and the human can revise the
policy and share useful pieces of it.

**First try:** match current trouble against a small collection of prior cases,
or select useful context for a returning worker. Use ordinary files and retained
values. Keep durable source/data distinct from live closures and runtime handles.

## Useful compositions to try

- Failure → relevant saved component → evidence gathering → unresolved question
  for Luna → Luna improves the component → another agent tries the revision.
- Shared API change → affected assumptions → demonstrated migration example →
  focused checks → small counterexample for any remaining failure.
- New task → applicable decisions and components → proposed approach → semantic
  alignment check → owner resolves the genuinely new question.

These are illustrative authored programs. Known sequence stays in code; Jev
answers the semantic questions at the points where evidence permits them.
Recent trials support selecting useful evidence more strongly than predicting
the next generic workflow step.

## What to save from each attempt

Keep the working definition, actual input, result, revision, known limitation,
and a short note about what the consumer did next. For reusable components,
include an example call and effects/imports needed. Inspect accepted alternatives
explicitly: Jev threshold acceptance does not mean the chosen answer approves
the candidate. Preserve missing-evidence and no-fit outcomes.

The next decision is which experiment naturally fits the next task. Mechanism
plus usage discovery and migration archaeology are promising low-machinery
starting points; the shared-components plan remains the main collaboration trial.

## Sources and status

- [Resident Haskell side quests](actor-model/resident-haskell-side-quests.md):
  executable inventories, hypothesis trees, reusable instruments.
- [Astra exploration report](actor-model/astra-ux-exploration-field-report.md):
  observed use of retained models and enumerated counterexamples.
- [Bash and code exploration sketches](jev-bash-lsp-examples.md):
  reuse evidence, migration history, focused verification, and reproducers.
- [Typed request triage](jev/typed-request-triage.md):
  semantic fields and decision matching; generic interception remains speculative.
- [Cell patterns](jev/microprogram-patterns.md) and
  [run-ahead](jev/run-ahead.md): evidence preparation and explicit return to a
  model. Their historical API sketches and workflow-prediction claims need care.
- [Companion directions](../harness-dogfooding/companion/INSPIRATION.md):
  co-designed attention and memory as executable policy.

The document recovery included source spot checks, not end-to-end validation of
these compositions. Illustrative LSP, file-editing, and sharing APIs in older
plans must not be advertised as shipped solely because their sketches exist.
