# Coordination after wave19: own the continuation

## Observed flow and intended outcome

The root wanted component experts to discover seams, wait for a compiling
baseline, then implement and review in parallel. It admitted whole-feature
requests before the necessary shared decisions existed. Workers reported
questions and ended model rounds while keeping their obligations open. Runtime
reminders repeatedly asked them to finish those requests. All four initial
requests settled `Blocked`; three leads were replaced, one retained lead received
a new request. Evidence: `docs/reports/wave19-audit/admission-and-scaffold.md`.

The useful flow is: bounded discovery when needed, an executable shared contract,
implementation on retained workers, questions routed to their actual decision
owner, accepted updates delivered to the same pending execution request, and
incorporation checked before downstream acceptance. Independent work continues
while a narrower prerequisite is unresolved.

## Ownership explains the current friction

`Project.Routing` observes progress and settlements. It does not own the workers'
requests and therefore cannot issue `updateRequest` on the root's behalf.
`Project.BaselineIncorporation` collects one accepted baseline episode; the
requesting caller performs updates and observes their delivery. It cannot cover
the preceding question/decision flow or unforeseen successive episodes.

This is an authority boundary, not missing reminder intelligence. Passing an
opaque Response into another actor does not transfer request ownership. A
coordinator that can advance a request must originate it.

## Revised implementation scope

The accepted scope is now a recursive typed `WorkPlan a`, with typed parallel
joins, sequencing, component ownership, review, integration and verification.
Sol Medium leads the wave; bounded Luna trees own parallel components. An
authored coordinator owns execution requests and routes routine events. It
reuses Routing, ReviewFlow, Merge and Checks rather than adding competing
request or evidence registries. Ordinary clarification travels by message;
`updateRequest` is reserved for an explicit change to an execution obligation.

Delegable context checkpoints let a coordinator admit later children from an
exact hosted prefix. Checkpoint publication, source coherence, issuer retirement,
budget attenuation and explicit lease release are runtime obligations. Prove
the small composed flow before running the recursive whole-wave experiment.

The original one-worker design below is retained as a rehearsal and explanation
of request authority, not a restriction to a one-worker launch.

## Prerequisite-correction rehearsal

1. Finish a finite discovery request when discovery is necessary. Retain the
   worker and its useful context. Skip discovery when the contract is known.
2. A persistent authored coordinator starts the execution request on that worker
   with `requestWithProgressInto`. Its callback retains Response and Progress
   before submission. Reuse `Project.Routing` for question and result history.
3. For a changed execution obligation, a designated owner supplies an accepted decision against the full current
   Question and exact baseline. Validate sender and question/source identity;
   a matching question label alone is insufficient.
4. The coordinator issues the update through its own request authority. Refusal,
   uncertain delivery and an already-settled request remain visible. They do not
   authorize a blind retry, a new request or a replacement worker.
5. The worker reports incorporation through a typed capability while the original
   execution request stays pending. Natural progress/report events can trigger
   observation of update presentation. Do not add a polling actor or require
   presentation before the worker can reach the boundary that presents it.
6. Keep authorization, delivery, reported incorporation, independently executed
   checks, review and integrated source as distinct evidence. A report is not a
   passing gate. The responsible model owns unresolved engineering decisions.

Start with one worker and one prerequisite episode as an executable experiment.
This is a complete bounded flow, not a generic workflow schema. Share existing
baseline validation/report policy rather than copying it into another registry.
Retain original handles and failure evidence through repairs and cleanup.

## Alternatives and why

- Root-owned requests plus bulk baseline updates remain useful for simple work.
  They reduce calls but retain the root as a required relay; this is the fallback.
- Parsing worker prose or consulting Jev on every idle turn cannot establish
  prerequisite ownership. Known routing and source identity are deterministic.
  Bounded Jev judgments can help resolve an unmapped question, with uncertainty
  returned to its owner; they cannot grant authority.
- A native waiting registry would duplicate part of authored coordination before
  the required workflow is established. Revisit a primitive only if the compiled
  actor exposes a concrete missing capability.
- Request authority transfer is unnecessary for the first slice: originate the
  execution request in its intended owner. Do not add a runtime transfer API to
  preserve the old admission order.

## Immediate containment and checks

The host reminder now fires at most once per current request in a delivery pump,
instead of once per idle period, and explicitly permits legitimate waiting. It
does not infer completion, invent a `Blocked` variant, or settle work. This bounds
the observed loop while the authored continuation experiment is developed.
It is not durable cross-restart deduplication or a proof of dependency liveness.

The helper checks must exercise real request ownership, wrong-owner and stale
question refusals, update delivery, incorporation while the execution request
remains open, terminal/refused handling and explicit cleanup. Prompt examples must
execute. Observe next-wave root relay count, retained-worker reuse, unnecessary
settlements, wrong updates and setup cost; do not infer savings from compilation.

## Adjacent structural fixes

- Shared-type migrations need an owner for mechanical caller updates and a
  compile check of the resulting combined baseline.
- Historical product checkout selection must not silently assemble incompatible
  run-tooling modules. Resolve coherent source ownership before adding pin guards.
- Test-tool availability belongs in preparation, before workers promise checks.
- Browser recovery and application wiring need checks through actual product
  consumers; independent component correctness is insufficient.

Wave19 ended in storage exhaustion. Its retained evidence and recovery findings
are in `docs/reports/wave19-audit/`. These changes are evaluated in wave20 after
focused checks, source pinning and launch gates.
