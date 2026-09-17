# Jev in Shoal: research and design set

Recorded 2026-09-16 on `research/jev-integration`. These documents follow the
[Jev DSL handoff](../jev-dsl.md) and the
[session handoff](../../jev-integration/SESSION-HANDOFF.md). They assume the
typed single-operation Jev effect described there and change altitude: how
pervasive, cheap judgment snapshots change what Shoal does, rather than how
the effect is plumbed.

Status: design brainstorm and review. Nothing here is implemented, and no
production integration should begin before the unrelated engine work permits
it and the nearest `AGENTS.md` and owning sources are reread.

## Reading order

1. [What we learned about Jev](learned.md). The provider as observed: the
   contract, the measured frontier, four fresh live calls over real repository
   state, and the behavioral properties that constrain every design below.
2. [DSL review](dsl-review.md). An evaluation of the mode-interpreted record
   sketch and the authored usage examples, with the changes that today's
   observations motivate. Sol owns abstraction design; this is a mechanical
   soundness review with recommendations.
3. [DSL sketch](dsl-sketch.md) (Fable) and
   [independent sketch](dsl-astra-sketch.md) (Astra), written in parallel
   from the same brief, then [reconciled](dsl-reconciliation.md): where they
   agree is the design; where they differ the reconciliation takes a
   position and Sol decides. The reconciled surface is implemented as the
   standalone package at `~/dev/jev-dsl`; design and usage notes stay here.
4. [Shoal leverage](shoal-leverage.md). Where Shoal wastes intelligence, eight
   capabilities that reshape workflows around cheap semantic branch
   prediction, three before/after traces, a ranking, and conclusions.
5. [Typed request triage](typed-request-triage.md). The second capability to
   implement: explicitly annotated judgment fields of typed inter-agent
   requests filled from retained state, with decision memory as its first
   instance.
6. [Run-ahead](run-ahead.md). The emergent mechanism: executing the likely
   continuation of an expensive model's next decision before that model
   wakes. It reuses the authored investigation cell, which is implemented
   first.
7. [Microprogram patterns](microprogram-patterns.md). Authoring idioms for
   resident cells that use Jev: packet cadence, candidate pools,
   premise-prefixed questions, near-tie policy, and anti-patterns.
8. [Evaluation](evaluation.md). Replayable benchmarks built from journals,
   what each must show, and the experiments that would falsify the thesis
   fastest.
9. [Open questions](open-questions.md). Unknowns, risks, and decisions that
   remain the user's.

## One-paragraph thesis

Jev is a calibrated, wide judgment snapshot over one prepared world-state,
returned in about 200 ms with no model round. Shoal's deterministic state
machines already reach every point where the next step depends on a semantic
distinction; today the only mechanism at those points is to wake a model that
rereads everything. Giving the state machine the distinction itself, and
waking the model only when the distinction is genuinely open, changes the
shape of the system: typed requests answer themselves when their fields are
judgment-shaped, investigations run before anyone wakes, review depth adapts to
the change, and the frontier model becomes a branch-misprediction handler
rather than the loop that reads tool output.

## The baseline and the arithmetic

The compelling baseline: write a helper, let Jev carry its routine semantic
branches, fall back into the current conversation when the tree runs out,
resume with everything still in scope. Run-ahead, request interception, and
wake suppression are later applications; the authored cell is independently
useful without them.

The arithmetic is what makes this worth building. A helper does not need to
be autonomous. If it handles ninety to ninety-five percent of its
invocations reliably and hands the rest back with a precise question and
retained evidence, the model rounds spent on the handled majority disappear
and the handed-back minority arrives better prepared than today's first
round. Coverage with a safe fallback is the product; complete resolution is
not required.

## The System 1 framing

None of this must run unattended. The cell is System 1: ordinary Haskell with
semantic choices embedded, executed as far as it usefully goes. The current
model turn is System 2, already present, with the cell's intermediate values
still bound in the machine session. Every decision tree carries an "I do not
know" continuation that returns control with retained state, and returning
is a normal outcome, not a failure. A recurring return reason is the signal to
extend the resident helper so the next invocation gets further. See the
opening of [shoal-leverage.md](shoal-leverage.md) and the handback pattern
in [microprogram-patterns.md](microprogram-patterns.md).

## Review incorporated

Astra reviewed the set on 2026-09-16 and three corrections are folded in:
implementation starts with an authored investigation cell and reuses it for
run-ahead, rather than starting with request interception; prefill
eligibility is an explicit per-field annotation, never derived from a field's
type; and evaluation measures against independently checked outcomes with
untaken branches actually fetched, because matching the recorded agent could
reproduce the waste. Astra's extension, keeping competing explanations alive
through execution and gathering one discriminating observation per hypothesis
before judging again, is now the core of the investigation cell and the
central experiment.

## The package

`~/dev/jev-dsl` holds the implementation extracted on 2026-09-16 from the
prototype that used to live under `jev-integration/haskell/proto/`. Decisions
taken during extraction, beyond the reconciliation: positions take the
author's JSON value directly with outer shapes checked at `prepare` (no
`Content`/`Nullable` wrappers); every builder is total; an `Exact` root schema
carries verbatim question ids; provider rejections decode to a parsed
`Rejection`; and the core is polymorphic over a `JsonValue` class with an
aeson facade so user code never names a JSON type.

Later the same day the surface was rebuilt as two labelled fronts over one
core, then cut to what a program wants to express rather than every request
the provider accepts. `Jev.Operators`, for agent use and review, is
implemented: anonymous packets (`#k := q :& Nil`), alternatives and rubric
levels written once with `alt`/`level` and inferred, elimination by handler
lists checked positionally at compile time, pools named at their binding
and stamped into the questions that draw on them. About forty names are
typed in programs; the rest are types for optional signatures. Shapes the
surface leaves out on purpose (runtime rubrics, verbatim ids, raw
questions, omitted-versus-null criteria) are rendered by a replay module in
the package's test tree, so every capture still round-trips; the package's
`docs/authoring.md` names them. The declared-record front for human use is
designed in `docs/records-dsl.md` and not implemented. Records were
rejected as the agent form because a stateful session writes a new packet
every turn, and per-packet declarations shadow selectors and pollute scope.
The code sketches in this directory predate that change and use the record
vocabulary; their patterns carry over unchanged. Tidepool's next step is
copying `Jev.Core` into `haskell/lib` with an instance for
`Tidepool.Aeson.Value`; data families, poly-kinded chains, and `TypeError`
under the extractor are unverified.

## Vocabulary

Uses the [glossary](../../docs/GLOSSARY.md): **model round** for one provider
exchange, **machine session** for the resident JIT state, **journal** for
append-only durable records. "Packet" below means one Jev request: one
prepared state plus a coherent map of questions. "Semantic boundary" means the
point at which new evidence or new candidates make a fresh packet worthwhile.
