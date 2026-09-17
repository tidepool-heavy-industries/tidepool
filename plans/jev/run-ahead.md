# Run-ahead: executing the likely continuation before the model wakes

Recorded 2026-09-16. The emergent mechanism identified in
[shoal-leverage.md](shoal-leverage.md). It is not one capability; it is what
several of them become once judgments are cheap enough to spend
speculatively.

## The observation

A judgment snapshot costs about 200 ms and no model round. An expensive
model's next decision, at most of Shoal's wake points, has a small number of
likely continuations, and the evidence those continuations need is
deterministically fetchable. Today Shoal waits for the model to wake, read,
decide, and then fetch. With Jev, the harness can predict the continuation,
fetch its evidence, and let the model wake into a state where its most
probable next three actions have already happened.

This is branch prediction with speculative execution. The frontier model
becomes the misprediction handler: it acts only where the prediction was
wrong or where the distribution said the prediction was shaky.

## Where run-ahead applies

| Trigger | Predicted continuation | What runs ahead | What the wake carries |
|---|---|---|---|
| failing check | investigate from the diagnostic | the investigation hylo with Jev-steered expansion | witness span, path, distributions, selected test |
| reviewer `Fix` | classify and route findings | the findings packet; contractual findings to decision memory | findings labeled mechanical or contractual, matched decisions |
| worker raises a question | match against decisions | decision memory | delivered decision or a prepared consult |
| candidate `Produced` | choose checks and review depth | the candidate packet; the focused check runs | check result, predicted finding kind, premise status |
| typed request arrives | fill judgment-shaped fields | typed request triage | prefilled record with distributions |
| child stops with a reason | disposition | the wake-economy packet | either no wake or one wake with batched reasons |

The common structure: a deterministic trigger, a packet over already-retained
state, typed policy selecting from existing candidates, deterministic actions
that gather evidence, and a wake that is either avoided or made denser.

## What makes it safe

- **Only reads run ahead.** Investigation, check execution in a scratch
  checkout, decision matching, and field prefilling are observations. Nothing
  that changes shared state, spends a budget the model owns, or creates
  authority runs speculatively. Merges, repairs, and steering wait for the
  model or for a policy that would have acted anyway.
- **Budgets are deterministic.** The hylo's depth and read budgets, the check
  timeout, and the packet count per trigger are fixed by policy. Jev cannot
  widen them.
- **Every speculation is journaled with its distribution.** A wake sees what
  was predicted, at what mass, and what was fetched. A wrong prediction costs
  a few reads and is visible.
- **The model can ignore it.** Run-ahead evidence is attached, not asserted.
  A wake that starts elsewhere is a recorded misprediction and a data point
  for the evaluation corpus.

## Cost model

Per trigger: one to three packets at one to two thousand input tokens each,
plus the deterministic reads. At published prices the judgment cost is
negligible; the real cost is tool time and, for checks, compute. Run-ahead
should therefore be gated on the expected value of the wake it shortens: a
failing check that will certainly wake an implementer is worth an
investigation; a routine progress event is not.

A simple policy: run ahead when the trigger already implies a wake, and never
run ahead more than one semantic boundary past the trigger. The model wakes
at the second boundary if the first did not resolve.

## Its usual output is a handback

Run-ahead executes the same authored cell on a trigger, so its normal result
is the cell's outcome type: sometimes `Located`, often `NeedsJudgment` with
the evidence gathered and the alternatives that remain. That is the point.
The model that wakes receives a prepared question with resident evidence, not
a claim. Run-ahead never needs to resolve anything to be worth running; it
needs to get the wake to the right question faster.

## What it changes

- The first model round after a failure starts at the mechanism, not the
  log.
- Contract disputes surface before the repair round that would have failed.
- Reviewers see the focused check result and the premise status before they
  read the diff.
- Owners wake for decisions, not for routing.
- Journals gain a prediction record: for every wake, what was predicted and
  whether the model agreed. That record is the training signal for tightening
  margins and the evidence for whether the thesis holds.

## What it is not

- Not a planner. Run-ahead never chooses a sequence; it executes the
  continuation selected by one packet over one state, then stops.
- Not autonomy. It gathers evidence for a decision the model still makes,
  or takes an action a deterministic policy would have taken anyway.
- Not a second scheduler. It uses the existing command owner, worktree
  owner, and coordination actors.

## Open design points

- Where the trigger hook lives: in the coordination actor's sink, in the
  batch driver around `advance`, or as an authored combinator the notebook
  program wraps around its own effects. The last keeps it in the model's
  hands and inspectable; the first two make it pervasive. Probably both, with
  the authored form first.
- How a wake presents speculation: a typed `Speculation` record beside the
  ordinary context, rendered compactly, with the trace reference for
  drill-down.
- Whether to run ahead on more than one branch when the top two are close.
  The read budget bounds it; the policy question is whether a near-tie is
  worth two investigations or one wake.
