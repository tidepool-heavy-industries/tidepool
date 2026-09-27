# Haskell authoring improvements

## Scope and sequencing

Finish the current wave18 improvement batch and its focused verification first.
New authoring improvements use isolated worktrees and the same single expensive
build slot; they do not authorize deployment or a successor wave. Discuss the
harness milestone with Inanna before the next Sol-root wave.

## Bounded lanes started

- `rsi/w18-typed-recipe-assertions`: trace the existing RecipeCheck/RecipeTurn
  boundary and provide a typed assertion path, migrating real consumers. Rendered
  tuple/Bool text is not assertion evidence. Do not introduce a second evaluator
  or turn ordinary false into a machine-latching exception without reviewing the
  lifecycle implications. A cross-layer change needs an explicit seam review.
- `rsi/w18-record-diagnostics`: trace missing-record-field warnings from GHC to
  authored source and model-visible diagnostics. Repair demonstrated drops and
  add focused coverage. No global warning-as-error policy; preserve any intentional
  partial-record use pending an explicit decision.

Primary review owns both API choices and final acceptance. Further candidates:
shared original-evidence carriers, consistent typed continuation destinations,
and one finish operation for multi-actor authored procedures. Derive these from
actual consumers rather than adding an abstract workflow framework.

## Jev intermediate layer: discussion draft

Use the pinned Jev DSL's existing questions, packets, alternatives, uniform
payloads, policies and response evidence. Intermediate operators should construct
reusable questions/compositions; each helper must not eagerly issue a separate
Jev request. Existing packet composition remains the batching mechanism.

Start with applicability, bounded routing and change assessment. Scope checks
and escalation are concrete uses of these, not automatically separate engines.
Domain policies supply criteria, exclusions and evidence projections. Haskell
owns deterministic guards, source identity, side effects and continuation.

Keep question construction, execution and policy interpretation separate. Preserve
raw judgment evidence and service failure separately from semantic uncertainty.
Confidence policy must be visible at the meaningful decision boundary and may be
named/reused; avoid an invisible universal threshold. No generic concept model,
second scheduler, second provider client or automatic notification suppression.

First design exercise: express current review-scope routing and notification
novelty as clients of the same small building blocks. Review authored call sites
before settling the abstraction. Notification novelty stays a shadow experiment.
The primary agent owns this design discussion with Inanna before implementing
new intermediate-layer surface.

## Pattern catalog and lane findings

[Pattern building blocks](jev-pattern-legos.md) records the agreed direction:
opinionated, pluggable judgment patterns composed into ordinary Haskell decision
trees, with defaults reviewed between runs. These are design sketches, not shipped
interfaces. Start with contract routing and shadow update comparison.

The typed assertion audit found no narrow runtime fix: a truly typed resident
assertion needs an explicit actor-effect or retained-value boundary change. Its
report is committed separately as `5aebc87c`; do not encode false as a runtime
exception merely to avoid rendered output. The diagnostics lane found a real
warning drop through checked declarations and is preserving existing warnings
through that path, without imposing a new warning policy.
